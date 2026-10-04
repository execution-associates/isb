//! Tests of the controller and its workers.

use super::*;

fn def(y: &str) -> StackDef {
    StackDef {
        name: "app".into(),
        org: crate::org::OrgId::default_org(),
        file: serde_yaml_ng::from_str(y).unwrap(),
        base_dir: "/".into(),
        secrets: BTreeMap::new(),
        force: BTreeMap::new(),
        images: BTreeMap::new(),
        deployed_at: 0,
        deployed_by: String::new(),
        previous: None,
    }
}

fn quiet_controller() -> Controller {
    let dir = tempfile::tempdir().unwrap();
    let k = crate::secrets::Keyring::new(age::x25519::Identity::generate(), vec![]);
    let secrets = Arc::new(Secrets::new(crate::secrets::LocalDriver::new(
        dir.path(),
        Arc::new(k),
    )));
    let store = Store::open(dir.path()).unwrap();
    std::mem::forget(dir);
    Controller::start(
        Client::with_socket("/nonexistent/isb-test/incus.sock"),
        store,
        Duration::from_secs(3600),
        secrets,
    )
    .unwrap()
}

fn org_def(name: &str, org: &str) -> Arc<StackDef> {
    let mut d = def("services:\n  web:\n    image: x\n");
    d.name = name.into();
    d.org = crate::org::OrgId::new(org).unwrap();
    Arc::new(d)
}

fn shared(d: &Arc<StackDef>) -> Arc<WorkerShared> {
    Arc::new(WorkerShared {
        slot: Mutex::new(Slot {
            def: d.clone(),
            remove: false,
            remove_volumes: false,
        }),
        wake: Condvar::new(),
        stop: AtomicBool::new(false),
        kick: AtomicBool::new(false),
    })
}

fn status(state: &str, message: Option<&str>) -> ServiceStatus {
    ServiceStatus {
        service: "web".into(),
        image: "x".into(),
        rev: "r".into(),
        replicas: 1,
        running: 0,
        healthy: 0,
        state: state.into(),
        message: message.map(Into::into),
        instances: vec![],
        ports: vec![],
        rollout: None,
        checked_at: 0,
        domains: vec![],
    }
}

const QUOTA: &str = "slot 1: org lab is at its CPU quota (limits.cpu 2, 2 in use): stop or delete something in it, or ask for less; retrying in 40s";

fn failing_worker(ctl: &Controller, d: &Arc<StackDef>) -> Worker {
    let mut w = Worker::new(
        ctl.inner.clone(),
        d.name.clone(),
        d.qualified(),
        Client::with_socket("/nonexistent/isb-test/incus.sock"),
        "web".into(),
        shared(d),
        d.org.clone(),
    );
    w.seen = Some(d.clone());
    w.create_backoff = Some((Instant::now(), Duration::from_secs(40)));
    w.state = "failing".into();
    w.message = Some(QUOTA.into());
    w.last_error = Some(QUOTA.into());
    w
}

#[test]
fn a_new_deployment_drops_the_retry_backoff() {
    let ctl = quiet_controller();
    let d = org_def("app", "lab");
    let mut w = failing_worker(&ctl, &d);
    // The same instructions keep the backoff and the failure.
    w.begin_pass(&d);
    assert!(w.create_backoff.is_some());
    assert_eq!(w.state, "failing");
    // A deployment hands over a new definition: a fresh attempt, from
    // a clean state, whose errors are reported again.
    let d2 = org_def("app", "lab");
    w.begin_pass(&d2);
    assert!(w.create_backoff.is_none());
    assert_eq!(w.state, "updating");
    assert!(w.message.is_none());
    assert!(w.last_error.is_none());
}

#[test]
fn a_deployment_resets_the_failure_status_it_would_otherwise_report() {
    let ctl = quiet_controller();
    let d = org_def("app", "lab");
    let key = (d.qualified(), "web".to_string());
    ctl.inner
        .workers
        .lock()
        .unwrap()
        .insert(key.clone(), shared(&d));
    let mut st = ctl.inner.status.lock().unwrap();
    st.insert(key.clone(), status("failing", Some(QUOTA)));
    drop(st);
    ctl.apply(org_def("app", "lab"));
    let st = ctl.inner.status.lock().unwrap();
    let s = &st[&key];
    assert_eq!(s.state, "updating");
    assert!(s.message.is_none(), "{:?}", s.message);
}

#[test]
fn a_limits_change_wakes_the_services_failing_on_a_limit() {
    let ctl = quiet_controller();
    let lab = org_def("app", "lab");
    let other = org_def("app", "other");
    let lab2 = org_def("api", "lab");
    let mut shares = Vec::new();
    for (d, state, msg) in [
        (&lab, "failing", QUOTA),
        (&other, "failing", QUOTA),
        (&lab2, "failing", "slot 1: image not found"),
    ] {
        let key = (d.qualified(), "web".to_string());
        ctl.inner
            .stacks
            .lock()
            .unwrap()
            .insert(d.qualified(), d.clone());
        let sh = shared(d);
        ctl.inner
            .workers
            .lock()
            .unwrap()
            .insert(key.clone(), sh.clone());
        ctl.inner
            .status
            .lock()
            .unwrap()
            .insert(key, status(state, Some(msg)));
        shares.push(sh);
    }
    let n = ctl.org_limits_changed(&crate::org::OrgId::new("lab").unwrap());
    assert_eq!(n, 1);
    let kicked: Vec<bool> = shares
        .iter()
        .map(|s| s.kick.load(Ordering::SeqCst))
        .collect();
    // lab's quota failure only: not another org, not another failure.
    assert_eq!(kicked, [true, false, false]);
    // The woken worker's next pass retries at once.
    let mut w = failing_worker(&ctl, &lab);
    w.shared = shares[0].clone();
    w.begin_pass(&lab);
    assert!(w.create_backoff.is_none());
    assert!(!w.shared.kick.load(Ordering::SeqCst));
}

#[test]
fn limit_errors_are_recognised() {
    assert!(limit_error(QUOTA));
    assert!(limit_error(
        "incus project foo is at its CPU limit (limits.cpu 2): raise it"
    ));
    assert!(!limit_error("slot 1: image not found"));
}

#[test]
fn published_ports() {
    let s: SandboxSpec = serde_yaml_ng::from_str(
        "image: x\nports: ['8080:80', '0.0.0.0:9000:9000', {listen: 'tcp:127.0.0.1:5000', connect: 'tcp:127.0.0.1:5000', bind: guest}]\n",
    )
    .unwrap();
    let p = published(&s).unwrap();
    assert_eq!(
        p,
        vec![
            Published {
                listen: "127.0.0.1:8080".parse().unwrap(),
                target: 80,
                udp: false
            },
            Published {
                listen: "0.0.0.0:9000".parse().unwrap(),
                target: 9000,
                udp: false
            },
        ]
    );
    let bad: SandboxSpec =
        serde_yaml_ng::from_str("image: x\nports: ['8000-8001:8000-8001']\n").unwrap();
    assert!(published(&bad).is_err());
    // UDP needs a host address of its own (ports.rs has the rest).
    let udp: SandboxSpec = serde_yaml_ng::from_str("image: x\nports: ['53:53/udp']\n").unwrap();
    assert!(published(&udp).is_err());
}

#[test]
fn instance_specs_are_labelled_and_unpublished() {
    let d =
        def("services:\n  web: {image: x, ports: ['8080:80'], deploy: {labels: {tier: front}}}\n");
    let s = instance_spec(&d, "web", d.service("web").unwrap(), 2, "abcd").unwrap();
    assert!(s.ports.is_empty());
    assert_eq!(s.labels["isb.stack"], "app");
    assert_eq!(s.labels["isb.slot"], "2");
    assert_eq!(s.labels["isb.rev"], "abcd");
    assert_eq!(s.labels["tier"], "front");
    assert_eq!(s.restart, Some(RestartMode::Always));
}

#[test]
fn deploy_diff() {
    let a = def("services:\n  web: {image: x}\n  db: {image: y}\n");
    let b = def("services:\n  web: {image: x, deploy: {replicas: 3}}\n  api: {image: z}\n");
    let c: BTreeMap<String, String> = diff(Some(&a), &b)
        .unwrap()
        .into_iter()
        .map(|c| (c.service, c.change))
        .collect();
    assert_eq!(c["web"], "scale");
    assert_eq!(c["api"], "create");
    assert_eq!(c["db"], "remove");
    let c = diff(
        Some(&a),
        &def("services:\n  web: {image: x2}\n  db: {image: y}\n"),
    )
    .unwrap();
    assert_eq!(
        c.iter().find(|c| c.service == "web").unwrap().change,
        "update"
    );
    assert_eq!(
        c.iter().find(|c| c.service == "db").unwrap().change,
        "unchanged"
    );
}
