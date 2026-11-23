//! A deploy that changes nothing: a plain one replaces the instances all
//! the same (a restart the settings do not show), and one queued with
//! [`Apps::deploy_if_changed`] leaves them running, for a caller that
//! deploys the same definition again and again.

use super::{Apps, DeployLog, Deployment, Status, Trigger};
use crate::error::Result;
use crate::org::OrgId;

/// What a queued deployment was asked to do.
#[derive(Default)]
pub(super) struct Request {
    pub(super) requested: Option<String>,
    pub(super) rollback_of: Option<u64>,
    pub(super) if_changed: bool,
}

/// Why a deploy that changed nothing in the service must replace its
/// instances all the same, or `None` when they can stay. The settings
/// carry the image's digest when its registry gave one, so a moved tag is
/// a change of its own; what cannot be seen is a tag whose digest was not
/// resolved, this time or the last.
fn unseen_move(image: Option<&str>, digest: Option<&str>) -> Option<String> {
    let image = image?;
    if digest.is_some() {
        return None;
    }
    let (_, registry) = crate::image_check::remote(image)?;
    Some(format!(
        "{registry} gave no digest for {image}, so a moved tag would not show"
    ))
}

impl Deployment {
    /// A deployment as it is queued.
    pub(in crate::app) fn queued(id: u64, app: &str, trigger: Trigger, by: &str) -> Deployment {
        Deployment {
            id,
            app: app.into(),
            trigger,
            by: by.into(),
            status: Status::Queued,
            requested: None,
            rollback_of: None,
            commit: None,
            image: None,
            digest: None,
            error: None,
            created_at: crate::stack::controller::now_ms(),
            started_at: None,
            finished_at: None,
            rendered: None,
            if_changed: false,
            unchanged: false,
        }
    }
}

impl Apps {
    /// Queue a deploy that leaves the instances running when it changes
    /// nothing: the settings render to the service the stack already runs,
    /// and the image still resolves to the digest it runs. A plain
    /// [`Apps::deploy`] replaces them then. The record says which happened
    /// ([`Deployment::unchanged`]).
    pub fn deploy_if_changed(
        &self,
        org: &OrgId,
        name: &str,
        trigger: Trigger,
        by: &str,
    ) -> Result<Deployment> {
        let req = Request {
            if_changed: true,
            ..Default::default()
        };
        self.enqueue(org, name, trigger, by, req)
    }

    /// The stack already runs this service as rendered: replace its
    /// instances, or leave them when the deployment asked to and a moved
    /// tag would have shown.
    pub(super) fn unchanged(
        &self,
        q: &str,
        name: &str,
        dep: &mut Deployment,
        log: &mut DeployLog,
    ) -> Result<()> {
        let unseen = unseen_move(dep.image.as_deref(), dep.digest.as_deref());
        match (dep.if_changed, unseen) {
            (true, None) => {
                log.line("settings and image unchanged: the instances stay");
                dep.unchanged = true;
                return Ok(());
            }
            (true, Some(why)) => log.line(&format!(
                "settings unchanged, but {why}: replacing the instances"
            )),
            // Deploy means fresh instances, as in Dokploy: a moved tag or
            // a restart the settings do not show.
            (false, _) => log.line("settings unchanged: replacing the instances anyway"),
        }
        self.inner.ctl.redeploy(q, name)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use serde_json::json;

    use super::super::tests::apps;
    use super::*;
    use crate::app::AppSpec;

    #[test]
    fn an_unchanged_deploy_keeps_instances_only_when_a_moved_tag_would_show() {
        let d = Some("sha256:ab");
        // Pinned to a digest: a moved tag would have been a change.
        assert_eq!(unseen_move(Some("docker:nginx@sha256:ab"), d), None);
        // Built here, or rolled back: the digest is known.
        assert_eq!(unseen_move(Some("registry:web:abc"), d), None);
        // No registry to ask: compared by name.
        assert_eq!(unseen_move(Some("images:debian/12"), None), None);
        assert_eq!(unseen_move(Some("dev-base"), None), None);
        assert_eq!(unseen_move(None, None), None);
        // A registry image deployed by tag: the tag may have moved.
        let why = unseen_move(Some("docker:nginx:1.27"), None).unwrap();
        assert_eq!(
            why,
            "Docker Hub gave no digest for docker:nginx:1.27, so a moved tag would not show"
        );
        assert!(unseen_move(Some("ghcr:org/app:v1"), None).is_some());
    }

    #[test]
    fn if_changed_is_kept_on_the_record_and_out_of_plain_ones() {
        let dir = tempfile::tempdir().unwrap();
        let gate = Arc::new((Mutex::new(true), std::sync::Condvar::new()));
        let ap = apps(dir.path(), gate);
        let org = OrgId::new("acme").unwrap();
        ap.project_create(&org, "shop", "", &[]).unwrap();
        let spec: AppSpec = serde_json::from_value(json!({
            "name": "web", "project": "shop", "source": {"image": "docker:traefik/whoami"},
        }))
        .unwrap();
        ap.create(&org, spec).unwrap();
        let d = ap
            .deploy_if_changed(&org, "web", Trigger::Api, "t")
            .unwrap();
        assert!(d.if_changed && !d.unchanged);
        let d = ap.wait(&org, "web", d.id, Duration::from_secs(30)).unwrap();
        assert_eq!(d.summary()["if_changed"], json!(true));
        // No incusd here, so nothing was compared: never `unchanged`.
        assert!(d.summary().get("unchanged").is_none(), "{d:?}");
        let d = ap.deploy(&org, "web", Trigger::Api, "t", None).unwrap();
        assert!(d.summary().get("if_changed").is_none(), "{d:?}");
    }
}
