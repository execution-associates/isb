use super::*;
use crate::client::Client;
use crate::stack::Controller;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::AtomicU16;

/// A service over a controller with no incusd behind it.
fn service(dir: &Path) -> (Monitors, Controller) {
    let k = crate::secrets::Keyring::new(age::x25519::Identity::generate(), vec![]);
    let secrets = Arc::new(Secrets::new(crate::secrets::LocalDriver::new(
        dir,
        Arc::new(k),
    )));
    let client = Client::with_socket("/nonexistent/isb-test/incus.sock");
    let store = crate::stack::Store::open(dir).unwrap();
    let ctl = Controller::start(
        client.clone(),
        store,
        Duration::from_secs(60),
        secrets.clone(),
    )
    .unwrap();
    let apps = Apps::new(dir, client, ctl.clone(), secrets.clone());
    let m = Monitors::new(
        dir,
        apps,
        secrets,
        Arc::new(|| true),
        Some("https://isb.example.com/".into()),
    );
    (m, ctl)
}

/// An HTTP server answering with whatever status `status` holds.
fn flappy() -> (u16, Arc<AtomicU16>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let status = Arc::new(AtomicU16::new(200));
    let st = status.clone();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let mut b = [0u8; 4096];
            let _ = s.read(&mut b);
            let code = st.load(Ordering::SeqCst);
            let _ = write!(s, "HTTP/1.1 {code} X\r\nContent-Length: 2\r\n\r\nok");
        }
    });
    (port, status)
}

fn kinds(ctl: &Controller) -> Vec<(String, String)> {
    ctl.events(0, 1000)
        .1
        .into_iter()
        .filter_map(|e| Some((e.kind?, e.message)))
        .filter(|(k, _)| k.starts_with("monitor."))
        .collect()
}

#[test]
fn down_once_up_once_with_details() {
    let dir = tempfile::tempdir().unwrap();
    let (svc, ctl) = service(dir.path());
    let org = OrgId::new("acme").unwrap();
    let (port, status) = flappy();
    let mut m = Monitor::new("shop", Kind::Http);
    m.url = Some(format!("http://127.0.0.1:{port}/?token=s3cret"));
    m.interval = 30;
    svc.create(&org, m.clone()).unwrap();
    let run = |n: usize| {
        for _ in 0..n {
            let o = svc.check(&org, &m);
            svc.record(&org, &m, o).unwrap();
        }
    };
    run(2);
    assert!(kinds(&ctl).is_empty());
    status.store(503, Ordering::SeqCst);
    run(5);
    let k = kinds(&ctl);
    assert_eq!(k.len(), 1, "{k:?}");
    assert_eq!(k[0].0, "monitor.down");
    assert!(k[0].1.contains("HTTP 503 (expected 200-399)"), "{}", k[0].1);
    // The query (a token, maybe) is never in a message.
    assert!(!k[0].1.contains("s3cret"));
    let d = svc.details(&org, "monitor.down", &k[0].1).unwrap();
    assert_eq!(d["status"], 503);
    assert_eq!(d["monitor"], "shop");
    assert_eq!(d["link"], "https://isb.example.com/orgs/acme/uptime/shop");
    assert!(d["latency_ms"].is_u64());
    status.store(200, Ordering::SeqCst);
    run(4);
    let k = kinds(&ctl);
    assert_eq!(
        k.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(),
        ["monitor.down", "monitor.up"]
    );
    let d = svc.details(&org, "monitor.up", &k[1].1).unwrap();
    assert!(d["downtime_ms"].is_u64() && d["downtime"].is_string());
    // One incident, closed; eleven checks kept.
    let db = svc.db(&org).unwrap();
    let db = db.lock().unwrap();
    let inc = db.incidents(None, 10, now_ms()).unwrap();
    assert_eq!(inc.len(), 1);
    assert!(inc[0].ended.is_some());
    assert_eq!(db.recent("shop", 100).unwrap().len(), 11);
    // Events go to the org's `@monitors` stack, service = the monitor.
    let e = ctl.events(0, 1000).1;
    let e = e
        .iter()
        .find(|e| e.kind.as_deref() == Some("monitor.up"))
        .unwrap();
    assert_eq!(
        (e.stack.as_str(), e.service.as_str()),
        ("acme/@monitors", "shop")
    );
}

#[test]
fn edits_pause_and_delete() {
    let dir = tempfile::tempdir().unwrap();
    let (svc, _) = service(dir.path());
    let org = OrgId::new("acme").unwrap();
    let mut m = Monitor::new("db", Kind::Tcp);
    (m.host, m.port) = (Some("db.example.com".into()), Some(5432));
    svc.create(&org, m.clone()).unwrap();
    assert!(svc.create(&org, m.clone()).is_err(), "duplicate");
    let mut p = serde_json::Map::new();
    p.insert("interval".into(), json!(120));
    p.insert("auto".into(), json!(true));
    let n = svc.update(&org, "db", p).unwrap();
    assert_eq!(n.interval, 120);
    assert!(!n.auto, "auto is not the caller's to set");
    let mut p = serde_json::Map::new();
    p.insert("interval".into(), Value::Null);
    assert_eq!(svc.update(&org, "db", p).unwrap().interval, 60);
    let mut p = serde_json::Map::new();
    p.insert("keyword".into(), json!("x"));
    assert!(
        svc.update(&org, "db", p).is_err(),
        "a keyword on a tcp monitor"
    );
    let mut p = serde_json::Map::new();
    p.insert("name".into(), json!("db2"));
    assert!(svc.update(&org, "db", p).is_err());
    assert!(svc.set_paused(&org, "db", true).unwrap().paused);
    // A paused monitor's results are dropped.
    svc.record(
        &org,
        &m,
        Outcome {
            at: 1,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(svc.stored(&org, "db").unwrap().last.is_none());
    assert!(!svc.set_paused(&org, "db", false).unwrap().paused);
    // Secrets and apps must exist.
    let mut h = Monitor::new("h", Kind::Http);
    h.url = Some("https://a.example.com/".into());
    h.headers = vec![super::super::Header {
        name: "X-Key".into(),
        value: None,
        secret: Some("NOPE".into()),
    }];
    assert!(
        svc.create(&org, h)
            .unwrap_err()
            .to_string()
            .contains("no secret NOPE")
    );
    let mut a = Monitor::new("a", Kind::App);
    a.app = Some("ghost".into());
    assert!(svc.create(&org, a).is_err());
    svc.delete(&org, "db").unwrap();
    assert!(svc.get(&org, "db").is_err());
    assert!(svc.list(&OrgId::new("beta").unwrap()).unwrap().is_empty());
}

#[test]
fn an_apps_own_monitor_stays_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let (svc, _) = service(dir.path());
    let org = OrgId::default_org();
    // An auto monitor whose app is gone is removed by the sync.
    let mut m = Monitor::new("app-shop", Kind::App);
    m.app = Some("shop".into());
    m.auto = true;
    svc.save(&org, &[m.clone()]).unwrap();
    svc.sync_auto(&org).unwrap();
    assert!(svc.list(&org).unwrap().is_empty());
    // Deleting one excludes its app from then on.
    svc.save(&org, &[m]).unwrap();
    svc.delete(&org, "app-shop").unwrap();
    assert_eq!(svc.settings(&org).unwrap().exclude_apps, ["shop"]);
    assert!(svc.orgs().contains(&org));
}

#[test]
fn the_scheduler_queues_each_check_once() {
    let dir = tempfile::tempdir().unwrap();
    let (svc, _) = service(dir.path());
    let org = OrgId::new("acme").unwrap();
    for n in ["a", "b", "c"] {
        let mut m = Monitor::new(n, Kind::Tcp);
        (m.host, m.port) = (Some("127.0.0.1".into()), Some(9));
        svc.create(&org, m).unwrap();
    }
    svc.set_paused(&org, "c", true).unwrap();
    let (tx, rx) = std::sync::mpsc::sync_channel(QUEUE);
    // Created monitors are due within a second.
    let now = now_ms() + 2000;
    svc.tick(now, &tx);
    let got: Vec<String> = rx.try_iter().map(|j| j.monitor.name).collect();
    assert_eq!(got, ["a", "b"]);
    // Running checks are not queued again.
    svc.tick(now + 5000, &tx);
    assert_eq!(rx.try_iter().count(), 0);
    // A full queue leaves the rest for the next tick.
    let (tx1, rx1) = std::sync::mpsc::sync_channel(1);
    svc.inner
        .slots
        .lock()
        .unwrap()
        .values_mut()
        .for_each(|s| s.running = false);
    svc.tick(now + 6000, &tx1);
    assert_eq!(rx1.try_iter().count(), 1);
    // A deleted monitor's slot goes.
    svc.delete(&org, "a").unwrap();
    svc.tick(now + 7000, &tx);
    assert!(
        !svc.inner
            .slots
            .lock()
            .unwrap()
            .keys()
            .any(|(_, n)| n == "a")
    );
    // A first check's phase is within the interval (a minute at most).
    assert!(jitter(0) == 0 && jitter(100).abs() <= 100);
}

#[test]
fn certificates_are_warned_about_once() {
    let dir = tempfile::tempdir().unwrap();
    let (svc, _) = service(dir.path());
    let mut m = Monitor::new("x", Kind::Http);
    m.url = Some("https://a.example.com/".into());
    let mut s = State::default();
    let now = 1_800_000_000_000u64;
    let o = |days: u64| Outcome {
        at: now,
        ok: true,
        cert_expires: Some(now / 1000 + days * 86_400 + 60),
        url: Some("https://a.example.com/".into()),
        ..Default::default()
    };
    assert_eq!(svc.cert_due(&m, &o(30), &mut s), None);
    assert_eq!(svc.cert_due(&m, &o(9), &mut s), Some(9));
    assert_eq!(
        svc.cert_due(&m, &o(9), &mut s),
        None,
        "once per certificate"
    );
    assert_eq!(
        svc.cert_due(&m, &o(8), &mut s),
        Some(8),
        "a new certificate"
    );
    m.cert_expiry_days = 0;
    assert_eq!(svc.cert_due(&m, &o(1), &mut s), None);
    let msg = cert_message(&m, &o(9), 9);
    assert!(msg.contains("expires in 9 days (2027-01-"), "{msg}");
    assert_eq!(civil(0), (1970, 1, 1));
    assert_eq!(
        civil(super::super::probe::days_from_civil(2024, 2, 29)),
        (2024, 2, 29)
    );
}

#[test]
fn messages() {
    let mut m = Monitor::new("web", Kind::Tcp);
    (m.host, m.port) = (Some("db".into()), Some(5432));
    let o = Outcome {
        error: Some("timed out connecting".into()),
        ..Default::default()
    };
    let d = down_message(&m, &o, 2, false);
    assert_eq!(
        d,
        "Monitor web is DOWN: db:5432: timed out connecting (2 failed checks in a row)"
    );
    assert!(down_message(&m, &o, 1, true).contains("flapping"));
    let o = Outcome {
        latency_ms: Some(12),
        url: Some("db:5432".into()),
        ..Default::default()
    };
    assert_eq!(
        up_message(&m, &o, 252_000),
        "Monitor web is UP again after 4m 12s: db:5432 answered in 12 ms"
    );
}

fn failed(at: u64) -> Outcome {
    Outcome {
        at,
        ok: false,
        error: Some("connection refused".into()),
        ..Default::default()
    }
}

#[test]
fn failures_before_the_first_success_are_pending_not_downtime() {
    let dir = tempfile::tempdir().unwrap();
    let (svc, ctl) = service(dir.path());
    let org = OrgId::new("acme").unwrap();
    let (port, status) = flappy();
    status.store(503, Ordering::SeqCst);
    let mut m = Monitor::new("shop", Kind::Http);
    m.url = Some(format!("http://127.0.0.1:{port}/"));
    svc.create(&org, m.clone()).unwrap();
    let run = |n: usize| {
        for _ in 0..n {
            let o = svc.check(&org, &m);
            svc.record(&org, &m, o).unwrap();
        }
    };
    run(5);
    let s = svc.summary(&org, &m).unwrap();
    assert_eq!(s["status"], "pending");
    assert_eq!(s["never_up"], false);
    assert!(s["incident"].is_null());
    assert_eq!(s["uptime"]["24h"], Value::Null);
    assert!(kinds(&ctl).is_empty(), "{:?}", kinds(&ctl));
    {
        let db = svc.db(&org).unwrap();
        let db = db.lock().unwrap();
        assert!(db.incidents(None, 10, now_ms()).unwrap().is_empty());
        assert!(db.recent("shop", 10).unwrap().iter().all(|c| c.pending));
    }
    // The first success: up, quietly, and 100% (the pending ones count for nothing).
    status.store(200, Ordering::SeqCst);
    run(1);
    let s = svc.summary(&org, &m).unwrap();
    assert_eq!(s["status"], "up");
    assert_eq!(s["uptime"]["24h"], 100.0);
    assert!(kinds(&ctl).is_empty());
    // Real downtime after that pages as usual.
    status.store(503, Ordering::SeqCst);
    run(2);
    let k = kinds(&ctl);
    assert_eq!(k.len(), 1, "{k:?}");
    assert_eq!(k[0].0, "monitor.down");
}

#[test]
fn a_monitor_that_never_comes_up_says_so_once() {
    let dir = tempfile::tempdir().unwrap();
    let (svc, ctl) = service(dir.path());
    let org = OrgId::new("acme").unwrap();
    let mut m = Monitor::new("db", Kind::Tcp);
    (m.host, m.port) = (Some("db.example.com".into()), Some(5432));
    svc.create(&org, m.clone()).unwrap();
    let t0 = now_ms();
    svc.record(&org, &m, failed(t0)).unwrap();
    svc.record(&org, &m, failed(t0 + 10 * 60_000)).unwrap();
    assert!(kinds(&ctl).is_empty());
    let late = t0 + super::super::state::NEVER_UP_MS + 1000;
    svc.record(&org, &m, failed(late)).unwrap();
    svc.record(&org, &m, failed(late + 60_000)).unwrap();
    let k = kinds(&ctl);
    assert_eq!(k.len(), 1, "{k:?}");
    assert_eq!(k[0].0, "monitor.down");
    assert!(k[0].1.contains("never came up"), "{}", k[0].1);
    let d = svc.details(&org, "monitor.down", &k[0].1).unwrap();
    assert_eq!(d["never_up"], true);
    let s = svc.summary(&org, &m).unwrap();
    assert_eq!(
        (s["status"].as_str(), s["never_up"].as_bool()),
        (Some("pending"), Some(true))
    );
    // Its first success clears the flag, closes the incident and says up.
    let ok = Outcome {
        at: late + 120_000,
        ok: true,
        latency_ms: Some(5),
        ..Default::default()
    };
    svc.record(&org, &m, ok).unwrap();
    let k = kinds(&ctl);
    assert_eq!(
        k.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(),
        ["monitor.down", "monitor.up"]
    );
    let db = svc.db(&org).unwrap();
    let inc = db.lock().unwrap().incidents(None, 10, now_ms()).unwrap();
    assert_eq!(inc.len(), 1);
    assert!(inc[0].ended.is_some());
}

#[test]
fn an_app_monitor_waits_for_a_live_deployment() {
    let dir = tempfile::tempdir().unwrap();
    let (svc, ctl) = service(dir.path());
    let org = OrgId::new("acme").unwrap();
    let mut m = Monitor::new("app-web", Kind::App);
    m.app = Some("web".into());
    m.auto = true;
    svc.save(&org, &[m.clone()]).unwrap();
    // No app, so nothing live: the check is a wait, kept as pending.
    let o = svc.check(&org, &m);
    assert_eq!(o.error.as_deref(), Some(WAITING_FOR_APP));
    svc.record(&org, &m, o).unwrap();
    let st = svc.stored(&org, "app-web").unwrap();
    assert_eq!(st.state.status, Status::Pending);
    assert!(kinds(&ctl).is_empty());
    let db = svc.db(&org).unwrap();
    let c = db.lock().unwrap().recent("app-web", 5).unwrap();
    assert!(c[0].pending);
}

/// The ingress as the controller sees it: fixed domains per service.
type Domains = BTreeMap<(String, String), Vec<crate::ingress::DomainStatus>>;
struct FakeIngress(Mutex<Domains>);

impl crate::stack::controller::Observer for FakeIngress {
    fn rotation(&self, _: &str, _: &str, _: &[std::net::IpAddr]) {}
    fn drain(&self, _: &str, _: &str, _: std::net::IpAddr, _: Duration) {}
    fn stacks_changed(&self, _: Vec<Arc<crate::stack::StackDef>>) {}
    fn domains(&self, stack: &str, service: &str) -> Vec<crate::ingress::DomainStatus> {
        let m = self.0.lock().unwrap();
        m.get(&(stack.to_string(), service.to_string()))
            .cloned()
            .unwrap_or_default()
    }
}

impl FakeIngress {
    fn set(&self, stack: &str, service: &str, d: Vec<crate::ingress::DomainStatus>) {
        self.0
            .lock()
            .unwrap()
            .insert((stack.to_string(), service.to_string()), d);
    }
}

fn serving(url: &str, upstream: &str) -> crate::ingress::DomainStatus {
    crate::ingress::DomainStatus {
        host: "wiki.acme.dev".into(),
        path: "/".into(),
        url: Some(url.into()),
        provider: "caddy".into(),
        state: "serving".into(),
        cert: "none".into(),
        upstreams: vec![upstream.into()],
        ..Default::default()
    }
}

/// A service over a controller with `stacks` deployed in org acme and a
/// fake ingress; private targets as `allow_private` says.
fn service_with(
    dir: &Path,
    stacks: &[(&str, &str)],
    allow_private: bool,
) -> (Monitors, Arc<FakeIngress>) {
    let store = crate::stack::Store::open(dir).unwrap();
    for (name, y) in stacks {
        store
            .save(&crate::stack::StackDef {
                source: None,
                domains: Default::default(),
                name: (*name).into(),
                org: OrgId::new("acme").unwrap(),
                file: serde_yaml_ng::from_str(y).unwrap(),
                base_dir: "/".into(),
                secrets: Default::default(),
                force: Default::default(),
                images: Default::default(),
                deployed_at: 0,
                deployed_by: String::new(),
                previous: None,
            })
            .unwrap();
    }
    let k = crate::secrets::Keyring::new(age::x25519::Identity::generate(), vec![]);
    let secrets = Arc::new(Secrets::new(crate::secrets::LocalDriver::new(
        dir,
        Arc::new(k),
    )));
    let client = Client::with_socket("/nonexistent/isb-test/incus.sock");
    let ing = Arc::new(FakeIngress(Mutex::new(BTreeMap::new())));
    let ctl = Controller::start_with(
        client.clone(),
        store,
        Duration::from_secs(3600),
        secrets.clone(),
        Some(ing.clone()),
    )
    .unwrap();
    let apps = Apps::new(dir, client, ctl, secrets.clone());
    let m = Monitors::new(dir, apps, secrets, Arc::new(move || allow_private), None);
    (m, ing)
}

/// An HTTP server that answers every request with `head` and `ok`.
fn answering(head: &'static str) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let mut b = [0u8; 4096];
            let _ = s.read(&mut b);
            let _ = write!(s, "{head}\r\nContent-Length: 2\r\n\r\nok");
        }
    });
    port
}

const WIKI: &str = "services:\n  web: {image: x, domains: [{host: wiki.acme.dev, port: 80}]}\n  redis: {image: x}\n";

#[test]
fn compose_stack_services_with_a_domain_get_their_own_monitor() {
    let dir = tempfile::tempdir().unwrap();
    let app_web = "services:\n  web: {image: x, labels: {isb.app: web}, domains: [{host: shop.acme.dev, port: 80}]}\n";
    let (svc, ing) = service_with(
        dir.path(),
        &[
            ("wiki", WIKI),
            ("isb-tunnel", "services:\n  cloudflared: {image: x}\n"),
            ("shop-production", app_web),
            ("shop-production-pr-3", app_web),
        ],
        true,
    );
    let org = OrgId::new("acme").unwrap();
    for s in ["acme/shop-production", "acme/shop-production-pr-3"] {
        ing.set(
            s,
            "web",
            vec![serving("https://shop.acme.dev/", "10.0.0.6:80")],
        );
    }
    // Declared but not served yet: nothing.
    svc.sync_auto(&org).unwrap();
    assert!(svc.list(&org).unwrap().is_empty());
    ing.set(
        "acme/wiki",
        "web",
        vec![serving("https://wiki.acme.dev/", "10.0.0.5:80")],
    );
    svc.sync_auto(&org).unwrap();
    let all = svc.list(&org).unwrap();
    // Only wiki's web: not redis (no domain), the tunnel, or what apps render.
    assert_eq!(all.len(), 1, "{all:?}");
    let m = &all[0];
    assert_eq!(m.name, "stack-wiki-web");
    assert_eq!((m.kind, m.auto), (Kind::Service, true));
    assert_eq!(m.target(), "service wiki/web");
    // Not live yet: a pending wait, as an app's.
    let o = svc.check(&org, m);
    assert_eq!(o.error.as_deref(), Some(WAITING_FOR_SERVICE));
    // Deleting it excludes the service from then on.
    svc.delete(&org, "stack-wiki-web").unwrap();
    assert_eq!(svc.settings(&org).unwrap().exclude_services, ["wiki/web"]);
    svc.sync_auto(&org).unwrap();
    assert!(svc.list(&org).unwrap().is_empty());
    // Made by hand, it must name a stack and service that exist.
    let mut h = Monitor::new("w", Kind::Service);
    (h.stack, h.service) = (Some("wiki".into()), Some("nope".into()));
    assert!(svc.create(&org, h.clone()).is_err());
    h.service = Some("web".into());
    svc.create(&org, h).unwrap();
}

#[test]
fn a_service_behind_access_is_checked_at_its_own_endpoint() {
    let org = OrgId::new("acme").unwrap();
    let access = answering(
        "HTTP/1.1 302 Found\r\nLocation: https://team.cloudflareaccess.com/cdn-cgi/access/login/x",
    );
    let origin = answering("HTTP/1.1 200 OK");
    let mut m = Monitor::new("stack-wiki-web", Kind::Service);
    (m.stack, m.service) = (Some("wiki".into()), Some("web".into()));
    let upstream = format!("127.0.0.1:{origin}");
    for (allow_private, public, why) in [
        (true, access, "behind Cloudflare Access"),
        (false, origin, "private address"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (svc, ing) = service_with(dir.path(), &[("wiki", WIKI)], allow_private);
        let url = format!("http://127.0.0.1:{public}/");
        ing.set("acme/wiki", "web", vec![serving(&url, &upstream)]);
        svc.create(&org, m.clone()).unwrap();
        // Up before: checked without waiting for a live deployment.
        svc.edit_state(&org, &m.name, |s| s.status = Status::Up)
            .unwrap();
        let o = svc.check(&org, &m);
        assert!(o.ok, "{o:?}");
        assert_eq!(
            o.via.as_deref(),
            Some(format!("internal: upstream {upstream}").as_str())
        );
        let note = o.note.unwrap_or_default();
        assert!(
            note.contains(why) && note.contains("checked the service's own endpoint"),
            "{note}"
        );
        // Events are about the stack's service.
        assert_eq!(
            svc.subject(&org, &m),
            ("acme/wiki".to_string(), "web".to_string())
        );
    }
}
