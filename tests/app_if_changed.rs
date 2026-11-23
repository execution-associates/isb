//! `Apps::deploy_if_changed` against a real incusd (`ISB_INTEGRATION=1`;
//! build in a sandbox with `cargo test --no-run`, run the binary on the
//! host). Deploys into the test org (`common::test_org`), removed
//! afterwards, pass or fail.

#[allow(dead_code, reason = "each test binary uses some of the shared helpers")]
mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use common::enabled;
use isb::Client;
use isb::app::deploy::{Deployment, Status, Trigger};
use isb::image_check::Probe;

/// A secret store under `state` with a throwaway key.
fn test_secrets(state: &std::path::Path) -> std::sync::Arc<isb::secrets::Secrets> {
    let k = isb::secrets::Keyring::new(age::x25519::Identity::generate(), vec![]);
    std::sync::Arc::new(isb::secrets::Secrets::new(isb::secrets::LocalDriver::new(
        state,
        std::sync::Arc::new(k),
    )))
}

/// Apps on a controller of their own, with a registry that stops answering
/// when the flag is set, and the stack removed when the test ends.
struct Fixture {
    apps: isb::app::Apps,
    ctl: isb::stack::Controller,
    offline: Arc<AtomicBool>,
    project: String,
    _state: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Fixture {
        let client = Client::new();
        let state = tempfile::tempdir().unwrap();
        let store = isb::stack::Store::open(state.path()).unwrap();
        let secrets = test_secrets(state.path());
        let ctl = isb::stack::Controller::start(
            client.clone(),
            store,
            Duration::from_secs(2),
            secrets.clone(),
        )
        .unwrap();
        let offline = Arc::new(AtomicBool::new(false));
        let off = offline.clone();
        let probe: isb::app::ImageProbe = Arc::new(move |i: &str, t: Duration| {
            if off.load(Ordering::SeqCst) {
                Probe::Unknown("offline".into())
            } else {
                isb::image_check::probe(i, t)
            }
        });
        let apps = isb::app::Apps::new(state.path(), client, ctl.clone(), secrets)
            .with_probe(probe)
            .with_timeout(Duration::from_secs(400));
        Fixture {
            apps,
            ctl,
            offline,
            project: format!("isbk{}", std::process::id() % 100000),
            _state: state,
        }
    }

    fn stack(&self) -> String {
        common::q(&format!("{}-test", self.project))
    }

    /// Wait for a queued deployment to finish done; it and its log.
    fn done(&self, d: Deployment) -> (Deployment, String) {
        let org = common::test_org();
        let d = self
            .apps
            .wait(&org, "web", d.id, Duration::from_secs(600))
            .unwrap();
        let log = self.apps.log(&org, "web", d.id, 0).unwrap().0;
        assert_eq!(d.status, Status::Done, "{d:?}\n{log}");
        (d, log)
    }

    fn plain(&self) -> (Deployment, String) {
        let org = common::test_org();
        self.done(
            self.apps
                .deploy(&org, "web", Trigger::Api, "test", None)
                .unwrap(),
        )
    }

    fn if_changed(&self) -> (Deployment, String) {
        let org = common::test_org();
        self.done(
            self.apps
                .deploy_if_changed(&org, "web", Trigger::Api, "test")
                .unwrap(),
        )
    }

    fn instances(&self) -> Vec<String> {
        let st = self.ctl.status(&self.stack()).unwrap();
        let s = st.services.iter().find(|s| s.service == "web").unwrap();
        let mut v: Vec<String> = s.instances.iter().map(|i| i.name.clone()).collect();
        v.sort();
        v
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.apps.delete(&common::test_org(), "web");
        let _ = self
            .ctl
            .remove(&self.stack(), true, Duration::from_secs(120));
        self.ctl.shutdown();
    }
}

/// `deploy_if_changed` on incusd: a deploy that changes nothing leaves the
/// instances running, a plain one replaces them, and so does one whose
/// registry gave no digest (a moved tag would not show) or whose settings
/// changed.
#[test]
fn an_app_deploy_if_changed_keeps_unchanged_instances() {
    if !enabled() {
        return;
    }
    let f = Fixture::new();
    let org = common::test_org();
    f.apps
        .project_create(&org, &f.project, "", &["test".into()])
        .unwrap();
    let spec: isb::app::AppSpec = serde_json::from_value(serde_json::json!({
        "name": "web", "project": f.project, "environment": "test",
        "source": {"image": "docker:traefik/whoami"}, "env": "A=1\n",
    }))
    .unwrap();
    f.apps.create(&org, spec).unwrap();

    // The first deploy creates the service, asked to keep it or not.
    let (d1, _) = f.if_changed();
    assert!(!d1.unchanged, "{d1:?}");
    let first = f.instances();
    assert_eq!(first.len(), 1);
    let Some(digest) = d1.digest.clone() else {
        eprintln!("skipped: no digest for docker:traefik/whoami (skopeo, or the registry)");
        return;
    };

    // Nothing changed: the same instance, and the record says so.
    let (d2, log) = f.if_changed();
    assert!(d2.unchanged && d2.if_changed, "{d2:?}");
    assert_eq!(d2.digest.as_deref(), Some(digest.as_str()));
    assert!(
        log.contains("settings and image unchanged: the instances stay"),
        "{log}"
    );
    assert_eq!(f.instances(), first);
    assert_eq!(f.apps.get(&org, "web").unwrap().current, Some(d2.id));

    // A plain deploy replaces it all the same.
    let (d3, log) = f.plain();
    assert!(!d3.unchanged, "{d3:?}");
    assert!(log.contains("replacing the instances anyway"), "{log}");
    let second = f.instances();
    assert_ne!(second, first);

    // Changed settings roll, asked to keep or not.
    f.apps.env_set(&org, "web", "A=2\n").unwrap();
    let (d4, _) = f.if_changed();
    assert!(!d4.unchanged, "{d4:?}");
    let third = f.instances();
    assert_ne!(third, second);

    // The registry stops answering: the image goes out by tag (a change,
    // from the digest), and from then on nothing would show a moved tag,
    // so an unchanged deploy replaces the instances.
    f.offline.store(true, Ordering::SeqCst);
    let (d5, _) = f.if_changed();
    assert!(d5.digest.is_none() && !d5.unchanged, "{d5:?}");
    let fourth = f.instances();
    assert_ne!(fourth, third);
    let (d6, log) = f.if_changed();
    assert!(!d6.unchanged, "{d6:?}");
    assert!(
        log.contains("so a moved tag would not show: replacing the instances"),
        "{log}"
    );
    assert_ne!(f.instances(), fourth);
}
