//! Nesting (docs/concepts/security.md#nesting): no instance in an org runs
//! with `security.nesting` or system-call interception, whoever asks,
//! superadmins included. An org's restricted project blocks both (incus'
//! defaults, which [`super::ensure`] puts back), and [`check_config`]
//! refuses the keys before incus is asked, with a reason. Docker inside an
//! org runs in a VM (`type: vm`).

use super::*;

/// A config key that nests or intercepts system calls.
pub fn is_nesting_key(k: &str) -> bool {
    k == "security.nesting" || k.starts_with("security.syscalls.intercept.")
}

/// Refuse nesting keys in an org project's instance config. Projects
/// outside isb's orgs (incus' own `default`) are the host's: there `isb up`
/// may ask for nesting as it always could.
pub fn check_config(
    name: &str,
    org: Option<&OrgId>,
    config: &BTreeMap<String, String>,
) -> Result<()> {
    let Some(org) = org else {
        return Ok(());
    };
    match config.keys().find(|k| is_nesting_key(k)) {
        Some(k) => Err(Error::invalid(format!(
            "{name}: {k} is refused in org {org}: nothing in an org nests; run Docker in a VM (type: vm) (docs/concepts/security.md#nesting)"
        ))),
        None => Ok(()),
    }
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
    fn nothing_in_an_org_nests() {
        let acme = OrgId::new("acme").unwrap();
        for k in [
            "security.nesting",
            "security.syscalls.intercept.mknod",
            "security.syscalls.intercept.setxattr",
            "security.syscalls.intercept.mount",
        ] {
            let e = check_config("web-1", Some(&acme), &cfg(&[k])).unwrap_err();
            assert!(e.to_string().contains("type: vm"), "{e}");
        }
        assert!(check_config("web-1", Some(&acme), &cfg(&["boot.autostart"])).is_ok());
        // Outside isb's orgs the host decides.
        assert!(check_config("dev", None, &cfg(&["security.nesting"])).is_ok());
    }

    #[test]
    fn a_spec_asking_for_nesting_in_an_org_is_refused() {
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
            initial_owner: false,
            incus_version: Some("7.5.1".into()),
            invoking_ids: (1000, 1000),
            shared_root: None,
            org: Some(OrgId::new("acme").unwrap()),
            registry: None,
            project: "isb-acme".into(),
        };
        let resolve = |s: &crate::spec::SandboxSpec| {
            crate::plan::resolve(s, &Default::default(), &host, Path::new("/"))
        };
        // A sandbox, a replica, an app: whatever asks, in an org.
        let spec = crate::spec::SandboxSpec::new("web-1", "dev-base")
            .raw_config("security.nesting", "true");
        let e = resolve(&spec).unwrap_err();
        assert!(e.to_string().contains("refused in org acme"), "{e}");
        let intercept = crate::spec::SandboxSpec::new("web-2", "dev-base")
            .raw_config("security.syscalls.intercept.mknod", "true");
        assert!(resolve(&intercept).is_err());
        // Outside isb's orgs the host decides.
        let mut outside = host.clone();
        outside.org = None;
        outside.project = "default".into();
        let d = crate::plan::resolve(&spec, &Default::default(), &outside, Path::new("/")).unwrap();
        assert_eq!(d.config["security.nesting"], "true");
    }
}
