//! Closing deployments that will never run: a cancelled one, and those a
//! stopped daemon left unfinished.

use super::deploy::{Apps, Status};
use crate::error::Result;
use crate::org::OrgId;

impl Apps {
    /// Close a deployment that has not started as `cancelled`, with
    /// `reason`. Returns whether it was cancelled (false when it already
    /// started or finished).
    pub fn cancel_queued(&self, org: &OrgId, app: &str, id: u64, reason: &str) -> Result<bool> {
        if let Some(q) = self
            .inner
            .queues
            .lock()
            .unwrap()
            .get_mut(&(org.clone(), app.to_string()))
        {
            if q.next == Some(id) {
                q.next = None;
            }
        }
        let g = self.inner.edit.lock().unwrap();
        let mut d = self.deployment(org, app, id)?;
        if d.status != Status::Queued {
            return Ok(false);
        }
        d.advance(Status::Cancelled)?;
        d.error = Some(reason.to_string());
        self.save_dep(org, &d)?;
        drop(g);
        self.event(
            org,
            app,
            "info",
            format!("deployment {id} cancelled: {reason}"),
        );
        Ok(true)
    }

    /// Mark deployments a stopped daemon left unfinished as failed.
    pub(super) fn recover(&self) {
        let mut orgs = vec![OrgId::default_org()];
        if let Ok(rd) = std::fs::read_dir(self.inner.state.join("orgs")) {
            for e in rd.flatten() {
                if let Some(o) = e.file_name().to_str().and_then(|s| OrgId::new(s).ok()) {
                    orgs.push(o);
                }
            }
        }
        for org in orgs {
            for app in self.list(&org).unwrap_or_default() {
                for mut d in self.deployments(&org, &app.spec.name).unwrap_or_default() {
                    if d.status.finished() {
                        continue;
                    }
                    d.error = Some("interrupted: the daemon stopped during it".into());
                    d.status = Status::Failed;
                    d.finished_at = Some(crate::stack::controller::now_ms());
                    let _ = self.save_dep(&org, &d);
                }
                self.recover_previews(&org, &app.spec.name);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant};

    use serde_json::json;

    use super::*;
    use crate::app::deploy::Trigger;
    use crate::client::Client;
    use crate::stack::Controller;

    /// Apps over `dir`; `docker:slow` holds its deploy in `building` until
    /// the gate opens.
    fn apps(dir: &std::path::Path, gate: Arc<(Mutex<bool>, Condvar)>) -> Apps {
        let k = crate::secrets::Keyring::new(age::x25519::Identity::generate(), vec![]);
        let secrets = Arc::new(crate::secrets::Secrets::new(
            crate::secrets::LocalDriver::new(dir, Arc::new(k)),
        ));
        let client = Client::with_socket("/nonexistent/isb-test/incus.sock");
        let store = crate::stack::Store::open(dir).unwrap();
        let ctl = Controller::start(
            client.clone(),
            store,
            Duration::from_secs(60),
            secrets.clone(),
        )
        .unwrap();
        Apps::new(dir, client, ctl, secrets).with_digest(Arc::new(move |_: &str| {
            let (m, cv) = &*gate;
            let mut open = m.lock().unwrap();
            while !*open {
                open = cv.wait(open).unwrap();
            }
            None
        }))
    }

    #[test]
    fn a_queued_deployment_is_cancelled_and_a_restart_closes_unfinished_ones() {
        let dir = tempfile::tempdir().unwrap();
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let ap = apps(dir.path(), gate.clone());
        let org = OrgId::new("acme").unwrap();
        ap.project_create(&org, "shop", "", &[]).unwrap();
        let spec = json!({"name": "slow", "project": "shop", "source": {"image": "docker:slow"}});
        ap.create(&org, serde_json::from_value(spec).unwrap())
            .unwrap();
        let d1 = ap.deploy(&org, "slow", Trigger::Api, "t", None).unwrap();
        let started = Instant::now();
        while ap.deployment(&org, "slow", d1.id).unwrap().status != Status::Building {
            assert!(started.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(20));
        }
        let d2 = ap
            .deploy(&org, "slow", Trigger::Api, "t", Some("template".into()))
            .unwrap();
        // Closed with the reason and never run; a running one is left alone.
        let why = "template deploy stopped: db failed";
        assert!(ap.cancel_queued(&org, "slow", d2.id, why).unwrap());
        assert!(!ap.cancel_queued(&org, "slow", d1.id, why).unwrap());
        let d = ap.deployment(&org, "slow", d2.id).unwrap();
        assert_eq!(d.status, Status::Cancelled);
        assert_eq!(d.error.as_deref(), Some(why));
        assert!(d.status.finished() && !d.status.can_become(Status::Queued));
        // A daemon starting over the same state closes what was unfinished.
        let d3 = ap.deploy(&org, "slow", Trigger::Api, "t", None).unwrap();
        let open = Arc::new((Mutex::new(true), Condvar::new()));
        let ap2 = apps(dir.path(), open);
        for id in [d1.id, d3.id] {
            let d = ap2.deployment(&org, "slow", id).unwrap();
            assert_eq!(d.status, Status::Failed, "{d:?}");
            assert!(d.error.unwrap().contains("interrupted"));
        }
        let d = ap2.deployment(&org, "slow", d2.id).unwrap();
        assert_eq!(d.status, Status::Cancelled);
        let (m, cv) = &*gate;
        *m.lock().unwrap() = true;
        cv.notify_all();
        let _ = ap.wait(&org, "slow", d3.id, Duration::from_secs(10));
    }
}
