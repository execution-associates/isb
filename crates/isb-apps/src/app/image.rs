//! The image an app runs, looked up in its registry: when it is saved
//! (refused when the registry does not have it) and at each deploy (where
//! the answer is also the digest it is pinned to).

use super::deploy::DeployLog;
use super::{AppSpec, Apps, Source};
use crate::error::{Error, Result};
use crate::image_check::{self, Probe};

impl Apps {
    /// An image reference pinned to its current digest, when one is found.
    /// An image its registry does not have fails the deployment here,
    /// before the stack is touched: deployed, it would only crash-loop.
    pub(super) fn resolve(&self, i: &str, log: &mut DeployLog) -> Result<(String, Option<String>)> {
        let Some((_, registry)) = image_check::remote(i) else {
            log.line("not a registry image; deploying by name");
            return Ok((i.to_string(), None));
        };
        match (self.inner.probe)(i, image_check::DEPLOY_TIMEOUT) {
            Probe::Found(Some(d)) => {
                let pinned = super::pin(i, &d).unwrap_or_else(|| i.to_string());
                log.line(&format!("resolved to {pinned}"));
                Ok((pinned, Some(d)))
            }
            Probe::Found(None) => {
                log.line("no digest found; deploying by tag");
                Ok((i.to_string(), None))
            }
            Probe::NotFound(why) => {
                let better = image_check::suggestion(i).filter(|s| {
                    matches!(
                        (self.inner.probe)(s, image_check::CHECK_TIMEOUT),
                        Probe::Found(_)
                    )
                });
                Err(Error::invalid(image_check::not_found(
                    i, &registry, &why, better,
                )))
            }
            Probe::Denied(why) | Probe::Unknown(why) => {
                log.line(&format!(
                    "could not check the image on {registry} ({why}); deploying by tag"
                ));
                Ok((i.to_string(), None))
            }
        }
    }

    /// Check the remote image `spec` names (an image source, or a
    /// database's) before it is saved: `Err` when its registry does not
    /// have it, a warning when that could not be confirmed. With `before`
    /// (an update), an image that did not change is not asked about again.
    pub fn check_image(&self, before: Option<&AppSpec>, spec: &AppSpec) -> Result<Option<String>> {
        let Some(image) = image_of(spec) else {
            return Ok(None);
        };
        if before.and_then(image_of).as_deref() == Some(image.as_str()) {
            return Ok(None);
        }
        let probe = |i: &str| (self.inner.probe)(i, image_check::CHECK_TIMEOUT);
        image_check::check(&image, &probe)
    }
}

/// The image an app's settings name: its image source, or its database's.
pub(super) fn image_of(spec: &AppSpec) -> Option<String> {
    match &spec.source {
        Source::Image(i) => Some(i.clone()),
        Source::Database(db) => Some(db.image()),
        Source::Git(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use serde_json::json;

    use super::super::deploy::tests::apps;
    use super::super::deploy::{Status, Trigger};
    use crate::org::OrgId;

    fn spec(v: serde_json::Value) -> super::AppSpec {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn an_image_its_registry_lacks_is_refused_and_never_deployed() {
        let dir = tempfile::tempdir().unwrap();
        let gate = Arc::new((Mutex::new(true), std::sync::Condvar::new()));
        let ap = apps(dir.path(), gate);
        let org = OrgId::new("acme").unwrap();
        ap.project_create(&org, "shop", "", &[]).unwrap();
        let typo = spec(json!({
            "name": "web", "project": "shop", "source": {"image": "docker:traefik:whoami"},
        }));
        let e = ap.check_image(None, &typo).unwrap_err().to_string();
        assert!(e.contains("did you mean docker:traefik/whoami?"), "{e}");
        let fine = spec(json!({
            "name": "web", "project": "shop", "source": {"image": "docker:traefik/whoami"},
        }));
        assert_eq!(ap.check_image(None, &fine).unwrap(), None);
        // An update that keeps the image does not ask again.
        assert_eq!(ap.check_image(Some(&typo), &typo).unwrap(), None);

        // Saved some other way (before this check existed, or a restore),
        // its deploy fails at the image, before the stack is touched.
        ap.create(&org, typo).unwrap();
        ap.deploy(&org, "web", Trigger::Api, "t", None).unwrap();
        let d = ap.wait(&org, "web", 1, Duration::from_secs(30)).unwrap();
        assert_eq!(d.status, Status::Failed, "{d:?}");
        let err = d.error.clone().unwrap_or_default();
        assert!(
            err.contains("image docker:traefik:whoami not found on Docker Hub (manifest unknown)"),
            "{err}"
        );
        assert!(d.rendered.is_none(), "the stack was never deployed: {d:?}");
    }
}
