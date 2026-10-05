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

/// The controller's polling round over every due 1Password binding, in
/// every stack: one `op item get` per (org, vault, item), however many
/// stacks, keys and fields use it; a bump moves only the bindings into that
/// item, and the services using them act per their `on_change`.
#[test]
fn polling_asks_1password_once_per_item_across_stacks() {
    let op = crate::secrets::onepassword::fake::FakeOp::new();
    let (ctl, due) = onepassword_stacks(&op);
    assert_eq!(due.len(), 72);
    let before = op.calls().len();
    ctl.poll(due.clone());
    assert_eq!(op.calls().len() - before, 6, "{:?}", &op.calls()[before..]);

    // smtp moves: still one ask per item and org, and only smtp's bindings
    // move.
    op.set_version("ops", "smtp", 2);
    let before = op.calls().len();
    let (seq, _) = ctl.events(0, 10_000);
    ctl.poll(due.clone());
    assert_eq!(op.calls().len() - before, 6);
    for d in ctl.definitions() {
        for (k, b) in &d.secrets {
            let want = if matches!(k.as_str(), "d" | "e") {
                2
            } else {
                1
            };
            assert_eq!(b.version, want, "{} {k}", d.name);
        }
    }
    // Each service says what it does about it: web restarts for d (the
    // strongest of d's restart and e's none), api leaves e stale.
    let (_, evs) = ctl.events(seq, 10_000);
    let rotated: Vec<&Event> = evs
        .iter()
        .filter(|e| e.kind.as_deref() == Some("secret.rotated"))
        .collect();
    assert_eq!(rotated.len(), 24, "{rotated:?}");
    let find = |svc: &str| {
        rotated
            .iter()
            .find(|e| e.stack == "alpha/s0" && e.service == svc)
            .unwrap()
    };
    let web = find("web");
    assert!(
        web.message.contains("restarting its replicas in place"),
        "{}",
        web.message
    );
    assert!(
        web.message.contains("ops/smtp/password v1 -> v2"),
        "{}",
        web.message
    );
    let api = find("api");
    assert_eq!(api.level, "warn");
    assert!(api.message.contains("on_change: none"), "{}", api.message);
    // A forced refresh of one reference asks once, whatever uses it.
    op.set_version("ops", "smtp", 3);
    let before = op.calls().len();
    let alpha = crate::org::OrgId::new("alpha").unwrap();
    let (found, cycles) = ctl.refresh_secret(&alpha, "ops/smtp/password").unwrap();
    assert_eq!(op.calls().len() - before, 1);
    assert_eq!(found, [("onepassword".to_string(), 3)]);
    assert_eq!(cycles.len(), 6, "{cycles:?}");
    assert!(
        cycles
            .iter()
            .all(|c| c.action == crate::spec::OnChange::Restart)
    );
    assert_eq!(crate::stack::secrets::cycled_stacks(&cycles).len(), 6);
    ctl.shutdown();
}

/// 12 stacks in two orgs, each with 3 keys into the db item, 2 into smtp
/// and 1 into api: 72 references, 3 items per org. Returns the controller
/// and every (stack, key).
fn onepassword_stacks(
    op: &crate::secrets::onepassword::fake::FakeOp,
) -> (Controller, Vec<(String, String)>) {
    for i in ["db", "smtp", "api"] {
        op.set_version("ops", i, 1);
    }
    let dir = tempfile::tempdir().unwrap();
    let k = crate::secrets::Keyring::new(age::x25519::Identity::generate(), vec![]);
    let secrets = Arc::new(
        Secrets::new(crate::secrets::LocalDriver::new(dir.path(), Arc::new(k)))
            .with_driver(Arc::new(op.driver()))
            .unwrap(),
    );
    let ctl = Controller::start(
        Client::with_socket("/nonexistent/isb-test/incus.sock"),
        Store::open(dir.path().join("stacks")).unwrap(),
        Duration::from_secs(3600),
        secrets.clone(),
    )
    .unwrap();
    let yaml = concat!(
        "secrets:\n",
        "  a: {driver: onepassword, name: ops/db/password}\n",
        "  b: {driver: onepassword, name: ops/db/login/user}\n",
        "  c: {driver: onepassword, name: ops/db/host}\n",
        "  d: {driver: onepassword, name: ops/smtp/password, on_change: restart}\n",
        "  e: {driver: onepassword, name: ops/smtp/login/user, on_change: none}\n",
        "  f: {driver: onepassword, name: ops/api/password}\n",
        "services:\n",
        "  web: {image: x, secrets: [a, b, c, d, e]}\n",
        "  api: {image: x, environment: {K: {secret: f}, U: {secret: e}}}\n",
    );
    let mut due = Vec::new();
    for n in 0..12 {
        let org = if n % 2 == 0 { "alpha" } else { "beta" };
        let mut d = def(yaml);
        d.name = format!("s{n}");
        d.org = crate::org::OrgId::new(org).unwrap();
        d.secrets =
            crate::stack::secrets::bind(&secrets, &d.org, &d.name, &d.file, &BTreeMap::new(), true)
                .unwrap();
        for key in d.secrets.keys() {
            due.push((d.qualified(), key.clone()));
        }
        ctl.inner.store.save(&d).unwrap();
        ctl.apply(Arc::new(d));
    }
    // The controller outlives this function; so must its state.
    std::mem::forget(dir);
    (ctl, due)
}
