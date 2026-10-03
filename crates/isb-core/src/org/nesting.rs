//! The Docker exception (docs/concepts/security.md#the-docker-exception):
//! an org setting, `allow_nesting`, that lets the org's workspace and
//! nothing else run with `security.nesting`, so Docker works inside it.
//!
//! Two layers keep it to the workspace:
//!
//! - **The project.** An org's restricted project blocks nesting and
//!   system-call interception (incus' defaults). With the setting on it
//!   allows both (`restricted.containers.nesting=allow`,
//!   `restricted.containers.interception=allow`), and records the setting
//!   as `user.isb.allow-nesting`.
//! - **isb itself.** Once the project allows them, any instance in it could
//!   ask for the keys, so [`check_config`] refuses them in every org project
//!   for every instance that is not a workspace built by the daemon
//!   ([`crate::spec::SandboxSpec::workspace_nesting`], which no spec,
//!   compose file or tool argument can set). Sandboxes, stack replicas,
//!   apps and builds never get them, whoever asks, superadmins included.

use super::*;

/// The project key that records the setting.
pub const KEY_ALLOW_NESTING: &str = "user.isb.allow-nesting";

/// What the workspace gets when its org allows nesting: what unprivileged
/// Docker needs in an incus container. `mknod` and `setxattr` interception
/// let the daemon make device nodes and set the overlay filesystem's
/// extended attributes from inside a user namespace.
pub const WORKSPACE_KEYS: [(&str, &str); 3] = [
    ("security.nesting", "true"),
    ("security.syscalls.intercept.mknod", "true"),
    ("security.syscalls.intercept.setxattr", "true"),
];

/// A config key only a nesting workspace may carry in an org.
pub fn is_nesting_key(k: &str) -> bool {
    k == "security.nesting" || k.starts_with("security.syscalls.intercept.")
}

/// Whether a project's config allows nesting for its workspace.
pub fn allowed(cfg: &Value) -> bool {
    cfg[KEY_ALLOW_NESTING].as_str() == Some("true")
}

/// The project keys for the setting.
fn project_keys(on: bool) -> [(&'static str, &'static str); 3] {
    let (r, flag) = if on { ("allow", "true") } else { ("block", "") };
    [
        ("restricted.containers.nesting", r),
        ("restricted.containers.interception", r),
        (KEY_ALLOW_NESTING, flag),
    ]
}

/// Refuse nesting keys in an org project's instance config, unless the
/// instance is a workspace the daemon builds. Projects outside isb's orgs
/// (incus' own `default`, the legacy default org) are the host's: there
/// `isb up` may ask for nesting as it always could.
pub fn check_config(
    name: &str,
    org: Option<&OrgId>,
    config: &BTreeMap<String, String>,
    workspace: bool,
) -> Result<()> {
    let Some(org) = org.filter(|o| !o.is_legacy_default()) else {
        return Ok(());
    };
    if workspace {
        return Ok(());
    }
    match config.keys().find(|k| is_nesting_key(k)) {
        Some(k) => Err(Error::invalid(format!(
            "{name}: {k} is refused in org {org}: only the org's workspace may nest, and only when a superadmin allows it (docs/concepts/security.md#the-docker-exception)"
        ))),
        None => Ok(()),
    }
}

/// Turn the setting on or off on the org's project. Turning it off fails
/// (incus refuses) while an instance in the org still has nesting; the
/// daemon takes it off the workspace first.
pub fn set(base: &Client, org: &OrgId, on: bool) -> Result<()> {
    resolve_default(base);
    if org.is_legacy_default() {
        return Err(Error::invalid(
            "the default org on this host is incus' default project; it has no settings",
        ));
    }
    let h = host(base);
    let pp = format!("/1.0/projects/{}", encode_segment(&org.incus_project()));
    let p = h
        .get_opt(&pp)?
        .ok_or_else(|| Error::NotFound(format!("org {org}")))?;
    let mut cfg = p["config"].clone();
    for (k, v) in project_keys(on) {
        cfg[k] = json!(v);
    }
    h.mutate(
        "PUT",
        &pp,
        Some(&json!({"description": p["description"], "config": cfg})),
        &format!(
            "{} nesting in org {org}",
            if on { "allow" } else { "block" }
        ),
        h.get_timeouts().other,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(keys: &[&str]) -> BTreeMap<String, String> {
        keys.iter()
            .map(|k| (k.to_string(), "true".into()))
            .collect()
    }

    #[test]
    fn only_a_workspace_may_nest_in_an_org() {
        let acme = OrgId::new("acme").unwrap();
        for k in [
            "security.nesting",
            "security.syscalls.intercept.mknod",
            "security.syscalls.intercept.setxattr",
            "security.syscalls.intercept.mount",
        ] {
            let e = check_config("web-1", Some(&acme), &cfg(&[k]), false).unwrap_err();
            assert!(e.to_string().contains("only the org's workspace"), "{e}");
            assert!(check_config("workspace", Some(&acme), &cfg(&[k]), true).is_ok());
        }
        assert!(check_config("web-1", Some(&acme), &cfg(&["boot.autostart"]), false).is_ok());
        // Outside isb's orgs the host decides.
        assert!(check_config("dev", None, &cfg(&["security.nesting"]), false).is_ok());
    }

    #[test]
    fn a_spec_asking_for_nesting_in_an_org_is_refused_unless_it_is_the_workspace() {
        let ids = "root:1000000:1000000000\n".to_string();
        let host = crate::plan::HostFacts {
            subids: crate::idmap::SubIds {
                subuid: ids.clone(),
                subgid: ids,
                caller_owned: false,
            },
            pools: vec!["default".into()],
            path_map: None,
            initial_copy: false,
            shared_root: None,
            org: Some(OrgId::new("acme").unwrap()),
            registry: None,
        };
        let resolve = |s: &crate::spec::SandboxSpec| {
            crate::plan::resolve(s, &Default::default(), &host, Path::new("/"))
        };
        // A sandbox, a replica, an app: whatever asks, in an org.
        let mut spec = crate::spec::SandboxSpec::new("web-1", "dev-base")
            .raw_config("security.nesting", "true");
        let e = resolve(&spec).unwrap_err();
        assert!(e.to_string().contains("refused in org acme"), "{e}");
        let intercept = crate::spec::SandboxSpec::new("web-2", "dev-base")
            .raw_config("security.syscalls.intercept.mknod", "true");
        assert!(resolve(&intercept).is_err());
        // The daemon's workspace, and only it, may.
        spec.workspace_nesting = true;
        let d = resolve(&spec).unwrap();
        assert_eq!(d.config["security.nesting"], "true");
        // No spec can claim to be that workspace.
        let y = "image: dev-base\nworkspace_nesting: true\n";
        assert!(serde_yaml_ng::from_str::<crate::spec::SandboxSpec>(y).is_err());
        let j = json!({"image": "dev-base", "workspace_nesting": true});
        assert!(serde_json::from_value::<crate::spec::SandboxSpec>(j).is_err());
    }

    #[test]
    fn the_setting_opens_and_closes_the_project() {
        assert_eq!(
            project_keys(true),
            [
                ("restricted.containers.nesting", "allow"),
                ("restricted.containers.interception", "allow"),
                (KEY_ALLOW_NESTING, "true"),
            ]
        );
        assert_eq!(project_keys(false)[0].1, "block");
        assert_eq!(project_keys(false)[1].1, "block");
        assert!(allowed(&json!({KEY_ALLOW_NESTING: "true"})));
        assert!(!allowed(&json!({KEY_ALLOW_NESTING: ""})));
        assert!(!allowed(&json!({})));
        assert!(WORKSPACE_KEYS.iter().all(|(k, _)| is_nesting_key(k)));
    }
}
