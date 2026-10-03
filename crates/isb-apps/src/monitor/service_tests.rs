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
