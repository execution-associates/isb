use super::*;
use serde_json::json;

fn bridge(name: &str, addr: &str) -> Value {
    json!({"name": name, "type": "bridge", "managed": true, "config": {"ipv4.address": addr}})
}

fn net() -> Subnet {
    Subnet::from_network(&bridge("isbbr1", "10.83.115.1/24")).unwrap()
}

fn ip(s: &str) -> Ipv4Addr {
    s.parse().unwrap()
}

/// An instance as `GET /1.0/instances?recursion=2` lists it, on isbbr1:
/// `pin` its NIC's ipv4.address, `addr` the address it has (running).
fn inst(name: &str, pin: Option<&str>, addr: Option<&str>, created: &str) -> Value {
    let mut nic = json!({"type": "nic", "network": "isbbr1", "name": "eth0"});
    if let Some(p) = pin {
        nic["ipv4.address"] = json!(p);
    }
    let state = match addr {
        Some(a) => json!({"network": {
            "lo": {"hwaddr": "", "addresses": [{"family": "inet", "address": "127.0.0.1"}]},
            "eth0": {"hwaddr": "10:66:6a:00:00:01", "addresses": [
                {"family": "inet", "address": a}, {"family": "inet6", "address": "fe80::1"}]}}}),
        None => json!(null),
    };
    json!({
        "name": name, "project": "isb-exa", "created_at": created,
        "status": if addr.is_some() { "Running" } else { "Stopped" },
        "config": {"volatile.eth0.hwaddr": "10:66:6a:00:00:01", "user.isb.stack": "analytics",
                   "user.isb.service": "postgres"},
        "expanded_devices": {"root": {"type": "disk"}, "eth0": nic},
        "state": state,
    })
}

fn plan_of(insts: &[Value]) -> (Vec<Fix>, Vec<Finding>) {
    plan(
        "isbbr1",
        &net(),
        &claims(insts, "isbbr1", &net()),
        &BTreeSet::new(),
    )
}

#[test]
fn a_subnet_is_a_managed_bridge_with_an_ipv4_address() {
    let s = net();
    assert!(s.contains(ip("10.83.115.43")));
    assert!(!s.contains(ip("10.83.116.43")));
    assert!(
        Subnet::from_network(&bridge("isbbrxabcd", "10.1.2.1/24")).is_none(),
        "egress"
    );
    assert!(Subnet::from_network(&bridge("isbbr1", "none")).is_none());
    let mut unmanaged = bridge("br0", "10.1.2.1/24");
    unmanaged["managed"] = json!(false);
    assert!(Subnet::from_network(&unmanaged).is_none());
}

#[test]
fn a_free_address_is_never_the_gateway_network_broadcast_or_used() {
    let s = Subnet::from_network(&bridge("isbbr1", "10.9.9.1/29")).unwrap();
    // .0 network, .1 gateway, .7 broadcast: .2-.6 are the hosts.
    let mut used = BTreeSet::new();
    let mut got = Vec::new();
    while let Some(a) = s.pick_free(&used, "x") {
        used.insert(a);
        got.push(a);
    }
    got.sort();
    let want: Vec<Ipv4Addr> = (2..=6).map(|h| Ipv4Addr::new(10, 9, 9, h)).collect();
    assert_eq!(got, want);
}

#[test]
fn seeds_spread_picks_over_the_subnet() {
    let s = net();
    let used = BTreeSet::new();
    let picks: BTreeSet<Ipv4Addr> = (0..20)
        .filter_map(|i| s.pick_free(&used, &format!("isb-exa/web-1-{i}")))
        .collect();
    assert!(picks.len() > 15, "{picks:?}");
}

#[test]
fn a_claim_reads_pin_address_and_labels() {
    let c = claims(
        &[inst("pg", Some("10.83.115.43"), Some("10.83.115.43"), "a")],
        "isbbr1",
        &net(),
    );
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].pin, Some(ip("10.83.115.43")));
    assert_eq!(c[0].addrs, [ip("10.83.115.43")]);
    assert_eq!(c[0].stack.as_deref(), Some("analytics"));
    // Another network, and a project outside isb's orgs, are not claims.
    assert!(claims(&[inst("pg", None, None, "a")], "isbbr2", &net()).is_empty());
    let mut other = inst("pg", None, None, "a");
    other["project"] = json!("default");
    assert!(claims(&[other], "isbbr1", &net()).is_empty());
}

#[test]
fn a_vms_address_is_found_by_mac() {
    let mut v = inst("vm", None, Some("10.83.115.9"), "a");
    let eth0 = v["state"]["network"]["eth0"].take();
    v["state"]["network"]["enp5s0"] = eth0;
    v["state"]["network"]
        .as_object_mut()
        .unwrap()
        .remove("eth0");
    let c = claims(&[v], "isbbr1", &net());
    assert_eq!(c[0].addrs, [ip("10.83.115.9")]);
}

#[test]
fn the_incident_is_a_conflict_about_the_newcomer() {
    // 2026-11-09: the gateway got the address analytics-postgres held.
    let (fixes, found) = plan_of(&[
        inst(
            "analytics-production-analytics-postgres-1-0a5e",
            None,
            Some("10.83.115.43"),
            "2026-10-12",
        ),
        inst(
            "mcp-gateway-1-2ee0",
            None,
            Some("10.83.115.43"),
            "2026-11-09",
        ),
    ]);
    assert!(
        fixes.is_empty(),
        "neither is pinned at a shared address: {fixes:?}"
    );
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].instance, "mcp-gateway-1-2ee0");
    assert!(
        found[0].message.contains("10.83.115.43"),
        "{}",
        found[0].message
    );
    assert!(
        found[0]
            .message
            .contains("analytics-production-analytics-postgres-1-0a5e")
    );
}

#[test]
fn a_pin_and_another_instances_address_conflict() {
    // A stopped instance pinned to an address a running one has: it could
    // not start.
    let (fixes, found) = plan_of(&[
        inst("a", Some("10.83.115.43"), None, "1"),
        inst("b", None, Some("10.83.115.43"), "2"),
    ]);
    assert!(fixes.is_empty(), "a shared pin is not adopted: {fixes:?}");
    assert_eq!(found.len(), 1);
    assert!(
        found[0].message.contains("a (pinned)"),
        "{}",
        found[0].message
    );
    assert!(
        found[0].message.contains("b (has it)"),
        "{}",
        found[0].message
    );
}

#[test]
fn a_running_instance_is_pinned_at_its_own_address() {
    let (fixes, found) = plan_of(&[inst("web", None, Some("10.83.115.7"), "1")]);
    assert!(found.is_empty());
    let [
        Fix::Pin {
            ip: a,
            props,
            running: true,
            ..
        },
    ] = fixes.as_slice()
    else {
        panic!("{fixes:?}");
    };
    assert_eq!(*a, ip("10.83.115.7"));
    assert_eq!(props["network"], "isbbr1");
    assert_eq!(props["ipv4.address"], "10.83.115.7");
}

#[test]
fn a_stopped_instance_gets_a_free_address() {
    let reserved = BTreeSet::from([ip("10.83.115.9")]);
    let insts = [
        inst("pg", Some("10.83.115.8"), Some("10.83.115.8"), "1"),
        inst("old", None, None, "2"),
    ];
    let cl = claims(&insts, "isbbr1", &net());
    let (fixes, _) = plan("isbbr1", &net(), &cl, &reserved);
    let pinned: Vec<Ipv4Addr> = fixes
        .iter()
        .filter_map(|f| match f {
            Fix::Pin { instance, ip, .. } if instance == "old" => Some(*ip),
            _ => None,
        })
        .collect();
    assert_eq!(pinned.len(), 1);
    assert!(![ip("10.83.115.8"), ip("10.83.115.9"), ip("10.83.115.1")].contains(&pinned[0]));
}

#[test]
fn a_pin_set_by_hand_is_adopted_once() {
    let mut v = inst("pg", Some("10.83.115.43"), Some("10.83.115.43"), "1");
    let (fixes, found) = plan_of(std::slice::from_ref(&v));
    assert!(found.is_empty());
    assert_eq!(
        fixes,
        [Fix::Adopt {
            project: "isb-exa".into(),
            instance: "pg".into(),
            device: "eth0".into(),
            ip: ip("10.83.115.43"),
        }]
    );
    v["config"]["user.isb.ipv4.eth0"] = json!("10.83.115.43");
    assert_eq!(plan_of(&[v]), (vec![], vec![]));
}

#[test]
fn a_pin_the_instance_does_not_have_is_drift() {
    let mut v = inst("e", Some("10.83.115.28"), Some("10.83.115.7"), "1");
    v["config"]["user.isb.ipv4.eth0"] = json!("10.83.115.28");
    let (fixes, found) = plan_of(&[v]);
    assert!(fixes.is_empty());
    assert_eq!(found.len(), 1);
    assert!(
        found[0]
            .message
            .contains("pinned to 10.83.115.28 but has 10.83.115.7")
    );
}

#[test]
fn a_starting_instance_waits_for_its_address() {
    let mut v = inst("web", None, Some("10.83.115.7"), "1");
    v["state"]["network"]["eth0"]["addresses"] = json!([]);
    assert_eq!(plan_of(&[v]), (vec![], vec![]));
}

#[test]
fn findings_keep_their_key_from_pass_to_pass() {
    let insts = [
        inst("a", None, Some("10.83.115.43"), "1"),
        inst("b", None, Some("10.83.115.43"), "2"),
    ];
    assert_eq!(plan_of(&insts).1[0].key, plan_of(&insts).1[0].key);
}

#[test]
fn the_diff_ignores_and_never_prunes_isbs_pin() {
    let desired = Props::from([
        ("type".into(), "nic".into()),
        ("network".into(), "isbbr1".into()),
    ]);
    let mut actual = desired.clone();
    actual.insert("ipv4.address".into(), "10.83.115.43".into());
    let mut a = actual.clone();
    ignore_pin(&desired, &mut a);
    assert_eq!(a, desired);
    // A spec that names an address keeps it compared.
    let mut want = desired.clone();
    want.insert("ipv4.address".into(), "10.83.115.44".into());
    let mut a = actual.clone();
    ignore_pin(&want, &mut a);
    assert_ne!(a, want);

    let config = Props::from([("user.isb.ipv4.eth0".into(), "10.83.115.43".into())]);
    assert!(is_pin("eth0", &actual, &config));
    assert!(!is_pin("eth0", &actual, &Props::new()));
    assert!(!is_pin("eth0", &desired, &config));
}
