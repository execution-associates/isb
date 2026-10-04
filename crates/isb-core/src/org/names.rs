//! Service names for orgs that exist already: `isb-default` is made by the
//! daemon, which may start before `isb host setup` has made the host's
//! directory for names. Every start (and the daemon's watch afterwards)
//! turns names on for the orgs that lack them, so the order of the two
//! never matters.

use super::ensure::{PrepareDir, ensure_service_names_in};
use super::*;

/// Make sure the default org exists: create `isb-default` with default
/// settings when it is missing. An existing one keeps its settings, except
/// that its service names are turned on when the host can have them now,
/// whatever order `isb serve install` and `isb host setup` ran in. Returns
/// whether it created the org. Idempotent.
pub fn ensure_default(base: &Client, report: &mut dyn FnMut(&str)) -> Result<bool> {
    ensure_default_in(base, &crate::discovery::prepare_org, report)
}

fn ensure_default_in(
    base: &Client,
    prepare: PrepareDir,
    report: &mut dyn FnMut(&str),
) -> Result<bool> {
    let org = OrgId::default_org();
    match host(base).get_opt(&format!("/1.0/projects/{DEFAULT_ORG_PROJECT}"))? {
        Some(p) if p["config"][KEY_ORG].as_str() == Some(DEFAULT_ORG) => {
            ensure_service_names_in(base, &org, prepare, report)?;
            Ok(false)
        }
        Some(_) => Err(Error::AlreadyExists(format!(
            "incus project {DEFAULT_ORG_PROJECT} exists but is not isb's default org"
        ))),
        None => ensure(base, &org, &OrgOptions::default(), report).map(|_| true),
    }
}

/// Turn service names on for every org whose bridge lacks them and whose
/// host can have them now. Returns how many orgs are still without names
/// (the host has no directory for them yet). Orgs that cannot be read are
/// reported and left as they are.
pub fn ensure_all_service_names(base: &Client, report: &mut dyn FnMut(&str)) -> Result<usize> {
    let v = host(base).get("/1.0/projects?recursion=1")?;
    let mut off = 0;
    for p in v.as_array().into_iter().flatten() {
        let Some(org) = p["name"].as_str().and_then(OrgId::from_incus_project) else {
            continue;
        };
        if p["config"][KEY_ORG].as_str() != Some(org.as_str()) {
            continue;
        }
        match ensure_service_names(base, &org, report) {
            Ok(Names::Unavailable) => off += 1,
            Ok(_) => {}
            Err(e) => {
                report(&format!("{org}: service names: {e}"));
                off += 1;
            }
        }
    }
    Ok(off)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::fake::{Route, serve};

    #[test]
    fn ensure_default_turns_service_names_on_for_an_existing_default_org() {
        // The org was made by `isb serve install` before `isb host setup`:
        // its bridge has no raw.dnsmasq. The daemon starts again with the
        // directory in place and the bridge is patched.
        let bridge = bridge_name(&OrgId::default_org());
        let routes = |patch: bool| {
            let mut r = vec![
                Route {
                    prefix: "GET /1.0/projects/isb-default",
                    status: 200,
                    body: json!({"config": {KEY_ORG: "default"}}),
                },
                Route {
                    prefix: "GET /1.0/networks/",
                    status: 200,
                    body: json!({"config": {"ipv4.address": "10.1.2.1/24"}}),
                },
            ];
            if patch {
                r.push(Route {
                    prefix: "PATCH /1.0/networks/",
                    status: 200,
                    body: json!({}),
                });
            }
            r
        };
        let dir = |_: &OrgId| Ok(Some(PathBuf::from("/var/lib/isb/dns/default")));
        let (_d, c) = serve(routes(true));
        let mut lines = Vec::new();
        assert!(!ensure_default_in(&c, &dir, &mut |l| lines.push(l.to_string())).unwrap());
        assert!(
            lines
                .iter()
                .any(|l| l.contains("turning on service names") && l.contains(&bridge)),
            "{lines:?}"
        );
        // Still no directory: nothing to patch, and it says why.
        let none = |_: &OrgId| Ok(None);
        let (_d, c) = serve(routes(false));
        let mut lines = Vec::new();
        assert!(!ensure_default_in(&c, &none, &mut |l| lines.push(l.to_string())).unwrap());
        assert!(
            lines.iter().any(|l| l.contains("service names are off")),
            "{lines:?}"
        );
    }

    #[test]
    fn ensure_default_leaves_an_existing_default_org_alone() {
        let (_d, c) = serve(vec![Route {
            prefix: "GET /1.0/projects/isb-default",
            status: 200,
            body: json!({"config": {KEY_ORG: "default"}}),
        }]);
        let mut lines = Vec::new();
        let none = |_: &OrgId| Ok(None);
        assert!(!ensure_default_in(&c, &none, &mut |l| lines.push(l.to_string())).unwrap());
        // It has no bridge here, so there are no service names to turn on.
        assert!(lines.is_empty());
        // Another tool's project of that name is refused, not taken over.
        let (_d, c) = serve(vec![Route {
            prefix: "GET /1.0/projects/isb-default",
            status: 200,
            body: json!({"config": {}}),
        }]);
        let e = ensure_default_in(&c, &none, &mut |_| {}).unwrap_err();
        assert!(e.to_string().contains("not isb's default org"), "{e}");
    }

    #[test]
    fn every_org_is_visited_and_a_missing_directory_is_counted() {
        let (_d, c) = serve(vec![Route {
            prefix: "GET /1.0/projects?recursion=1",
            status: 200,
            body: json!([
                {"name": "default", "config": {}},
                {"name": "isb-default", "config": {KEY_ORG: "default"}},
                {"name": "isb-acme", "config": {KEY_ORG: "acme"}},
                {"name": "isb-other", "config": {}},
            ]),
        }]);
        // Neither org has a bridge on this fake, so both count as having
        // nothing to change; the other projects are not orgs.
        let mut lines = Vec::new();
        let off = ensure_all_service_names(&c, &mut |l| lines.push(l.to_string())).unwrap();
        assert_eq!(off, 0, "{lines:?}");
    }
}
