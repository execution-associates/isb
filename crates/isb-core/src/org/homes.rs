//! Host folders an org's workspaces keep their homes in. A restricted
//! project binds only the disk paths it lists, so each home's path is
//! recorded on the project (`user.isb.workspace-homes`) besides being
//! listed, and [`super::ensure`] lists it again whatever bind roots it is
//! given: rewriting an org's settings never takes a workspace's home away.

use std::path::Path;

use super::*;

/// The project key that records host-folder home paths (comma-separated).
pub(super) const KEY_WORKSPACE_HOMES: &str = "user.isb.workspace-homes";

fn split(v: &Value) -> Vec<String> {
    v.as_str()
        .unwrap_or("")
        .split(',')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

/// The home paths recorded on a project's config.
pub(super) fn recorded(cfg: &Value) -> Vec<String> {
    split(&cfg[KEY_WORKSPACE_HOMES])
}

/// The project's disk paths: the bind roots, then each home no root
/// already covers.
pub(super) fn disk_paths(roots: &[String], homes: &[String]) -> Vec<String> {
    let mut out = roots.to_vec();
    for h in homes {
        if !out.iter().any(|r| Path::new(h).starts_with(r)) {
            out.push(h.clone());
        }
    }
    out
}

/// The config that lets the project bind `path` and records it as a home,
/// or `None` when it already does both.
fn with_home(cfg: &Value, path: &Path) -> Option<Value> {
    let p = path.display().to_string();
    let mut homes = recorded(cfg);
    let mut paths = split(&cfg["restricted.devices.disk.paths"]);
    let covered = cfg["restricted.devices.disk"].as_str() == Some("allow")
        && paths.iter().any(|x| path.starts_with(x));
    let known = homes.contains(&p);
    if covered && known {
        return None;
    }
    if !known {
        homes.push(p.clone());
    }
    if !covered {
        paths.push(p);
    }
    let mut merged = cfg.clone();
    merged["restricted.devices.disk"] = json!("allow");
    merged["restricted.devices.disk.paths"] = json!(paths.join(","));
    merged[KEY_WORKSPACE_HOMES] = json!(homes.join(","));
    Some(merged)
}

/// Let the org's restricted project bind `path` (and what is under it) as
/// a workspace home, and record it so that the org's later updates keep
/// it.
pub fn allow_home(base: &Client, org: &OrgId, path: &Path) -> Result<()> {
    let h = host(base);
    let pp = format!("/1.0/projects/{}", encode_segment(&org.incus_project()));
    let p = h
        .get_opt(&pp)?
        .ok_or_else(|| Error::NotFound(format!("org {org}")))?;
    if p["config"]["restricted"].as_str() != Some("true") {
        return Ok(());
    }
    let Some(merged) = with_home(&p["config"], path) else {
        return Ok(());
    };
    h.mutate(
        "PUT",
        &pp,
        Some(&json!({"description": p["description"], "config": merged})),
        &format!("let org {org} bind {}", path.display()),
        h.get_timeouts().other,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn disk_paths_keep_every_home_whatever_the_roots() {
        let homes = s(&["/srv/ws/lab"]);
        assert_eq!(disk_paths(&[], &homes), s(&["/srv/ws/lab"]));
        assert_eq!(
            disk_paths(&s(&["/data"]), &homes),
            s(&["/data", "/srv/ws/lab"])
        );
        // A root that covers the home lists it once.
        assert_eq!(disk_paths(&s(&["/srv/ws"]), &homes), s(&["/srv/ws"]));
        assert!(disk_paths(&[], &[]).is_empty());
    }

    #[test]
    fn allowing_a_home_records_it_and_lists_it_once() {
        let cfg = json!({"restricted": "true", "restricted.devices.disk": "managed"});
        let m = with_home(&cfg, Path::new("/srv/ws/lab")).unwrap();
        assert_eq!(m["restricted.devices.disk"], "allow");
        assert_eq!(m["restricted.devices.disk.paths"], "/srv/ws/lab");
        assert_eq!(recorded(&m), s(&["/srv/ws/lab"]));
        assert_eq!(with_home(&m, Path::new("/srv/ws/lab")), None);

        // Covered by a bind root: recorded all the same, not listed twice.
        let cfg =
            json!({"restricted.devices.disk": "allow", "restricted.devices.disk.paths": "/srv"});
        let m = with_home(&cfg, Path::new("/srv/ws/lab")).unwrap();
        assert_eq!(m["restricted.devices.disk.paths"], "/srv");
        assert_eq!(recorded(&m), s(&["/srv/ws/lab"]));
    }
}
