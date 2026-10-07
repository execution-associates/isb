//! The Docker exception in the daemon (docs/concepts/security.md#the-docker-exception):
//! `org_nesting`, for superadmins only, and what an org's workspace gets
//! while its org allows nesting.
//!
//! - **On**: the org's project allows nesting and interception, and each of
//!   the org's workspaces gets [`WORKSPACE_KEYS`] at once (incus takes them
//!   live; they apply from the workspace's next start, so the answer says
//!   when a restart is due). A workspace built or rebuilt while it is on
//!   gets them from the start.
//! - **Off**: refused while a workspace of the org runs with nesting, since
//!   taking it away under running containers would leave them half there:
//!   stop the workspace first. Then the keys come off the workspaces and
//!   the project blocks nesting again.

use super::*;
use crate::org::nesting::{self as nest, WORKSPACE_KEYS};

/// The badge's sentence, shown wherever the setting is.
pub(super) const WARNING: &str =
    "Nesting allowed: this workspace can run Docker; more of the host kernel is exposed.";

/// Whether the org allows its workspace to nest.
pub(super) fn org_allows(client: &Client, org: &OrgId) -> bool {
    crate::org::get(client, org).is_ok_and(|o| o.allow_nesting)
}

/// The keys on a workspace's spec, for an org that allows nesting.
pub(super) fn apply(spec: &mut crate::spec::SandboxSpec) {
    for (k, v) in WORKSPACE_KEYS {
        spec.raw_config.insert(k.into(), v.into());
    }
    spec.workspace_nesting = true;
}

/// Whether an instance's config has nesting on.
pub(super) fn nests(cfg: &BTreeMap<String, String>) -> bool {
    cfg.get("security.nesting").is_some_and(|v| v == "true")
}

/// The keys put on (or taken off) a workspace's instance: whether it changed.
fn set_instance(oc: &Client, instance: &str, on: bool) -> Result<bool> {
    let path = format!("/1.0/instances/{}", encode_segment(instance));
    let Some(mut v) = oc.get_opt(&path)? else {
        return Ok(false);
    };
    let mut changed = false;
    if let Some(cfg) = v["config"].as_object_mut() {
        for (k, val) in WORKSPACE_KEYS {
            if on && cfg.get(k).and_then(Value::as_str) != Some(val) {
                cfg.insert(k.into(), json!(val));
                changed = true;
            } else if !on && cfg.remove(k).is_some() {
                changed = true;
            }
        }
    }
    if changed {
        let body = json!({
            "architecture": v["architecture"], "config": v["config"], "devices": v["devices"],
            "ephemeral": v["ephemeral"], "profiles": v["profiles"], "stateful": v["stateful"],
            "description": v["description"],
        });
        let what = format!(
            "{} nesting on {instance}",
            if on { "turn on" } else { "turn off" }
        );
        oc.mutate("PUT", &path, Some(&body), &what, oc.get_timeouts().other)?;
    }
    Ok(changed)
}

/// What `org_nesting` turning the setting off would interrupt: the org's
/// workspaces that run with nesting.
fn running_with_nesting(d: &Daemon, org: &OrgId) -> Vec<String> {
    let oc = d.workspaces.oc(org);
    d.workspaces
        .store
        .list(org)
        .unwrap_or_default()
        .into_iter()
        .filter(|w| {
            Sandbox::get(&oc, w.instance())
                .and_then(|s| s.info())
                .is_ok_and(|i| i.status.eq_ignore_ascii_case("running") && nests(&i.config))
        })
        .map(|w| w.name)
        .collect()
}

fn org_nesting(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        org: String,
        #[serde(default)]
        allow_nesting: Option<bool>,
    }
    let a: A = args(a)?;
    let org = OrgId::new(a.org)?;
    // The tool is a superadmin one (`superadmin::TOOLS`); checked here too.
    if !c.is_trusted() {
        return Err(Error::Forbidden("org_nesting is for superadmins".into()));
    }
    crate::org::check_exists(&d.client, &org)?;
    let wsm = &d.workspaces;
    let oc = wsm.oc(&org);
    let mut out = Vec::new();
    if let Some(on) = a.allow_nesting {
        let _g = wsm.lock.lock().unwrap_or_else(|e| e.into_inner());
        if !on {
            let busy = running_with_nesting(d, &org);
            if !busy.is_empty() {
                return Err(Error::invalid(format!(
                    "workspace {} in org {org} is running with nesting (its Docker containers live on it): stop it first (workspace_stop), then turn nesting off; it starts again without",
                    busy.join(", ")
                )));
            }
        } else {
            nest::set(&d.client, &org, true)?;
        }
        for w in wsm.store.list(&org)? {
            let changed = set_instance(&oc, w.instance(), on)?;
            out.push((w.name, changed));
        }
        if !on {
            nest::set(&d.client, &org, false)?;
        }
        let actor = c.superadmin_source().unwrap_or_else(|| c.to_string());
        wsm.record(
            &org,
            if on {
                "org.nesting.allowed"
            } else {
                "org.nesting.blocked"
            },
            org.as_str(),
            &actor,
            format!(
                "org {org}: nesting {} for its workspace by {actor}",
                if on { "allowed" } else { "blocked" }
            ),
            json!({"allow_nesting": on}),
        );
    }
    let allowed = org_allows(&d.client, &org);
    let workspaces: Vec<Value> = wsm
        .store
        .list(&org)?
        .iter()
        .map(|w| {
            let info = Sandbox::get(&oc, w.instance()).and_then(|s| s.info()).ok();
            let running = info
                .as_ref()
                .is_some_and(|i| i.status.eq_ignore_ascii_case("running"));
            let changed = out.iter().any(|(n, ch)| n == &w.name && *ch);
            json!({
                "name": w.name,
                "status": info.as_ref().map(|i| i.status.clone()),
                "nesting": info.as_ref().is_some_and(|i| nests(&i.config)),
                "restart_needed": running && changed && allowed,
            })
        })
        .collect();
    Ok(json!({
        "org": org.as_str(),
        "allow_nesting": allowed,
        "warning": allowed.then_some(WARNING),
        "workspaces": workspaces,
    }))
}

pub(super) fn register(r: &mut Registry, d: Arc<Daemon>) -> Result<()> {
    r.register(
        Tool::new(
            "org_nesting",
            "Superadmins only: whether the org's workspace (and nothing else in the org) may run with security.nesting, so Docker works inside it. allow_nesting true or false changes it; leaving it out reads it. Turning it on applies at the workspace's next start (restart_needed says so); turning it off is refused while the workspace runs with nesting: stop it first. Sandboxes, apps and builds never get nesting. It exposes more of the host kernel to the workspace (docs/concepts/security.md#the-docker-exception).",
            json!({
                "type": "object",
                "properties": {
                    "org": {"type": "string", "description": "The org."},
                    "allow_nesting": {"type": "boolean", "description": "true allows it, false blocks it; leave it out to read."}
                },
                "required": ["org"],
                "additionalProperties": false
            }),
            move |a, c| org_nesting(&d, a, c),
        )
        .title("Allow Docker in an org's workspace")
        .annotations(json!({"destructiveHint": false, "openWorldHint": false})),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_superadmins_may_set_nesting() {
        use crate::daemon::tests::{token, user};
        let set = json!({"org": "acme", "allow_nesting": true});
        let cls = crate::daemon::audit::Class::default();
        let auth = |c: &Caller, anon: bool| {
            crate::daemon::authorize_class(c, "org_nesting", cls, set.clone(), None, anon)
        };
        // Platform admins, the org's owner, an owner's admin-scoped token.
        for c in [
            user(&[], true),
            user(&[("acme", Role::Owner)], false),
            token(&[("acme", Role::Owner)], &["admin"]),
        ] {
            let e = auth(&c, false).unwrap_err();
            assert!(e.to_string().contains("for superadmins"), "{e}");
        }
        let anon = Caller::Unauthenticated {
            addr: "127.0.0.1:1".parse().unwrap(),
        };
        assert!(auth(&anon, true).is_err());
        // The org-bound endpoint of the org itself changes nothing.
        let owner = user(&[("acme", Role::Owner)], false);
        let acme = OrgId::new("acme").unwrap();
        let cls2 = crate::daemon::audit::Class::default();
        assert!(
            crate::daemon::authorize_class(
                &owner,
                "org_nesting",
                cls2,
                set.clone(),
                Some(&acme),
                false
            )
            .is_err()
        );
        let sa = Caller::Superadmin(Arc::new(crate::auth::Superadmin::synthetic(
            crate::auth::SuperadminSource::Token {
                id: 1,
                name: "ops".into(),
            },
        )));
        assert!(auth(&sa, false).is_ok());
        assert!(auth(&Caller::Local { uid: None }, false).is_ok());
    }

    #[test]
    fn a_workspace_spec_gets_the_keys_and_the_daemons_mark() {
        let mut s = crate::spec::SandboxSpec::new("workspace", "dev-base");
        assert!(!s.workspace_nesting);
        apply(&mut s);
        assert!(s.workspace_nesting);
        assert_eq!(s.raw_config["security.nesting"], "true");
        assert_eq!(s.raw_config["security.syscalls.intercept.mknod"], "true");
        assert_eq!(s.raw_config["security.syscalls.intercept.setxattr"], "true");
        let cfg = BTreeMap::from([("security.nesting".to_string(), "true".to_string())]);
        assert!(nests(&cfg));
        assert!(!nests(&BTreeMap::new()));
    }
}
