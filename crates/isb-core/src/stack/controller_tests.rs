//! Tests of the controller and its workers.

use super::*;

fn def(y: &str) -> StackDef {
    StackDef {
        source: None,
        domains: Default::default(),
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
        last_failed_attempt: None,
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

#[test]
fn a_scope_change_rewrites_the_service_name_and_nothing_else_does() {
    let ctl = quiet_controller();
    let d = org_def("wiki", "lab");
    let mut w = failing_worker(&ctl, &d);
    w.rt.insert(
        "wiki-web-1".into(),
        InstRt {
            ip: Some("10.0.0.2".parse().unwrap()),
            in_rotation: true,
            ..Default::default()
        },
    );
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("wiki.web");
    let read = || std::fs::read_to_string(&file).unwrap();
    // No scope answerer: today's names.
    w.sync_dns_in(dir.path());
    assert_eq!(read(), "10.0.0.2 web.wiki.lab.isb web.wiki\n");
    // Nothing changed: not written again.
    std::fs::remove_file(&file).unwrap();
    w.sync_dns_in(dir.path());
    assert!(!file.exists());
    // The stack joins wiki/production: the names follow on the next pass.
    let scope = Arc::new(Mutex::new(None::<DnsScope>));
    let s2 = scope.clone();
    ctl.set_dns_scope(Arc::new(move |org: &crate::org::OrgId, stack: &str| {
        assert_eq!((org.as_str(), stack), ("lab", "wiki"));
        s2.lock().unwrap().clone()
    }));
    *scope.lock().unwrap() = Some(DnsScope {
        name: "wiki-production".into(),
        alias: ["web".to_string()].into(),
    });
    w.sync_dns_in(dir.path());
    assert_eq!(
        read(),
        "10.0.0.2 web.wiki.lab.isb web.wiki web.wiki-production.lab.isb web.wiki-production\n"
    );
    std::fs::remove_file(&file).unwrap();
    w.sync_dns_in(dir.path());
    assert!(!file.exists(), "the same scope writes nothing");
    // A scope without this service (it lost a collision): back to today's.
    *scope.lock().unwrap() = Some(DnsScope {
        name: "wiki-production".into(),
        alias: BTreeSet::new(),
    });
    w.sync_dns_in(dir.path());
    assert_eq!(read(), "10.0.0.2 web.wiki.lab.isb web.wiki\n");
    // republish_dns wakes the org's workers (none here): no panic, no lock held.
    ctl.republish_dns(&crate::org::OrgId::new("lab").unwrap());
}

fn health_probe(start_period: Option<&str>) -> HealthProbe {
    crate::spec::Healthcheck {
        test: vec!["CMD".into(), "true".into()],
        interval: Some("5s".into()),
        start_period: start_period.map(Into::into),
        ..Default::default()
    }
    .probe()
    .unwrap()
    .unwrap()
}

/// Failures `at` these offsets (seconds) from the (re)start.
fn fail_at(rt: &mut InstRt, p: &HealthProbe, t0: Instant, at: &[u64]) {
    for s in at {
        rt.record_probe(false, p, t0 + Duration::from_secs(*s));
    }
}

#[test]
fn the_startup_grace_defaults_to_twice_the_failure_budget_within_bounds() {
    let p = health_probe(None);
    assert_eq!(p.start_period, Duration::ZERO);
    // 5s x 3 x 2 = 30s, raised to 60s.
    assert_eq!(p.startup_grace, Duration::from_secs(60));
    let g = |i: u64, r: u32| HealthProbe::default_grace(Duration::from_secs(i), r);
    assert_eq!(g(30, 3), Duration::from_secs(180));
    assert_eq!(g(120, 5), Duration::from_secs(300));
    // A start_period, even 0s, is the grace as written.
    assert_eq!(
        health_probe(Some("10s")).startup_grace,
        Duration::from_secs(10)
    );
    assert_eq!(health_probe(Some("0s")).startup_grace, Duration::ZERO);
}

#[test]
fn a_replica_that_never_passed_is_starting_not_restarted_within_its_grace() {
    let p = health_probe(None);
    let t0 = Instant::now();
    let mut rt = InstRt::default();
    rt.started(t0);
    // Stalwart blocking ~30s on OIDC discovery: interval * retries (15s)
    // would have restarted it; within the 60s grace it stays starting.
    fail_at(&mut rt, &p, t0, &[0, 5, 10, 15, 20, 25, 30, 45, 55]);
    assert_eq!(rt.healthy, None, "starting: out of rotation, not restarted");
    assert_eq!(rt.failures, 0);
    rt.record_probe(true, &p, t0 + Duration::from_secs(58));
    assert_eq!(rt.healthy, Some(true));
}

#[test]
fn a_replica_that_never_passed_is_restarted_after_its_grace() {
    let p = health_probe(None);
    let t0 = Instant::now();
    let mut rt = InstRt::default();
    rt.started(t0);
    fail_at(&mut rt, &p, t0, &[50, 60, 65]);
    assert_eq!(rt.healthy, None, "two counted failures of three");
    fail_at(&mut rt, &p, t0, &[70]);
    assert_eq!(rt.healthy, Some(false), "unhealthy: its app is restarted");
    // The restart starts a new grace.
    let t1 = t0 + Duration::from_secs(75);
    rt.started(t1);
    fail_at(&mut rt, &p, t1, &[5, 10, 15, 20]);
    assert_eq!(rt.healthy, None);
}

#[test]
fn a_replica_that_passed_then_fails_is_restarted_as_before() {
    let p = health_probe(None);
    let t0 = Instant::now();
    let mut rt = InstRt::default();
    rt.started(t0);
    rt.record_probe(true, &p, t0 + Duration::from_secs(5));
    // Well within the startup grace, but it has passed: failures count.
    fail_at(&mut rt, &p, t0, &[10, 15]);
    assert_eq!(rt.healthy, Some(true), "still serving below retries");
    fail_at(&mut rt, &p, t0, &[20]);
    assert_eq!(rt.healthy, Some(false));
    // With a start_period, a pass inside it does not end it.
    let p = health_probe(Some("30s"));
    let mut rt = InstRt::default();
    rt.started(t0);
    rt.record_probe(true, &p, t0 + Duration::from_secs(5));
    fail_at(&mut rt, &p, t0, &[10, 15, 20]);
    assert_eq!(rt.healthy, Some(true));
    fail_at(&mut rt, &p, t0, &[30, 35, 40]);
    assert_eq!(rt.healthy, Some(false));
}

#[test]
fn a_cursor_from_before_a_restart_starts_over() {
    let ctl = quiet_controller();
    ctl.note("info", "web", "one".into());
    ctl.note("info", "web", "two".into());
    let (head, _) = ctl.events(0, 10);
    assert_eq!(head, 2);
    assert_eq!(ctl.resume_from(head), head);
    assert_eq!(ctl.resume_from(1), 1);
    // A browser saw seq 500 from the last process; this one is at 2.
    assert_eq!(ctl.resume_from(500), 0);
    let (_, evs) = ctl.events(ctl.resume_from(500), 10);
    assert_eq!(evs.len(), 2);
}
