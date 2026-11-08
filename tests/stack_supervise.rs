//! A stack replica's supervision against a real incusd (`ISB_INTEGRATION=1`;
//! build in a sandbox with `cargo test --no-run`, run the binary on the
//! host): an OCI replica's DNS is pinned to its org bridge, incus never
//! restarts it, and the controller restarts a crash loop with a backoff,
//! raising `restart.failed` once. Deploys into the test org
//! (`common::test_org`), removed afterwards, pass or fail.

use std::time::{Duration, Instant};

use isb::Client;

#[allow(dead_code, reason = "each test binary uses some of the shared helpers")]
mod common;
use common::{enabled, test_org_client};

fn test_secrets(state: &std::path::Path) -> std::sync::Arc<isb::secrets::Secrets> {
    let k = isb::secrets::Keyring::new(age::x25519::Identity::generate(), vec![]);
    std::sync::Arc::new(isb::secrets::Secrets::new(isb::secrets::LocalDriver::new(
        state,
        std::sync::Arc::new(k),
    )))
}

/// The app, as PID 1: waits for `/die`, then exits 1. Started again with
/// `/die` there, it counts the start in it and exits at once, five times;
/// the sixth start removes it and waits again.
const APP: &str = "if [ -f /die ]; then n=$(( $(cat /die) + 1 )); echo $n > /die; \
                   [ $n -lt 6 ] && exit 1; rm /die; fi; \
                   while [ ! -f /die ]; do sleep 1; done; exit 1";

#[test]
fn an_oci_replica_is_pinned_and_supervised_by_isb_alone() {
    if !enabled() {
        return;
    }
    let client = Client::new();
    let state = tempfile::tempdir().unwrap();
    let store = isb::stack::Store::open(state.path()).unwrap();
    let ctl = isb::stack::Controller::start(
        client.clone(),
        store,
        Duration::from_secs(2),
        test_secrets(state.path()),
    )
    .unwrap();
    let stack = format!("isb-test-s{}", std::process::id() % 100000);
    let q = common::q(&stack);
    struct Rm(isb::stack::Controller, String);
    impl Drop for Rm {
        fn drop(&mut self) {
            let _ = self.0.remove(&self.1, true, Duration::from_secs(120));
            self.0.shutdown();
        }
    }
    let _rm = Rm(ctl.clone(), q.clone());
    // `$$`: compose interpolates a single `$`.
    let app = APP.replace('$', "$$");
    let yaml = format!(
        "services:\n  app:\n    image: docker:busybox\n    labels: {{isb-test: '1'}}\n\
         \x20   command: [sh, -c, '{app}']\n"
    );
    let p = isb::compose::load_docs(
        &[(state.path().join("isb.yaml"), yaml)],
        state.path(),
        Some(&stack),
        &|_| None,
    )
    .unwrap();
    let org = common::test_org();
    ctl.deploy(common::test_def(&stack, org.clone(), p.file, state.path()))
        .unwrap();
    let st = isb::daemon::wait_settled(&ctl, &q, Duration::from_secs(300)).unwrap();
    assert!(st.converged, "{st:?}");
    let name = st.services[0].instances[0].name.clone();
    let oc = test_org_client(&client);

    // Pinned to the org bridge, and left to the controller to restart.
    let dns = isb::plan::dns::OciDns::detect(&oc, true).expect("the org bridge's resolver");
    assert_eq!(dns.domain, format!("{org}.isb"));
    let inst = oc
        .get(&format!("/1.0/instances/{name}"))
        .expect("the replica");
    let config = &inst["config"];
    assert_eq!(config["boot.autorestart"], "false", "{config}");
    assert_eq!(config["oci.dns.nameservers"], dns.nameserver.as_str());
    assert_eq!(config["oci.dns.domain"], dns.domain.as_str());
    let resolv = || {
        let b = oc.read_file(&name, "/etc/resolv.conf").unwrap().unwrap();
        String::from_utf8(b).unwrap()
    };
    let r = resolv();
    assert!(r.contains(&format!("nameserver {}", dns.nameserver)), "{r}");

    // Six exits: the first three are restarted at once, then 10 s, 20 s
    // and 40 s apart. incus restarting it too would be done in seconds.
    let (since, _) = ctl.events(0, 0);
    oc.push_file(&name, "/die", b"0\n", 0, 0, 0o644).unwrap();
    let started = Instant::now();
    let deadline = started + Duration::from_secs(300);
    let mut serving_again = false;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_secs(2));
        let running = oc
            .get(&format!("/1.0/instances/{name}"))
            .is_ok_and(|v| v["status"] == "Running");
        let gone = running && oc.read_file(&name, "/die").is_ok_and(|f| f.is_none());
        if gone {
            serving_again = true;
            break;
        }
    }
    let took = started.elapsed();
    assert!(serving_again, "still crash looping after {took:?}");
    assert!(took >= Duration::from_secs(60), "no backoff: {took:?}");
    let (_, events) = ctl.events(since, 1000);
    let kinds: Vec<&str> = events.iter().filter_map(|e| e.kind.as_deref()).collect();
    assert_eq!(
        kinds.iter().filter(|k| **k == "restart.failed").count(),
        1,
        "{events:?}"
    );
    // The controller made every start, none by incus behind its back. A
    // start the app dies during is retried once inside it (`Sandbox::start`),
    // so six starts may count as five.
    let restarts = ctl.status(&q).unwrap().services[0].instances[0].restarts;
    assert!((5..=6).contains(&restarts), "{restarts}: {events:?}");

    // Every one of those starts named the bridge's resolver.
    let r = resolv();
    assert!(r.contains(&format!("nameserver {}", dns.nameserver)), "{r}");
    std::thread::sleep(Duration::from_secs(12));
    let (_, events) = ctl.events(since, 1000);
    assert!(
        !events
            .iter()
            .any(|e| e.kind.as_deref() == Some("dns.failed")),
        "{events:?}"
    );
}
