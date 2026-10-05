//! A compose stack service's own uptime monitor against a real incusd
//! (`ISB_INTEGRATION=1`, docs/guides/uptime.md): a service with a served
//! domain gets `stack-<stack>-<service>`, its check falls back to the
//! replica the ingress routes to when the domain resolves to a private
//! address, and removing the domain removes the monitor.
//!
//! The domain is `<stack>.127.0.0.1.sslip.io`, so the host needs DNS. The
//! stack runs in org `isb-test` and is removed afterwards, pass or fail.

use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use isb::Client;
use isb::monitor::{Kind, Monitors};

#[allow(dead_code, reason = "each test binary uses some of the shared helpers")]
mod common;
use common::{enabled, image, q, test_def, test_org, test_org_client};

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn test_secrets(state: &std::path::Path) -> Arc<isb::secrets::Secrets> {
    let k = isb::secrets::Keyring::new(age::x25519::Identity::generate(), vec![]);
    Arc::new(isb::secrets::Secrets::new(isb::secrets::LocalDriver::new(
        state,
        Arc::new(k),
    )))
}

fn def(stack: &str, state: &std::path::Path, domain: Option<&str>) -> isb::stack::StackDef {
    let doms = domain
        .map(|h| format!("\x20   domains: [{{host: {h}, port: 8000, https: false}}]\n"))
        .unwrap_or_default();
    let yaml = format!(
        "services:\n  web:\n    image: {}\n    labels: {{isb-test: '1'}}\n    user: dev\n\
         \x20   command: [sh, -c, 'echo ok > /tmp/index.html && exec python3 -m http.server 8000 -d /tmp']\n\
         \x20   healthcheck: {{test: [CMD, python3, -c, \"import urllib.request as u; u.urlopen('http://127.0.0.1:8000')\"], interval: 2s, start_interval: 1s}}\n{doms}",
        image()
    );
    let p = isb::compose::load_docs(
        &[(state.join("isb.yaml"), yaml)],
        state,
        Some(stack),
        &|_| None,
    )
    .unwrap();
    test_def(stack, test_org(), p.file, state)
}

#[test]
fn a_stack_service_with_a_domain_gets_a_monitor_until_the_domain_goes() {
    if !enabled() {
        return;
    }
    let client = Client::new();
    test_org_client(&client);
    let state = tempfile::tempdir().unwrap();
    let store = isb::stack::Store::open(state.path()).unwrap();
    let secrets = test_secrets(state.path());
    let hp = free_port();
    let cfg = isb::ingress::IngressConfig {
        http: Some(format!("127.0.0.1:{hp}").parse().unwrap()),
        caddy_bin: std::env::var_os("ISB_CADDY_BIN").map(PathBuf::from),
        ..Default::default()
    };
    let m = isb::ingress::Manager::new(cfg, client.clone(), secrets.clone(), state.path()).unwrap();
    let ctl = isb::stack::Controller::start_with(
        client.clone(),
        store,
        Duration::from_secs(2),
        secrets.clone(),
        Some(m.clone()),
    )
    .unwrap();
    m.start(ctl.clone()).unwrap();
    let stack = format!("isb-test-m{}", std::process::id() % 100000);
    struct Rm(isb::stack::Controller, String, Arc<isb::ingress::Manager>);
    impl Drop for Rm {
        fn drop(&mut self) {
            let _ = self.0.remove(&self.1, true, Duration::from_secs(120));
            self.0.shutdown();
            self.2.shutdown();
        }
    }
    let _rm = Rm(ctl.clone(), q(&stack), m.clone());
    let apps = isb::app::Apps::new(state.path(), client.clone(), ctl.clone(), secrets.clone());
    let mons = Monitors::new(state.path(), apps, secrets, Arc::new(|| false), None);
    let org = test_org();
    let name = format!("stack-{stack}-web");

    let host = format!("{stack}.127.0.0.1.sslip.io");
    let d = def(&stack, state.path(), Some(&host));
    m.check(&d).unwrap();
    ctl.deploy(d).unwrap();
    let st = isb::daemon::wait_settled(&ctl, &q(&stack), Duration::from_secs(300)).unwrap();
    assert!(st.converged, "{st:?}");

    // Served within seconds of converging: the next sync makes its monitor.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mon = loop {
        mons.sync_auto(&org).unwrap();
        if let Ok(x) = mons.get(&org, &name) {
            break x;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no {name}: {:?}",
            ctl.status(&q(&stack))
                .map(|s| s.services[0].domains.clone())
        );
        std::thread::sleep(Duration::from_millis(500));
    };
    assert_eq!((mon.kind, mon.auto), (Kind::Service, true));
    assert_eq!(mon.stack.as_deref(), Some(stack.as_str()));

    // The domain resolves to 127.0.0.1, refused as private: the check goes
    // to the replica the ingress routes to, with the domain as Host.
    let o = mons.check(&org, &mon);
    assert!(o.ok, "{o:?}");
    assert!(
        o.via
            .as_deref()
            .unwrap_or_default()
            .starts_with("internal: replica "),
        "{o:?}"
    );
    assert!(
        o.note
            .as_deref()
            .unwrap_or_default()
            .contains("private address"),
        "{o:?}"
    );

    // Redeployed without the domain: the monitor goes.
    ctl.deploy(def(&stack, state.path(), None)).unwrap();
    isb::daemon::wait_settled(&ctl, &q(&stack), Duration::from_secs(300)).unwrap();
    mons.sync_auto(&org).unwrap();
    assert!(mons.get(&org, &name).is_err(), "{:?}", mons.list(&org));
}
