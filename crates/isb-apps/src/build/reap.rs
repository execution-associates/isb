//! Build instances whose maker died: each records the process that made it,
//! and a later process deletes the ones whose maker is gone.

use serde_json::{Value, json};

use crate::client::Client;
use crate::sandbox::Sandbox;

/// The config key naming the process that made a build instance
/// ([`crate::foreground::owner_tag`]), so a later process can delete one
/// whose maker died ([`reap_stale`]).
pub(super) const OWNER: &str = "user.isb.build.owner";

/// `config` with this process recorded as the instance's maker.
pub(super) fn owned(mut config: Value) -> Value {
    if let Some(t) = crate::foreground::owner_tag() {
        config[OWNER] = json!(t);
    }
    config
}

/// Delete the build instances in `client`'s project whose maker is gone.
///
/// The process that makes a build instance deletes it when the build ends,
/// however it ends. One whose process died first (a daemon restarted or
/// killed mid-build) would otherwise run on, count against the org's quota
/// and keep the build cache volume, which no later build's sandbox can then
/// mount ("Idmaps of container ... and storage volume ... are not
/// identical"). `untagged` also takes build instances from before makers
/// were recorded, which only a starting daemon can judge stale.
pub fn reap_stale(client: &Client, untagged: bool, log: &mut dyn FnMut(&str)) -> usize {
    let Ok(list) = client.get("/1.0/instances?recursion=1") else {
        return 0;
    };
    let mut n = 0;
    for i in list.as_array().into_iter().flatten() {
        let Some(name) = i["name"].as_str() else {
            continue;
        };
        let config = &i["config"];
        let build = config.get("user.isb.build").is_some()
            || config.get("user.isb.workspace-image-build").is_some()
            || name.starts_with("builder-prep-");
        let stale = match config[OWNER].as_str() {
            Some(t) => crate::foreground::owner_alive(t) == Some(false),
            None => untagged,
        };
        if !(build && stale) {
            continue;
        }
        match Sandbox::remove(client, name, true) {
            Ok(()) => {
                log(&format!(
                    "deleted {name}: its build ended with the process that ran it"
                ));
                n += 1;
            }
            Err(e) if e.is_not_found() => {}
            Err(e) => log(&format!("{name}: a stale build sandbox, not deleted: {e}")),
        }
    }
    n
}

/// [`reap_stale`] in every isb project (the orgs' and `isb-system`),
/// untagged instances included: what a starting daemon runs, before any
/// build of its own begins.
pub fn reap_stale_everywhere(base: &Client, log: &mut dyn FnMut(&str)) -> usize {
    let Ok(projects) = base.get("/1.0/projects") else {
        return 0;
    };
    projects
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p.as_str()?.rsplit('/').next())
        .filter(|p| p.starts_with("isb-"))
        .map(|p| reap_stale(&base.clone().project(p), true, log))
        .sum()
}
