//! An org's disk limit against a real incusd (docs/concepts/orgs.md#limits-are-budgets):
//! setting it is refused while an instance has no root size; under it each
//! new instance (a sandbox, a stack replica) gets a root size of its own and
//! the default profile none, and a 10GiB profile size isb wrote earlier
//! comes off.
//!
//! Gated like the other integration tests (`ISB_INTEGRATION=1`; build them
//! in a sandbox with `cargo test --no-run`, run the binary on the host). It
//! works in org `isb-test` and lifts the limit and removes its instances
//! afterwards, pass or fail.

#[allow(dead_code, reason = "each test binary uses some of the shared helpers")]
mod common;

use std::time::Duration;

use isb::Client;
use isb::org::{Limit, OrgId, OrgOptions};
use isb::stack::{Controller, StackDef};

/// Two services: one without a root size, one with its own.
fn def(org: &OrgId, name: &str, dir: &std::path::Path) -> StackDef {
    let file = serde_json::json!({"services": {
        "plain": {"image": common::image(), "command": ["sleep", "1d"]},
        "sized": {"image": common::image(), "command": ["sleep", "1d"],
                  "raw_devices": {"root": {"size": "15GiB"}}},
    }});
    let p = isb::compose::load_docs(
        &[(dir.join("isb.yaml"), file.to_string())],
        dir,
        Some(name),
        &|_| None,
    )
    .unwrap();
    common::test_def(name, org.clone(), p.file, dir)
}

fn set_disk(client: &Client, org: &OrgId, disk: Option<&str>) -> isb::Result<isb::org::OrgInfo> {
    let opts = OrgOptions {
        disk: disk.map(String::from),
        lift: if disk.is_none() {
            vec![Limit::Disk]
        } else {
            vec![]
        },
        ..Default::default()
    };
    isb::org::ensure(client, org, &opts, &mut |l| eprintln!("{l}"))
}

fn root(oc: &Client, name: &str) -> serde_json::Value {
    oc.get(&format!("/1.0/instances/{name}")).unwrap()["devices"]["root"].clone()
}

/// An instance without a root size blocks the limit, by name; nothing is
/// changed.
fn refused_while_unsized(client: &Client, oc: &Client, org: &OrgId) {
    let spec = isb::SandboxSpec::new("isb-test-disk-sb", common::image());
    isb::Sandbox::create(oc, &spec).unwrap();
    assert!(root(oc, "isb-test-disk-sb").get("size").is_none());
    let e = set_disk(client, org, Some("60GiB"))
        .unwrap_err()
        .to_string();
    assert!(e.contains("isb-test-disk-sb"), "{e}");
    assert!(e.contains("raw_devices: {root: {size: ...}}"), "{e}");
    let p = client
        .get(&format!("/1.0/projects/{}", org.incus_project()))
        .unwrap();
    assert!(p["config"].get("limits.disk").is_none(), "{p}");
    isb::Sandbox::remove(oc, "isb-test-disk-sb", true).unwrap();
}

/// The profile as an earlier isb left it under a disk limit: root 10GiB.
fn seed_isbs_old_profile_size(oc: &Client) {
    let mut prof = oc.get("/1.0/profiles/default").unwrap();
    prof["devices"]["root"]["size"] = serde_json::json!("10GiB");
    let body = serde_json::json!({
        "description": prof["description"], "config": prof["config"], "devices": prof["devices"],
    });
    oc.mutate(
        "PUT",
        "/1.0/profiles/default",
        Some(&body),
        "size the profile",
        Duration::from_secs(30),
    )
    .unwrap();
}

/// Each replica has its service's root size (else 10GiB) and the revision
/// computed from the spec alone.
fn replicas_have_their_own_size(ctl: &Controller, oc: &Client, q: &str, revs: &(String, String)) {
    let st = isb::daemon::wait_settled(ctl, q, Duration::from_secs(300)).unwrap();
    assert!(st.converged, "{st:?}");
    for svc in &st.services {
        let inst = &svc.instances[0].name;
        let (want, rev) = if svc.service == "sized" {
            ("15GiB", &revs.1)
        } else {
            ("10GiB", &revs.0)
        };
        assert_eq!(root(oc, inst)["size"], want, "{inst}");
        let got =
            oc.get(&format!("/1.0/instances/{inst}")).unwrap()["config"]["user.isb.rev"].clone();
        assert_eq!(got.as_str(), Some(rev.as_str()), "{inst}");
    }
}

#[test]
fn org_disk_limit_sizes_each_instance_not_the_profile() {
    if !common::enabled() {
        return;
    }
    let client = Client::new();
    let org = common::test_org();
    let oc = common::test_org_client(&client);
    let project = client
        .get(&format!("/1.0/projects/{}", org.incus_project()))
        .unwrap();
    if project["config"]["limits.disk"].is_string() {
        eprintln!("skipped: org {org} already has a disk limit");
        return;
    }

    struct Rm(Client, Client, OrgId, Option<Controller>);
    impl Drop for Rm {
        fn drop(&mut self) {
            if let Some(ctl) = self.3.take() {
                for s in ctl.definitions() {
                    let _ = ctl.remove(&s.qualified(), true, Duration::from_secs(120));
                }
                ctl.shutdown();
            }
            for n in ["isb-test-disk-sb", "isb-test-disk-sb2"] {
                let _ = isb::Sandbox::remove(&self.1, n, true);
            }
            let _ = set_disk(&self.0, &self.2, None);
        }
    }
    let mut rm = Rm(client.clone(), oc.clone(), org.clone(), None);

    refused_while_unsized(&client, &oc, &org);
    seed_isbs_old_profile_size(&oc);
    let info = set_disk(&client, &org, Some("60GiB")).unwrap();
    assert_eq!(info.disk.as_deref(), Some("60GiB"));
    assert_eq!(info.default_disk.as_deref(), Some("10GiB"));
    // No instance takes its root disk from the profile: its 10GiB is gone.
    let prof = oc.get("/1.0/profiles/default").unwrap();
    assert!(prof["devices"]["root"].get("size").is_none(), "{prof}");

    // A sandbox and stack replicas get their own size.
    let spec = isb::SandboxSpec::new("isb-test-disk-sb2", common::image());
    isb::Sandbox::create(&oc, &spec).unwrap();
    assert_eq!(root(&oc, "isb-test-disk-sb2")["size"], "10GiB");
    // A custom volume without a size gets one; one with a size keeps it.
    let pool = oc.get("/1.0/profiles/default").unwrap()["devices"]["root"]["pool"]
        .as_str()
        .unwrap()
        .to_string();
    for (v, cfg, want) in [
        ("isb-test-disk-vol", vec![], "10GiB"),
        ("isb-test-disk-vol2", vec![("size", "2GiB")], "2GiB"),
    ] {
        let cfg = cfg
            .into_iter()
            .map(|(k, v): (&str, &str)| (k.to_string(), v.to_string()))
            .collect();
        isb::volume::ensure(&oc, &pool, v, &cfg).unwrap();
        let got = oc
            .get(&format!("/1.0/storage-pools/{pool}/volumes/custom/{v}"))
            .unwrap();
        let _ = isb::volume::remove(&oc, &pool, v);
        assert_eq!(got["config"]["size"], want, "{got}");
    }

    let state = tempfile::tempdir().unwrap();
    let store = isb::stack::Store::open(state.path()).unwrap();
    let k = isb::secrets::Keyring::new(age::x25519::Identity::generate(), vec![]);
    let secrets = std::sync::Arc::new(isb::secrets::Secrets::new(isb::secrets::LocalDriver::new(
        state.path(),
        std::sync::Arc::new(k),
    )));
    let ctl = Controller::start(client.clone(), store, Duration::from_secs(2), secrets).unwrap();
    rm.3 = Some(ctl.clone());
    let name = "isb-test-disk";
    let d = def(&org, name, state.path());
    let revs = (d.revision("plain").unwrap(), d.revision("sized").unwrap());
    ctl.deploy(d).unwrap();
    let q = common::q(name);
    replicas_have_their_own_size(&ctl, &oc, &q, &revs);
    let prof = oc.get("/1.0/profiles/default").unwrap();
    assert!(prof["devices"]["root"].get("size").is_none(), "{prof}");

    // Lifting it leaves the instances' sizes alone.
    ctl.remove(&q, true, Duration::from_secs(120)).unwrap();
    let info = set_disk(&client, &org, None).unwrap();
    assert_eq!(info.disk, None);
    assert_eq!(root(&oc, "isb-test-disk-sb2")["size"], "10GiB");
    drop(rm);
}
