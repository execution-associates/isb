//! Registry images against a real incusd and Docker Hub: an app naming an
//! image its registry lacks is refused with the likely fix, and a stack
//! deployed with one anyway reports it plainly and backs off.
//!
//! Gated like the other integration tests (`ISB_INTEGRATION=1`; build them
//! in a sandbox with `cargo test --no-run`, run the binary on the host). It
//! needs skopeo and network access to Docker Hub, and deploys into the
//! test org (`isb-test`, never the default org); its stack is removed
//! afterwards, pass or fail.

#[allow(dead_code, reason = "each test binary uses some of the shared helpers")]
mod common;

use std::time::Duration;

use isb::Client;

use common::enabled;

fn test_secrets(state: &std::path::Path) -> std::sync::Arc<isb::secrets::Secrets> {
    let k = isb::secrets::Keyring::new(age::x25519::Identity::generate(), vec![]);
    std::sync::Arc::new(isb::secrets::Secrets::new(isb::secrets::LocalDriver::new(
        state,
        std::sync::Arc::new(k),
    )))
}

/// A registry image that does not exist, asked about for real (skopeo on
/// the host, Docker Hub): an app naming it is refused with the likely fix,
/// and a stack that deploys it anyway says so plainly and backs off for
/// minutes, not seconds.
#[test]
fn missing_images_are_refused_and_reported() {
    if !enabled() {
        return;
    }
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
    let apps = isb::app::Apps::new(state.path(), client.clone(), ctl.clone(), secrets);
    let spec = |image: &str| {
        serde_json::from_value::<isb::app::AppSpec>(serde_json::json!({
            "name": "web", "project": "shop", "source": {"image": image},
        }))
        .unwrap()
    };
    let e = apps
        .check_image(None, &spec("docker:traefik:whoami"))
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("image docker:traefik:whoami not found on Docker Hub (manifest unknown): did you mean docker:traefik/whoami?"),
        "{e}"
    );
    assert_eq!(
        apps.check_image(None, &spec("docker:traefik/whoami"))
            .unwrap(),
        None
    );

    // The stack runs in the test org; this makes its project.
    common::test_org_client(&client);
    let stack = format!("isb-test-img-{}", std::process::id() % 100000);
    let p = isb::compose::load_docs(
        &[(
            state.path().join("isb.yaml"),
            "services:\n  web:\n    image: docker:traefik:whoami\n    labels: {isb-test: '1'}\n"
                .to_string(),
        )],
        state.path(),
        Some(&stack),
        &|_| None,
    )
    .unwrap();
    let def = isb::stack::StackDef {
        name: stack.clone(),
        org: common::test_org(),
        file: p.file,
        base_dir: state.path().to_path_buf(),
        secrets: Default::default(),
        force: Default::default(),
        images: Default::default(),
        deployed_at: 0,
        deployed_by: "test".into(),
        source: None,
        domains: Default::default(),
        previous: None,
    };
    struct Rm(isb::stack::Controller, String);
    impl Drop for Rm {
        fn drop(&mut self) {
            let _ = self.0.remove(&self.1, true, Duration::from_secs(120));
            self.0.shutdown();
        }
    }
    let _rm = Rm(ctl.clone(), common::q(&stack));
    ctl.deploy(def).unwrap();
    let st = isb::daemon::wait_settled(&ctl, &common::q(&stack), Duration::from_secs(180)).unwrap();
    let web = &st.services[0];
    assert_eq!(web.state, "failing", "{web:?}");
    let m = web.message.as_deref().unwrap_or("");
    assert!(
        m.starts_with("image docker:traefik:whoami not found (manifest unknown): change the image and deploy again (retrying in 5m)"),
        "{m}"
    );
}
