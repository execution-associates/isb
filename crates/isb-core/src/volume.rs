//! Named custom storage volumes.

use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};

use crate::client::{Client, encode_segment};
use crate::error::{Error, Result};
use crate::plan::Props;

/// On a volume a plain sandbox created: that sandbox, as
/// `<incus project>/<name>`. Once the sandbox is gone and nothing has the
/// volume attached, [`sweep`] deletes it. A stack's, an app's and a `keep`
/// volume never carry it.
pub const KEY_SANDBOX: &str = "user.isb.sandbox";
/// When [`sweep`] first found the volume unused (epoch seconds).
pub const KEY_UNUSED_SINCE: &str = "user.isb.unused_since";

/// A custom volume as incus reports it.
#[derive(Debug, Clone, Serialize)]
pub struct VolumeInfo {
    pub name: String,
    pub pool: String,
    pub content_type: String,
    pub config: Props,
    /// Instances (and profiles) using it, as incus URLs.
    pub used_by: Vec<String>,
    /// RFC 3339, as incus gives it; empty when it does not.
    pub created_at: String,
}

impl VolumeInfo {
    fn from_api(pool: &str, v: &Value) -> VolumeInfo {
        VolumeInfo {
            name: v.get("name").and_then(Value::as_str).unwrap_or("").into(),
            pool: pool.into(),
            content_type: v
                .get("content_type")
                .and_then(Value::as_str)
                .unwrap_or("")
                .into(),
            config: v
                .get("config")
                .and_then(Value::as_object)
                .map(|m| {
                    m.iter()
                        .map(|(k, v)| {
                            (
                                k.clone(),
                                v.as_str()
                                    .map(String::from)
                                    .unwrap_or_else(|| v.to_string()),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
            used_by: v
                .get("used_by")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            created_at: v
                .get("created_at")
                .and_then(Value::as_str)
                .unwrap_or("")
                .into(),
        }
    }

    /// The sandbox that created it ([`KEY_SANDBOX`]): (incus project, name).
    pub fn sandbox(&self) -> Option<(&str, &str)> {
        self.config.get(KEY_SANDBOX)?.split_once('/')
    }

    /// [`KEY_UNUSED_SINCE`], when set and a number.
    pub fn unused_since(&self) -> Option<u64> {
        self.config.get(KEY_UNUSED_SINCE)?.parse().ok()
    }
}

fn vol_path(pool: &str, name: &str) -> String {
    format!(
        "/1.0/storage-pools/{}/volumes/custom/{}",
        encode_segment(pool),
        encode_segment(name)
    )
}

/// All custom volumes in `pool`.
pub fn list(client: &Client, pool: &str) -> Result<Vec<VolumeInfo>> {
    let v = client.get(&format!(
        "/1.0/storage-pools/{}/volumes/custom?recursion=1",
        encode_segment(pool)
    ))?;
    let mut out: Vec<VolumeInfo> = v
        .as_array()
        .map(|a| a.iter().map(|x| VolumeInfo::from_api(pool, x)).collect())
        .unwrap_or_default();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

pub fn get(client: &Client, pool: &str, name: &str) -> Result<Option<VolumeInfo>> {
    Ok(client
        .get_opt(&vol_path(pool, name))?
        .map(|v| VolumeInfo::from_api(pool, &v)))
}

/// Create if missing. Returns true if this call created it.
pub fn ensure(client: &Client, pool: &str, name: &str, config: &Props) -> Result<bool> {
    if get(client, pool, name)?.is_some() {
        return Ok(false);
    }
    // Under an org's disk limit incus counts every custom volume's size and
    // refuses one without: a volume whose definition sets none gets the
    // default, as a root disk does there.
    let mut config = config.clone();
    if !config.contains_key("size") && crate::org::disk::disk_limited(client) {
        config.insert(
            "size".into(),
            crate::org::limits::DEFAULT_ROOT_SIZE.to_string(),
        );
    }
    let body =
        json!({"name": name, "type": "custom", "content_type": "filesystem", "config": config});
    match client.mutate(
        "POST",
        &format!("/1.0/storage-pools/{}/volumes/custom", encode_segment(pool)),
        Some(&body),
        &format!("create volume {name}"),
        client.timeouts.other,
    ) {
        Ok(_) => Ok(true),
        // Lost a race with another creator: fine, as long as it exists now.
        Err(_) if get(client, pool, name)?.is_some() => Ok(false),
        Err(e) => Err(e),
    }
}

/// Delete a volume. Refuses while an instance uses it.
pub fn remove(client: &Client, pool: &str, name: &str) -> Result<()> {
    let Some(v) = get(client, pool, name)? else {
        return Err(Error::NotFound(format!("volume {name} in pool {pool}")));
    };
    if !v.used_by.is_empty() {
        return Err(Error::invalid(format!(
            "volume {name} is in use by {}",
            v.used_by.join(", ")
        )));
    }
    client
        .mutate(
            "DELETE",
            &vol_path(pool, name),
            None,
            &format!("delete volume {name}"),
            client.timeouts.other,
        )
        .map(|_| ())
}

/// What [`sweep`] makes of one volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Not a sandbox's volume ([`KEY_SANDBOX`] absent): never touched.
    NotOwned,
    /// Attached somewhere, or its sandbox still exists.
    InUse,
    /// Unused, seen so for the first time: the clock starts now.
    Unused,
    /// Unused, with this long left before it is deleted.
    Waiting(Duration),
    /// Unused for the whole of `ttl`.
    Due,
}

/// `sandbox_alive`: whether the sandbox that created `v` still exists.
pub fn verdict(v: &VolumeInfo, sandbox_alive: bool, now: u64, ttl: Duration) -> Verdict {
    if v.sandbox().is_none() {
        return Verdict::NotOwned;
    }
    if !v.used_by.is_empty() || sandbox_alive {
        return Verdict::InUse;
    }
    match v.unused_since() {
        None => Verdict::Unused,
        Some(t) => {
            let quiet = Duration::from_secs(now.saturating_sub(t));
            match ttl.checked_sub(quiet) {
                Some(left) if !left.is_zero() => Verdict::Waiting(left),
                _ => Verdict::Due,
            }
        }
    }
}

fn sandbox_alive(client: &Client, v: &VolumeInfo) -> Result<bool> {
    let Some((project, name)) = v.sandbox() else {
        return Ok(false);
    };
    Ok(client
        .clone()
        .project(project)
        .get_opt(&format!("/1.0/instances/{}", encode_segment(name)))?
        .is_some())
}

/// Set (`Some`) or remove (`None`) one config key, leaving the rest as it is.
fn set_key(client: &Client, v: &VolumeInfo, key: &str, value: Option<&str>) -> Result<()> {
    let path = vol_path(&v.pool, &v.name);
    let Some(cur) = client.get_opt(&path)? else {
        return Ok(());
    };
    let mut config = cur.get("config").cloned().unwrap_or_else(|| json!({}));
    if let Some(m) = config.as_object_mut() {
        match value {
            Some(x) => m.insert(key.into(), json!(x)),
            None => m.remove(key),
        };
    }
    let body = json!({
        "config": config,
        "description": cur.get("description").cloned().unwrap_or_else(|| json!("")),
    });
    client
        .mutate(
            "PUT",
            &path,
            Some(&body),
            &format!("update volume {}", v.name),
            client.timeouts.other,
        )
        .map(|_| ())
}

/// A pool's storage driver (`dir`, `zfs`, `btrfs`, ...).
pub fn pool_driver(client: &Client, pool: &str) -> Result<Option<String>> {
    Ok(client
        .get_opt(&format!("/1.0/storage-pools/{}", encode_segment(pool)))?
        .and_then(|p| p.get("driver").and_then(Value::as_str).map(String::from)))
}

/// Whether `name` matches `pattern`, where `*` stands for any run of
/// characters (none included).
pub fn name_matches(pattern: &str, name: &str) -> bool {
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or("");
    let Some(mut rest) = name.strip_prefix(first) else {
        return false;
    };
    let mut parts: Vec<&str> = parts.collect();
    let Some(last) = parts.pop() else {
        return rest.is_empty();
    };
    for p in parts {
        match rest.find(p) {
            Some(i) => rest = &rest[i + p.len()..],
            None => return false,
        }
    }
    rest.ends_with(last)
}

/// The names of the storage pools, in server order.
pub fn pools(client: &Client) -> Result<Vec<String>> {
    Ok(client
        .get("/1.0/storage-pools")?
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .filter_map(|u| u.rsplit('/').next())
                .map(|s| s.split('?').next().unwrap_or(s).to_string())
                .collect()
        })
        .unwrap_or_default())
}

/// Volumes nothing has attached: those a sandbox that is gone created
/// ([`KEY_SANDBOX`]), or with `unattached` every one, whoever made it. The
/// second kind includes data on purpose left unattached (a stack scaled to
/// zero), so it is for a person to read, never to delete wholesale.
pub fn orphans(client: &Client, pool: &str, unattached: bool) -> Result<Vec<VolumeInfo>> {
    let mut out = Vec::new();
    for v in list(client, pool)? {
        if !v.used_by.is_empty() {
            continue;
        }
        if unattached || (v.sandbox().is_some() && !sandbox_alive(client, &v)?) {
            out.push(v);
        }
    }
    Ok(out)
}

/// Delete the volumes in `pool` that plain sandboxes created and left
/// behind, once each has been unused for `ttl`: the first pass that finds
/// one unused notes the time on it ([`KEY_UNUSED_SINCE`]), and a volume in
/// use again loses the note. Returns the names deleted. One volume failing
/// is reported and does not stop the rest.
pub fn sweep(
    client: &Client,
    pool: &str,
    now: u64,
    ttl: Duration,
    report: &mut dyn FnMut(&str),
) -> Result<Vec<String>> {
    let mut deleted = Vec::new();
    for v in list(client, pool)? {
        if v.sandbox().is_none() {
            continue;
        }
        let step = sandbox_alive(client, &v).and_then(|alive| {
            match verdict(&v, alive, now, ttl) {
                Verdict::InUse if v.unused_since().is_some() => {
                    set_key(client, &v, KEY_UNUSED_SINCE, None)
                }
                Verdict::Unused => set_key(client, &v, KEY_UNUSED_SINCE, Some(&now.to_string())),
                Verdict::Due => match remove(client, &v.pool, &v.name) {
                    Ok(()) => {
                        deleted.push(v.name.clone());
                        Ok(())
                    }
                    // Gone already, or attached since the listing: not ours to take.
                    Err(e) if e.is_not_found() || matches!(e, Error::Invalid(_)) => Ok(()),
                    Err(e) => Err(e),
                },
                _ => Ok(()),
            }
        });
        if let Err(e) = step {
            report(&format!("volume {}: {e}", v.name));
        }
    }
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::plan::{Action, DiffOptions, HostFacts, SandboxPlan, VolumeDefs, diff, resolve};
    use crate::spec::{NamedVolumeSpec, SandboxSpec, Volume};

    #[test]
    fn a_plain_sandbox_marks_the_volumes_it_creates_as_its_own() {
        let spec = SandboxSpec::new("web-dev", "dev-base")
            .volume("/m", Volume::named("web-dev-modules"))
            .volume("/c", Volume::named("shared-cache"));
        let host = HostFacts {
            pools: vec!["default".into()],
            project: "default".into(),
            ..Default::default()
        };
        let mut defs = VolumeDefs::new();
        defs.insert(
            "shared-cache".into(),
            NamedVolumeSpec {
                keep: true,
                ..Default::default()
            },
        );
        let d = resolve(&spec, &defs, &host, Path::new("/")).unwrap();
        let missing: Vec<(String, String)> = d
            .volumes
            .iter()
            .map(|v| (v.pool.clone(), v.name.clone()))
            .collect();
        let plan = diff(&d, None, &missing, DiffOptions::default()).unwrap();
        let owners = |p: &SandboxPlan| -> Vec<(String, Option<String>)> {
            p.actions
                .iter()
                .filter_map(|a| match a {
                    Action::CreateVolume { volume, config, .. } => Some((
                        volume.clone(),
                        config.get(crate::volume::KEY_SANDBOX).cloned(),
                    )),
                    _ => None,
                })
                .collect()
        };
        let c = crate::Client::new().project("isb-acme");
        // A stack, an app, a job: nothing is marked, so nothing is ever swept.
        let unowned = crate::sandbox::own_volumes(&c, &d, plan.clone(), false);
        assert!(owners(&unowned).iter().all(|(_, o)| o.is_none()));
        // A plain sandbox: its own volume, but never one the file says to keep.
        let owned = crate::sandbox::own_volumes(&c, &d, plan, true);
        assert_eq!(
            owners(&owned),
            [
                (
                    "web-dev-modules".to_string(),
                    Some("isb-acme/web-dev".to_string())
                ),
                ("shared-cache".to_string(), None),
            ]
        );
        // A volume that exists already is not in the plan, so never re-marked.
        let again = diff(&d, None, &[], DiffOptions::default()).unwrap();
        assert!(owners(&crate::sandbox::own_volumes(&c, &d, again, true)).is_empty());
    }

    fn vol(owner: Option<&str>, used: bool, since: Option<u64>) -> VolumeInfo {
        let mut config = Props::new();
        if let Some(o) = owner {
            config.insert(KEY_SANDBOX.into(), o.into());
        }
        if let Some(t) = since {
            config.insert(KEY_UNUSED_SINCE.into(), t.to_string());
        }
        VolumeInfo {
            name: "v".into(),
            pool: "default".into(),
            content_type: "filesystem".into(),
            config,
            used_by: if used {
                vec!["/1.0/instances/a".into()]
            } else {
                vec![]
            },
            created_at: String::new(),
        }
    }

    #[test]
    fn only_a_gone_sandboxs_unused_volume_is_ever_due() {
        let day = Duration::from_secs(86_400);
        let now = 1_000_000;
        // A stack's, an app's or a hand-made volume carries no owner.
        assert_eq!(
            verdict(&vol(None, false, Some(0)), false, now, day),
            Verdict::NotOwned
        );
        // Attached, or its sandbox is still there (stopped, say).
        assert_eq!(
            verdict(&vol(Some("default/a"), true, Some(0)), false, now, day),
            Verdict::InUse
        );
        assert_eq!(
            verdict(&vol(Some("default/a"), false, Some(0)), true, now, day),
            Verdict::InUse
        );
        // The clock starts at the first sighting, not at creation.
        assert_eq!(
            verdict(&vol(Some("default/a"), false, None), false, now, day),
            Verdict::Unused
        );
        assert_eq!(
            verdict(
                &vol(Some("default/a"), false, Some(now - 3600)),
                false,
                now,
                day
            ),
            Verdict::Waiting(Duration::from_secs(86_400 - 3600))
        );
        assert_eq!(
            verdict(
                &vol(Some("default/a"), false, Some(now - 86_400)),
                false,
                now,
                day
            ),
            Verdict::Due
        );
        // A clock set forward and back again never makes a volume due early.
        assert_eq!(
            verdict(
                &vol(Some("default/a"), false, Some(now + 50)),
                false,
                now,
                day
            ),
            Verdict::Waiting(day)
        );
        assert_eq!(
            verdict(
                &vol(Some("default/a"), false, Some(now)),
                false,
                now,
                Duration::ZERO
            ),
            Verdict::Due
        );
    }

    #[test]
    fn names_match_with_stars() {
        for (p, n, want) in [
            ("web-dev-modules", "web-dev-modules", true),
            ("web-dev-modules", "web-dev-modules2", false),
            ("*-dev-modules", "web-dev-modules", true),
            ("*-dev-modules", "web-dev-mise", false),
            ("x-*", "x-cache-ct", true),
            ("x-*", "ax-cache", false),
            ("*", "anything", true),
            ("a*b*c", "a1b2c", true),
            ("a*b*c", "a1c2b", false),
            ("a*a", "a", false),
        ] {
            assert_eq!(name_matches(p, n), want, "{p} ~ {n}");
        }
    }

    #[test]
    fn the_owner_is_a_project_and_a_name() {
        assert_eq!(
            vol(Some("isb-exa/web-dev"), false, None).sandbox(),
            Some(("isb-exa", "web-dev"))
        );
        assert_eq!(vol(Some("nonsense"), false, None).sandbox(), None);
    }
}
