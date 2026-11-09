//! Addresses on an org bridge against a real incusd
//! (docs/concepts/orgs.md#addresses): a sandbox is pinned to an address no
//! other instance holds when it is created; the daemon's check pins an
//! instance made behind isb's back at the address it has without
//! restarting it, gives a stopped one a free address, adopts a pin set by
//! hand, and reports two instances holding one address.
//!
//! Gated like the other integration tests (`ISB_INTEGRATION=1`; build in a
//! sandbox with `cargo test --no-run`, run the binary on the host). Works in
//! the test org (`common::test_org`), removed afterwards, pass or fail; the
//! check is held to that org's bridge.

#[allow(dead_code, reason = "each test binary uses some of the shared helpers")]
mod common;

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use isb::Client;
use serde_json::{Value, json};

const NAMES: [&str; 5] = [
    "isb-test-ip-a",
    "isb-test-ip-b",
    "isb-test-ip-raw",
    "isb-test-ip-dup",
    "isb-test-ip-cold",
];

struct Rm(Client);
impl Drop for Rm {
    fn drop(&mut self) {
        for n in NAMES {
            let _ = isb::Sandbox::remove(&self.0, n, true);
        }
    }
}

fn inst(oc: &Client, name: &str) -> Value {
    oc.get(&format!("/1.0/instances/{name}")).unwrap()
}

fn pin(oc: &Client, name: &str) -> Option<String> {
    inst(oc, name)["devices"]["eth0"]["ipv4.address"]
        .as_str()
        .map(String::from)
}

fn recorded(oc: &Client, name: &str) -> Option<String> {
    inst(oc, name)["config"]["user.isb.ipv4.eth0"]
        .as_str()
        .map(String::from)
}

/// The instance's IPv4 address on eth0, once it has one, and its PID.
fn address(oc: &Client, name: &str) -> (String, i64) {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let st = oc.get(&format!("/1.0/instances/{name}/state")).unwrap();
        let a = st["network"]["eth0"]["addresses"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|a| a["family"] == "inet")
            .and_then(|a| a["address"].as_str().map(String::from));
        if let Some(a) = a {
            return (a, st["pid"].as_i64().unwrap_or_default());
        }
        assert!(Instant::now() < deadline, "{name} got no address");
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// An instance created behind isb's back (no pin), with `devices`.
fn create_raw(oc: &Client, name: &str, devices: Value, start: bool) {
    let body = json!({
        "name": name, "type": "container",
        "source": {"type": "image", "alias": common::image()},
        "config": {"user.isb-test": "1"}, "devices": devices, "profiles": ["default"],
    });
    let body = isb::org::disk::sized_root(oc, body);
    oc.mutate(
        "POST",
        "/1.0/instances",
        Some(&body),
        "create",
        Duration::from_secs(300),
    )
    .unwrap();
    if start {
        oc.mutate(
            "PUT",
            &format!("/1.0/instances/{name}/state"),
            Some(&json!({"action": "start", "timeout": 30})),
            "start",
            Duration::from_secs(120),
        )
        .unwrap();
    }
}

fn check(base: &Client, oc: &Client) -> Vec<isb::ipam::Finding> {
    isb::ipam::check(base, Some(oc.project_name()), &mut |m| {
        eprintln!("check: {m}")
    })
    .unwrap()
}

#[test]
fn addresses_on_an_org_bridge_are_pinned_and_checked() {
    if !common::enabled() {
        return;
    }
    let base = Client::new();
    let oc = common::test_org_client(&base);
    let _rm = Rm(oc.clone());

    // Created by isb: pinned at create, the pin recorded and taken.
    let spec = |n: &str| {
        let mut s = isb::SandboxSpec::new(n, common::image());
        s.labels.insert("isb-test".into(), "1".into());
        s
    };
    isb::Sandbox::create(&oc, &spec("isb-test-ip-a")).unwrap();
    isb::Sandbox::create(&oc, &spec("isb-test-ip-b")).unwrap();
    let (a, b) = (
        address(&oc, "isb-test-ip-a").0,
        address(&oc, "isb-test-ip-b").0,
    );
    assert_ne!(a, b);
    for (n, ip) in [("isb-test-ip-a", &a), ("isb-test-ip-b", &b)] {
        assert_eq!(pin(&oc, n).as_ref(), Some(ip), "{n}");
        assert_eq!(recorded(&oc, n).as_ref(), Some(ip), "{n}");
    }
    // Planning it again changes nothing, even pruning devices: the pin is
    // not drift from the spec.
    let d = isb::sandbox::resolve(
        &oc,
        &spec("isb-test-ip-a"),
        &isb::plan::VolumeDefs::new(),
        std::path::Path::new("/"),
    )
    .unwrap();
    let prune = isb::plan::DiffOptions {
        prune_devices: true,
    };
    let plan = isb::sandbox::plan_desired(&oc, &d, prune).unwrap();
    assert!(plan.is_noop(), "{plan:?}");

    // Behind isb's back: pinned in place by the check, same address, same
    // process.
    create_raw(&oc, "isb-test-ip-raw", json!({}), true);
    assert_eq!(pin(&oc, "isb-test-ip-raw"), None);
    let (raw, pid) = address(&oc, "isb-test-ip-raw");
    let found = check(&base, &oc);
    assert!(found.is_empty(), "{found:?}");
    assert_eq!(pin(&oc, "isb-test-ip-raw").as_ref(), Some(&raw));
    assert_eq!(recorded(&oc, "isb-test-ip-raw").as_ref(), Some(&raw));
    assert_eq!(
        address(&oc, "isb-test-ip-raw"),
        (raw.clone(), pid),
        "restarted or moved"
    );

    // A pin whose record is gone (one set by hand) is adopted.
    isb::sandbox::update_instance(&oc, "isb-test-ip-a", "unrecord", &mut |c, _| {
        c.remove("user.isb.ipv4.eth0");
        Ok(())
    })
    .unwrap();
    assert!(check(&base, &oc).is_empty());
    assert_eq!(recorded(&oc, "isb-test-ip-a").as_ref(), Some(&a));

    // A stopped instance without a pin gets a free address.
    create_raw(&oc, "isb-test-ip-cold", json!({}), false);
    assert!(check(&base, &oc).is_empty());
    let cold = pin(&oc, "isb-test-ip-cold").expect("pinned");
    assert!(![&a, &b, &raw].contains(&&cold), "{cold}");
    cold.parse::<Ipv4Addr>().unwrap();

    // Pinned by hand to b's address (stopped: incus would refuse to start
    // it): reported, nothing moved.
    let mut nic = inst(&oc, "isb-test-ip-b")["expanded_devices"]["eth0"].clone();
    nic["ipv4.address"] = json!(b);
    create_raw(&oc, "isb-test-ip-dup", json!({"eth0": nic}), false);
    let found = check(&base, &oc);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].message.contains(&b), "{}", found[0].message);
    assert!(
        found[0].message.contains("isb-test-ip-dup"),
        "{}",
        found[0].message
    );
    assert_eq!(
        recorded(&oc, "isb-test-ip-dup"),
        None,
        "a shared pin is not isb's"
    );
    assert_eq!(pin(&oc, "isb-test-ip-b").as_ref(), Some(&b));
    assert_eq!(address(&oc, "isb-test-ip-b").0, b);
}
