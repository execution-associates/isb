use super::*;
use crate::spec::{IdmapMode, IdmapSpec, NamedVolumeSpec, PortBinding, Volume};

const TITAN: &str = "root:1000000:1000000000\nroot:1000:1\n";

fn host() -> HostFacts {
    HostFacts {
        subids: SubIds {
            subuid: TITAN.into(),
            subgid: TITAN.into(),
            caller_owned: false,
        },
        pools: vec!["container-roots".into(), "default".into()],
        path_map: None,
        initial_copy: false,
        shared_root: None,
        org: None,
        registry: None,
        project: "default".into(),
    }
}

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn lasso_spec(web: &str) -> SandboxSpec {
    SandboxSpec::new("dev-lasso-x-12345678", "dev-base")
        .cpus(8)
        .memory("8GiB")
        .privileged(false)
        .idmap(IdmapSpec::Mode(IdmapMode::Auto))
        .label("lasso.worktree", "/w")
        .label("lasso.web", web)
        .volume("/home/dev/repo/src/web", Volume::bind(web).device("web"))
        .volume(
            "/home/dev/.bun/install/cache",
            Volume::named("lasso-bun-cache")
                .device("bun-cache")
                .owner("dev"),
        )
}

/// What incus would hold after creating `d`.
fn actual_from(d: &Desired) -> Actual {
    Actual {
        status: "Running".into(),
        config: d.config.clone(),
        devices: d
            .devices
            .iter()
            .map(|(k, v)| (k.clone(), v.props.clone()))
            .collect(),
        profiles: d.profiles.clone(),
        instance_type: "container".into(),
    }
}

#[test]
fn pool_auto_prefers_incus_zfs_then_default() {
    let mut h = host();
    assert_eq!(h.pick_pool(None).unwrap(), "default");
    h.pools.push("incus-zfs".into());
    assert_eq!(h.pick_pool(Some("auto")).unwrap(), "incus-zfs");
    h.pools = vec!["only".into()];
    assert_eq!(h.pick_pool(None).unwrap(), "only");
    assert!(h.pick_pool(Some("nope")).is_err());
    h.pools.clear();
    assert!(h.pick_pool(None).is_err());
}

#[test]
fn named_volumes_seed_from_the_image_when_the_server_can() {
    let t = tmp();
    let web = t.path().to_str().unwrap();
    let spec = lasso_spec(web).volume("/srv/plain", Volume::named("plain").nocopy(true));
    let copy = |h: &HostFacts, s: &SandboxSpec| {
        let d = resolve(s, &VolumeDefs::new(), h, Path::new("/")).unwrap();
        ["bun-cache", "web", "srv-plain"].map(|k| d.devices[k].props.get("initial.copy").cloned())
    };
    // An older server: no key anywhere, the behavior isb always had.
    assert_eq!(copy(&host(), &spec), [None, None, None]);
    let mut h = host();
    h.initial_copy = true;
    // Named volumes only, and nocopy opts out.
    assert_eq!(copy(&h, &spec), [Some("true".into()), None, None]);
    // Not in a VM: incus only seeds container volumes.
    let mut vm = SandboxSpec::new("vm-x", "dev-base").volume("/c", Volume::named("c"));
    vm.instance_type = InstanceType::VirtualMachine;
    let d = resolve(&vm, &VolumeDefs::new(), &h, Path::new("/")).unwrap();
    assert!(!d.devices["c"].props.contains_key("initial.copy"));
    // nocopy on a bind mount is an error, as are pool and external.
    let bad = lasso_spec(web).volume("/x", Volume::bind(web).nocopy(true));
    assert!(resolve(&bad, &VolumeDefs::new(), &h, Path::new("/")).is_err());
}

#[test]
fn initial_copy_alone_never_replaces_a_disk() {
    let t = tmp();
    let web = t.path().to_str().unwrap();
    // A sandbox made before the server could seed volumes, then reconciled
    // after the upgrade: its volume must not be remounted.
    let old = resolve(
        &lasso_spec(web),
        &VolumeDefs::new(),
        &host(),
        Path::new("/"),
    )
    .unwrap();
    let mut h = host();
    h.initial_copy = true;
    let new = resolve(&lasso_spec(web), &VolumeDefs::new(), &h, Path::new("/")).unwrap();
    assert!(new.devices["bun-cache"].props.contains_key("initial.copy"));
    assert!(device_matches(
        &new.devices["bun-cache"],
        &actual_from(&old).devices["bun-cache"]
    ));
}

#[test]
fn resolves_lasso_shape() {
    let t = tmp();
    let web = t.path().to_str().unwrap();
    let d = resolve(
        &lasso_spec(web),
        &VolumeDefs::new(),
        &host(),
        Path::new("/"),
    )
    .unwrap();
    assert_eq!(d.pool, "default");
    assert_eq!(d.config["limits.cpu"], "8");
    assert_eq!(d.config["limits.memory"], "8GiB");
    assert_eq!(d.config["security.privileged"], "false");
    assert_eq!(d.config["raw.idmap"], "both 1000 1000");
    assert_eq!(d.config["user.lasso.worktree"], "/w");
    let web_dev = &d.devices["web"].props;
    assert_eq!(web_dev["path"], "/home/dev/repo/src/web");
    let canon = std::fs::canonicalize(web).unwrap();
    assert_eq!(web_dev["source"], canon.to_str().unwrap());
    let bun = &d.devices["bun-cache"].props;
    assert_eq!(bun["pool"], "default");
    assert_eq!(bun["source"], "lasso-bun-cache");
    assert_eq!(d.devices["root"].props["pool"], "default");
    assert_eq!(d.volumes.len(), 1);
    assert_eq!(d.owners[0].path, "/home/dev/.bun/install/cache");
}

#[test]
fn fresh_plan_creates_then_starts_then_chowns() {
    let t = tmp();
    let d = resolve(
        &lasso_spec(t.path().to_str().unwrap()),
        &VolumeDefs::new(),
        &host(),
        Path::new("/"),
    )
    .unwrap();
    let missing = vec![("default".to_string(), "lasso-bun-cache".to_string())];
    let p = diff(&d, None, &missing, DiffOptions::default()).unwrap();
    let kinds: Vec<&str> = p
        .actions
        .iter()
        .map(|a| match a {
            Action::CreateVolume { .. } => "vol",
            Action::CreateInstance { .. } => "create",
            Action::StartInstance => "start",
            Action::FixOwner { .. } => "chown",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, vec!["vol", "create", "start", "chown"]);
    // Every mount is in the create request, before first boot.
    match &p.actions[1] {
        Action::CreateInstance { devices, .. } => {
            assert!(devices.contains_key("web"));
            assert!(devices.contains_key("bun-cache"));
            assert!(devices.contains_key("root"));
        }
        _ => unreachable!(),
    }
}

#[test]
fn ensure_on_correct_instance_is_noop() {
    let t = tmp();
    let d = resolve(
        &lasso_spec(t.path().to_str().unwrap()),
        &VolumeDefs::new(),
        &host(),
        Path::new("/"),
    )
    .unwrap();
    let mut a = actual_from(&d);
    // Things incus adds and other tools add must not register as drift.
    a.config.insert("volatile.uuid".into(), "x".into());
    a.config.insert("user.someone.else".into(), "y".into());
    a.devices.insert(
        "icon".into(),
        Props::from([
            ("type".into(), "disk".into()),
            ("path".into(), "/home/dev/repo/docs/icon".into()),
            ("source".into(), "/x/docs/icon".into()),
        ]),
    );
    a.devices.insert(
        "vite".into(),
        Props::from([
            ("type".into(), "proxy".into()),
            ("bind".into(), "host".into()),
            ("listen".into(), "tcp:1.2.3.4:5174".into()),
            ("connect".into(), "tcp:127.0.0.1:5173".into()),
        ]),
    );
    let p = diff(&d, Some(&a), &[], DiffOptions::default()).unwrap();
    assert!(p.is_noop(), "{:?}", p.actions);
    assert!(p.actions.is_empty(), "{:?}", p.actions);
}

#[test]
fn readonly_false_equals_absent() {
    let t = tmp();
    let d = resolve(
        &lasso_spec(t.path().to_str().unwrap()),
        &VolumeDefs::new(),
        &host(),
        Path::new("/"),
    )
    .unwrap();
    let mut a = actual_from(&d);
    a.devices
        .get_mut("web")
        .unwrap()
        .insert("readonly".into(), "false".into());
    let src = a.devices["web"]["source"].clone();
    a.devices
        .get_mut("web")
        .unwrap()
        .insert("source".into(), format!("{src}/"));
    assert!(
        diff(&d, Some(&a), &[], DiffOptions::default())
            .unwrap()
            .is_noop()
    );
}

#[test]
fn only_the_wrong_device_is_replaced() {
    let t = tmp();
    let d = resolve(
        &lasso_spec(t.path().to_str().unwrap()),
        &VolumeDefs::new(),
        &host(),
        Path::new("/"),
    )
    .unwrap();
    let mut a = actual_from(&d);
    a.devices
        .get_mut("web")
        .unwrap()
        .insert("source".into(), "/somewhere/else".into());
    let p = diff(&d, Some(&a), &[], DiffOptions::default()).unwrap();
    assert_eq!(p.actions.len(), 1, "{:?}", p.actions);
    match &p.actions[0] {
        Action::ReplaceDevice { device, from, .. } => {
            assert_eq!(device, "web");
            assert_eq!(from["source"], "/somewhere/else");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn missing_device_is_added_and_owner_fixed() {
    let t = tmp();
    let d = resolve(
        &lasso_spec(t.path().to_str().unwrap()),
        &VolumeDefs::new(),
        &host(),
        Path::new("/"),
    )
    .unwrap();
    let mut a = actual_from(&d);
    a.devices.remove("bun-cache");
    let p = diff(&d, Some(&a), &[], DiffOptions::default()).unwrap();
    assert!(matches!(&p.actions[0], Action::AddDevice { device, .. } if device == "bun-cache"));
    assert!(matches!(&p.actions[1], Action::FixOwner { .. }));
    assert_eq!(p.actions.len(), 2);
}

#[test]
fn equivalent_device_under_other_name_is_adopted() {
    let t = tmp();
    let d = resolve(
        &lasso_spec(t.path().to_str().unwrap()),
        &VolumeDefs::new(),
        &host(),
        Path::new("/"),
    )
    .unwrap();
    let mut a = actual_from(&d);
    let web = a.devices.remove("web").unwrap();
    a.devices.insert("legacy-web".into(), web);
    let p = diff(&d, Some(&a), &[], DiffOptions::default()).unwrap();
    assert!(p.is_noop(), "{:?}", p.actions);
    // Same path, different source: replaced under our name (two disks cannot
    // share a mount point).
    a.devices
        .get_mut("legacy-web")
        .unwrap()
        .insert("source".into(), "/other".into());
    let p = diff(&d, Some(&a), &[], DiffOptions::default()).unwrap();
    assert!(matches!(&p.actions[0],
        Action::ReplaceDevice { device, replaces, .. } if device == "web" && replaces == "legacy-web"));
}

#[test]
fn config_drift_and_restart_flag() {
    let t = tmp();
    let d = resolve(
        &lasso_spec(t.path().to_str().unwrap()),
        &VolumeDefs::new(),
        &host(),
        Path::new("/"),
    )
    .unwrap();
    let mut a = actual_from(&d);
    a.config.insert("limits.cpu".into(), "4".into());
    a.config.remove("raw.idmap");
    let p = diff(&d, Some(&a), &[], DiffOptions::default()).unwrap();
    assert!(p.actions.contains(&Action::SetConfig {
        key: "limits.cpu".into(),
        from: Some("4".into()),
        to: "8".into(),
        restart: false,
        secret: false,
    }));
    assert!(p.actions.contains(&Action::SetConfig {
        key: "raw.idmap".into(),
        from: None,
        to: "both 1000 1000".into(),
        restart: true,
        secret: false,
    }));
}

#[test]
fn secret_environment_is_set_but_never_shown() {
    let t = tmp();
    let mut s = lasso_spec(t.path().to_str().unwrap());
    s.env.secrets.insert("TOKEN".into(), "tok".into());
    s.env.vars.insert("TOKEN".into(), "hunter2".into());
    let d = resolve(&s, &VolumeDefs::new(), &host(), Path::new("/")).unwrap();
    assert_eq!(d.config["environment.TOKEN"], "hunter2");
    let created = diff(&d, None, &[], DiffOptions::default()).unwrap();
    let shown = format!(
        "{:?} {}",
        created.actions,
        serde_json::to_string(&created).unwrap()
    );
    assert!(!shown.contains("hunter2"), "{shown}");
    let mut a = actual_from(&d);
    a.config
        .insert("environment.TOKEN".into(), "old-value".into());
    let p = diff(&d, Some(&a), &[], DiffOptions::default()).unwrap();
    let shown = format!(
        "{} {}",
        p.actions
            .iter()
            .map(|a| a.to_string())
            .collect::<Vec<_>>()
            .join("; "),
        serde_json::to_string(&p).unwrap()
    );
    assert!(
        !shown.contains("hunter2") && !shown.contains("old-value"),
        "{shown}"
    );
    assert!(p.actions.iter().any(|a| matches!(
        a,
        Action::SetConfig { key, secret: true, .. } if key == "environment.TOKEN"
    )));
}

#[test]
fn stopped_instance_is_started_after_changes() {
    let t = tmp();
    let d = resolve(
        &lasso_spec(t.path().to_str().unwrap()),
        &VolumeDefs::new(),
        &host(),
        Path::new("/"),
    )
    .unwrap();
    let mut a = actual_from(&d);
    a.status = "Stopped".into();
    a.config.insert("limits.memory".into(), "1GiB".into());
    let p = diff(&d, Some(&a), &[], DiffOptions::default()).unwrap();
    assert!(matches!(p.actions[0], Action::SetConfig { .. }));
    assert_eq!(p.actions[1], Action::StartInstance);
}

#[test]
fn prune_removes_unknown_devices_but_never_root() {
    let t = tmp();
    let d = resolve(
        &lasso_spec(t.path().to_str().unwrap()),
        &VolumeDefs::new(),
        &host(),
        Path::new("/"),
    )
    .unwrap();
    let mut a = actual_from(&d);
    a.devices.insert(
        "extra".into(),
        Props::from([("type".into(), "none".into())]),
    );
    let p = diff(&d, Some(&a), &[], DiffOptions::default()).unwrap();
    assert!(p.is_noop());
    let p = diff(
        &d,
        Some(&a),
        &[],
        DiffOptions {
            prune_devices: true,
        },
    )
    .unwrap();
    assert_eq!(p.actions.len(), 1);
    assert!(matches!(&p.actions[0], Action::RemoveDevice { device, .. } if device == "extra"));
}

#[test]
fn searched_port_matches_anywhere_in_range() {
    let t = tmp();
    let spec = lasso_spec(t.path().to_str().unwrap()).port(
        PortBinding::host("tcp:100.1.2.3:5173", "tcp:127.0.0.1:5173")
            .name("vite")
            .search(50),
    );
    let d = resolve(&spec, &VolumeDefs::new(), &host(), Path::new("/")).unwrap();
    let mut a = actual_from(&d);
    a.devices
        .get_mut("vite")
        .unwrap()
        .insert("listen".into(), "tcp:100.1.2.3:5190".into());
    assert!(
        diff(&d, Some(&a), &[], DiffOptions::default())
            .unwrap()
            .is_noop()
    );
    a.devices
        .get_mut("vite")
        .unwrap()
        .insert("listen".into(), "tcp:100.1.2.3:5300".into());
    let p = diff(&d, Some(&a), &[], DiffOptions::default()).unwrap();
    assert!(matches!(&p.actions[0], Action::RemoveDevice { device, .. } if device == "vite"));
    assert!(matches!(&p.actions[1], Action::AddPort { search: 50, .. }));
    // Missing: deferred until after start so the search runs against live binds.
    a.devices.remove("vite");
    let p = diff(&d, Some(&a), &[], DiffOptions::default()).unwrap();
    assert!(matches!(&p.actions[0], Action::AddPort { search: 50, .. }));
    // Fresh: not in the create request.
    let p = diff(&d, None, &[], DiffOptions::default()).unwrap();
    match &p.actions[0] {
        Action::CreateInstance { devices, .. } => assert!(!devices.contains_key("vite")),
        other => panic!("{other:?}"),
    }
    assert!(matches!(&p.actions[2], Action::AddPort { .. }));
}

#[test]
fn port_shorthand() {
    let n = |a: &str| normalize_addr(a, "127.0.0.1");
    assert_eq!(n("5173").unwrap(), "tcp:127.0.0.1:5173");
    assert_eq!(n("0.0.0.0:5173").unwrap(), "tcp:0.0.0.0:5173");
    assert_eq!(n("5353/udp").unwrap(), "udp:127.0.0.1:5353");
    assert_eq!(n("10.0.0.1:5353/udp").unwrap(), "udp:10.0.0.1:5353");
    assert_eq!(n("tcp:5173").unwrap(), "tcp:127.0.0.1:5173");
    assert_eq!(n("udp:5353").unwrap(), "udp:127.0.0.1:5353");
    assert_eq!(n("tcp:100.1.2.3:5173").unwrap(), "tcp:100.1.2.3:5173");
    assert_eq!(n("[::1]:5173").unwrap(), "tcp:[::1]:5173");
    assert_eq!(n("tcp:[::1]:5173").unwrap(), "tcp:[::1]:5173");
    assert_eq!(n("8000-8010").unwrap(), "tcp:127.0.0.1:8000-8010");
    assert_eq!(n("80,443").unwrap(), "tcp:127.0.0.1:80,443");
    assert_eq!(n("unix:/run/x.sock").unwrap(), "unix:/run/x.sock");
    assert_eq!(normalize_addr("80", "0.0.0.0").unwrap(), "tcp:0.0.0.0:80");
    for bad in [
        "",
        "abc",
        "0",
        "70000",
        "::1:80",
        "5173/sctp",
        "unix:",
        ":80",
        "tcp:",
        "[::1]80",
    ] {
        assert!(n(bad).is_err(), "{bad:?} should be rejected");
    }
}

#[test]
fn shorthand_ports_match_existing_full_form_devices() {
    let t = tmp();
    let spec = lasso_spec(t.path().to_str().unwrap())
        .port(PortBinding::host("5173", "5173").name("vite"))
        .port(PortBinding::guest("8190", "tcp:127.0.0.1:9000").name("backend"));
    let d = resolve(&spec, &VolumeDefs::new(), &host(), Path::new("/")).unwrap();
    assert_eq!(d.devices["vite"].props["listen"], "tcp:127.0.0.1:5173");
    assert_eq!(d.devices["vite"].props["connect"], "tcp:127.0.0.1:5173");
    assert_eq!(d.devices["backend"].props["listen"], "tcp:127.0.0.1:8190");
    let a = actual_from(&d);
    assert!(
        diff(&d, Some(&a), &[], DiffOptions::default())
            .unwrap()
            .is_noop()
    );
    // A VM's connect side defaults to 0.0.0.0 (incus NAT finds the VM).
    let mut vm = spec.clone();
    vm.instance_type = crate::spec::InstanceType::VirtualMachine;
    vm.privileged = None;
    vm.idmap = None;
    vm.ports.retain(|p| p.bind == PortBind::Host);
    let d = resolve(&vm, &VolumeDefs::new(), &host(), Path::new("/")).unwrap();
    assert_eq!(d.devices["vite"].props["connect"], "tcp:0.0.0.0:5173");
    // Default names use the expanded address.
    let spec = lasso_spec(t.path().to_str().unwrap()).port(PortBinding::host("5353/udp", "53/udp"));
    let d = resolve(&spec, &VolumeDefs::new(), &host(), Path::new("/")).unwrap();
    assert!(d.devices.contains_key("port-host-udp-5353"));
}

#[test]
fn both_port_directions() {
    let t = tmp();
    let spec = lasso_spec(t.path().to_str().unwrap())
        .port(PortBinding::guest(
            "tcp:127.0.0.1:8190",
            "tcp:127.0.0.1:8191",
        ))
        .port(PortBinding::host(
            "tcp:127.0.0.1:5173",
            "tcp:127.0.0.1:5173",
        ));
    let d = resolve(&spec, &VolumeDefs::new(), &host(), Path::new("/")).unwrap();
    assert_eq!(d.devices["port-guest-8190"].props["bind"], "guest");
    assert_eq!(d.devices["port-host-5173"].props["bind"], "host");
}

#[test]
fn bind_sources_stay_under_the_shared_root() {
    let t = tmp();
    let root = t.path().canonicalize().unwrap();
    let mut h = host();
    h.shared_root = Some(root.to_string_lossy().into_owned());
    let base = lasso_spec(root.to_str().unwrap());
    let r = |s: &SandboxSpec| resolve(s, &VolumeDefs::new(), &h, Path::new("/"));
    assert!(r(&base).is_ok());
    let e = r(&base.clone().volume("/x", Volume::bind("/")))
        .unwrap_err()
        .to_string();
    assert!(e.contains("shared with the isb machine"), "{e}");
}

#[test]
fn validation_errors() {
    let t = tmp();
    let base = lasso_spec(t.path().to_str().unwrap());
    let r = |s: &SandboxSpec| resolve(s, &VolumeDefs::new(), &host(), Path::new("/"));
    let mut s = base.clone();
    s.name = Some("1bad".into());
    assert!(r(&s).is_err());
    let s = base.clone().volume("rel/path", Volume::bind("/"));
    assert!(r(&s).is_err());
    let s = base
        .clone()
        .volume("/x", Volume::bind("/definitely/not/here"));
    assert!(r(&s).unwrap_err().to_string().contains("does not exist"));
    let s = base.clone().volume("/x", Volume::bind("/").owner("dev"));
    assert!(r(&s).is_err());
    let s = base.clone().volume("/x", Volume::bind("/").pool("p"));
    assert!(r(&s).is_err());
    let s = base
        .clone()
        .port(PortBinding::guest("tcp:1.2.3.4:1", "tcp:1.2.3.4:2").search(3));
    assert!(r(&s).is_err());
    let s = base
        .clone()
        .port(PortBinding::host("1.2.3.4:x", "tcp:1.2.3.4:2"));
    assert!(r(&s).is_err());
    let s = base
        .clone()
        .volume("/y", Volume::bind("/").option("source", "/etc"));
    assert!(r(&s).unwrap_err().to_string().contains("core property"));
    let s = base.clone().volume("/y", Volume::bind("/").device("web"));
    assert!(r(&s).unwrap_err().to_string().contains("used twice"));
}

#[test]
fn docker_memory_units() {
    assert_eq!(memory_limit("512m").unwrap(), "512MiB");
    assert_eq!(memory_limit("8g").unwrap(), "8GiB");
    // Docker reads GB as GiB too.
    assert_eq!(memory_limit("8GB").unwrap(), "8GiB");
    assert_eq!(memory_limit("8GiB").unwrap(), "8GiB");
    assert_eq!(memory_limit("50%").unwrap(), "50%");
    assert_eq!(memory_limit("1073741824").unwrap(), "1073741824");
    assert!(memory_limit("1.5g").is_err());
    assert!(memory_limit("8 parsecs").is_err());
}

#[test]
fn cpus_and_cpuset() {
    let t = tmp();
    let r = |s: &SandboxSpec| resolve(s, &VolumeDefs::new(), &host(), Path::new("/"));
    let base = lasso_spec(t.path().to_str().unwrap());
    assert_eq!(r(&base).unwrap().config["limits.cpu"], "8");
    let mut s = base.clone();
    s.cpus = None;
    s.cpuset = Some("0-3".into());
    assert_eq!(r(&s).unwrap().config["limits.cpu"], "0-3");
    s.cpus = Some("2".into());
    assert!(r(&s).unwrap_err().to_string().contains("not both"));
    let mut s = base.clone();
    s.cpus = Some("0-3".into());
    assert!(r(&s).unwrap_err().to_string().contains("cpuset"));
}

#[test]
fn named_volume_uses_its_declared_name() {
    let t = tmp();
    let mut defs = VolumeDefs::new();
    defs.insert(
        "lasso-bun-cache".into(),
        NamedVolumeSpec {
            name: Some("lasso-dev_lasso-bun-cache".into()),
            ..Default::default()
        },
    );
    let d = resolve(
        &lasso_spec(t.path().to_str().unwrap()),
        &defs,
        &host(),
        Path::new("/"),
    )
    .unwrap();
    assert_eq!(
        d.devices["bun-cache"].props["source"],
        "lasso-dev_lasso-bun-cache"
    );
    assert_eq!(d.volumes[0].name, "lasso-dev_lasso-bun-cache");
}

#[test]
fn a_searched_port_connects_to_the_default_address() {
    let t = tmp();
    let r = |s: &SandboxSpec| resolve(s, &VolumeDefs::new(), &host(), Path::new("/"));
    let base = lasso_spec(t.path().to_str().unwrap());
    let ok = base
        .clone()
        .port(PortBinding::host("tcp:1.2.3.4:5173", "tcp:127.0.0.1:5173").search(5));
    assert_eq!(r(&ok).unwrap().devices["port-host-5173"].search, Some(5));
    let bad = base
        .clone()
        .port(PortBinding::host("tcp:1.2.3.4:5173", "tcp:10.0.0.2:5173").search(5));
    assert!(r(&bad).unwrap_err().to_string().contains("default address"));
}

#[test]
fn vm_rules() {
    use crate::spec::InstanceType;
    let t = tmp();
    let r = |s: &SandboxSpec| resolve(s, &VolumeDefs::new(), &host(), Path::new("/"));
    let mut s = lasso_spec(t.path().to_str().unwrap());
    s.instance_type = InstanceType::VirtualMachine;
    // privileged and explicit idmap are container-only.
    assert!(r(&s).unwrap_err().to_string().contains("container-only"));
    s.privileged = None;
    let d = r(&s).unwrap();
    // idmap: auto is a no-op for a VM; the default readiness waits for the agent.
    assert!(!d.config.contains_key("raw.idmap"));
    assert_eq!(d.ready, vec![ReadyCheck::Running, ReadyCheck::Agent]);
    assert_eq!(d.ready_timeout, Duration::from_secs(300));
    let s2 = s
        .clone()
        .port(PortBinding::host("tcp:0.0.0.0:80", "tcp:10.0.0.2:80"));
    assert_eq!(r(&s2).unwrap().devices["port-host-80"].props["nat"], "true");
    let s3 = s
        .clone()
        .port(PortBinding::guest("tcp:127.0.0.1:1", "tcp:127.0.0.1:2"));
    assert!(r(&s3).is_err());
    s.idmap = Some(IdmapSpec::Mode(IdmapMode::Always));
    assert!(r(&s).is_err());
    // `vm` is accepted as shorthand in YAML.
    let f: crate::spec::ComposeFile =
        serde_yaml_ng::from_str("services:\n  a: {image: x, type: vm}\n").unwrap();
    assert_eq!(f.services["a"].instance_type, InstanceType::VirtualMachine);
}

#[test]
fn external_volume_must_exist() {
    let t = tmp();
    let mut defs = VolumeDefs::new();
    defs.insert(
        "lasso-bun-cache".into(),
        NamedVolumeSpec {
            external: true,
            ..Default::default()
        },
    );
    let d = resolve(
        &lasso_spec(t.path().to_str().unwrap()),
        &defs,
        &host(),
        Path::new("/"),
    )
    .unwrap();
    let missing = vec![("default".to_string(), "lasso-bun-cache".to_string())];
    assert!(diff(&d, None, &missing, DiffOptions::default()).is_err());
}

#[test]
fn path_translation() {
    let mut h = host();
    h.path_map = Some(("/home/u".into(), "/srv/box/home".into()));
    assert_eq!(h.translate("/home/u/src/web"), "/srv/box/home/src/web");
    assert_eq!(h.translate("/home/u"), "/srv/box/home");
    assert_eq!(h.translate("/home/user2/x"), "/home/user2/x");
    assert_eq!(h.translate("/elsewhere"), "/elsewhere");
}

#[test]
fn device_names_are_deterministic() {
    assert_eq!(
        device_name_for_path("/home/dev/.bun/install/cache"),
        "home-dev-bun-install-cache"
    );
    let long = "/a/very/long/path/that/goes/on/and/on/and/on/forever/and/ever/amen";
    let n = device_name_for_path(long);
    assert!(n.len() <= 48, "{n}");
    assert_eq!(n, device_name_for_path(long));
    assert_ne!(n, device_name_for_path(&format!("{long}2")));
}

#[test]
fn image_sources() {
    let i = ImageSource::parse("images:debian/12").unwrap();
    assert_eq!(
        i.server.as_deref(),
        Some("https://images.linuxcontainers.org")
    );
    assert_eq!(i.alias, "debian/12");
    let i = ImageSource::parse("dev-base").unwrap();
    assert!(i.server.is_none());
    assert_eq!(i.to_api(Some("abc"))["fingerprint"], "abc");
    assert!(ImageSource::parse("nope:x").is_err());
    assert!(ImageSource::parse("").is_err());
}
