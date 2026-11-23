//! A stack's remote images against a real incusd (`ISB_INTEGRATION=1`;
//! build in a sandbox with `cargo test --no-run`, run the binary on the
//! host): a tag is pinned to the digest its registry names, the instances
//! run that digest, and deploying the same file again rolls nothing.
//! Deploys into the test org (`common::test_org`), removed afterwards,
//! pass or fail.

use std::time::Duration;

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

#[test]
fn a_remote_tag_is_pinned_to_its_digest_and_an_unmoved_one_rolls_nothing() {
    if !enabled() {
        return;
    }
    const IMAGE: &str = "docker:traefik/whoami";
    let isb::image_check::Probe::Found(Some(digest)) =
        isb::image_check::probe(IMAGE, Duration::from_secs(60))
    else {
        eprintln!("skipped: no digest for {IMAGE} (skopeo, or the registry)");
        return;
    };
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
    let stack = format!("isb-test-i{}", std::process::id() % 100000);
    let q = common::q(&stack);
    struct Rm(isb::stack::Controller, String);
    impl Drop for Rm {
        fn drop(&mut self) {
            let _ = self.0.remove(&self.1, true, Duration::from_secs(120));
            self.0.shutdown();
        }
    }
    let _rm = Rm(ctl.clone(), q.clone());
    let yaml = format!("services:\n  web:\n    image: {IMAGE}\n    labels: {{isb-test: '1'}}\n");
    let def = || {
        let p = isb::compose::load_docs(
            &[(state.path().join("isb.yaml"), yaml.clone())],
            state.path(),
            Some(&stack),
            &|_| None,
        )
        .unwrap();
        common::test_def(&stack, common::test_org(), p.file, state.path())
    };
    let instances = || -> Vec<String> {
        let st = isb::daemon::wait_settled(&ctl, &q, Duration::from_secs(300)).unwrap();
        assert!(st.converged, "{st:?}");
        st.services[0]
            .instances
            .iter()
            .map(|i| i.name.clone())
            .collect()
    };

    let changes = ctl.deploy(def()).unwrap();
    assert_eq!(changes[0].change, "create", "{changes:?}");
    let first = instances();
    let stored = ctl.definition(&q).unwrap();
    assert_eq!(stored.images.get("web"), Some(&digest));
    // The file keeps the tag; the instance was made from the digest.
    assert_eq!(stored.service("web").unwrap().image, IMAGE);
    let pinned = format!("docker:traefik/whoami@{digest}");
    assert_eq!(stored.instance_image("web", IMAGE), pinned);
    let oc = test_org_client(&client);
    let inst = oc.get(&format!("/1.0/instances/{}", first[0])).unwrap();
    let from = inst["config"]["volatile.base_image"].as_str().unwrap_or("");
    assert!(!from.is_empty(), "{inst}");

    // The same file again: the tag did not move, so nothing rolls.
    assert_eq!(ctl.plan(&def()).unwrap()[0].change, "unchanged");
    let changes = ctl.deploy(def()).unwrap();
    assert_eq!(changes[0].change, "unchanged", "{changes:?}");
    assert_eq!(instances(), first);

    // A forced redeploy asks again and replaces the instance, on the same digest.
    ctl.redeploy(&q, "web").unwrap();
    let second = instances();
    assert_ne!(second, first);
    assert_eq!(ctl.definition(&q).unwrap().images.get("web"), Some(&digest));
}
