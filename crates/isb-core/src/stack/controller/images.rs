//! The digest each service's image is pinned to at deploy time
//! ([`StackDef::images`]): a tag that moved is a new revision, a tag that
//! did not is none, and a rollback runs exactly what ran before.
//!
//! A `registry:` image is resolved in the org's own registry. An image on
//! a remote registry (`docker:`, `ghcr:`, `quay:`, `oci:`) is asked about
//! there ([`crate::image_check::probe`]), every distinct one at once. One
//! written with a digest is already pinned, and an `images:` image or a
//! local alias has no registry to ask.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use super::super::StackDef;
use super::Controller;
use crate::error::{Error, Result};
use crate::image_check::{self, Probe};

impl Controller {
    /// Pin every image of `def` given by tag to the digest the tag names
    /// now. `old` is what the stack runs: where a remote registry gives no
    /// answer, a service whose image did not change keeps its pin.
    pub(super) fn pin_images(&self, def: &mut StackDef, old: Option<&StackDef>) -> Result<()> {
        let mut pins = remote_pins(def, old, &image_check::probe)?;
        for (svc, spec) in &def.file.services {
            let Some(r) = spec.image.strip_prefix("registry:") else {
                continue;
            };
            let r = crate::registry::ImageRef::parse(r)?;
            let reg = crate::registry::Registry::shared(&self.inner.client)?;
            // A digest is checked too, so a typo (or another org's digest)
            // fails the deploy rather than the rollout.
            let d = reg.resolve(&def.org, &r)?;
            if r.digest.is_none() {
                pins.insert(svc.clone(), d);
            }
        }
        def.images = pins;
        Ok(())
    }
}

/// Whether two definitions name the same image for every service.
pub(super) fn same_images(a: &StackDef, b: &StackDef) -> bool {
    let images = |d: &StackDef| -> Vec<(String, String)> {
        let s = &d.file.services;
        s.iter()
            .map(|(k, v)| (k.clone(), v.image.clone()))
            .collect()
    };
    images(a) == images(b)
}

/// The digest of every remote image `def` names by tag, by service.
///
/// A registry that gives no answer (unreachable, wants credentials, no
/// digest) leaves the service on the pin it ran with when its image is the
/// one `old` named, so an outage rolls nothing; with no earlier pin it goes
/// out by tag. An image the registry does not have is refused, unless the
/// stack already runs it from an earlier pin.
pub(super) fn remote_pins(
    def: &StackDef,
    old: Option<&StackDef>,
    probe: &(dyn Fn(&str, Duration) -> Probe + Sync),
) -> Result<BTreeMap<String, String>> {
    let wanted: BTreeSet<&str> = def
        .file
        .services
        .values()
        .map(|s| s.image.as_str())
        .filter(|i| !i.contains('@') && image_check::remote(i).is_some())
        .collect();
    let answers: BTreeMap<&str, Probe> = std::thread::scope(|s| {
        let asked: Vec<_> = wanted
            .iter()
            .map(|i| (*i, s.spawn(move || probe(i, image_check::DEPLOY_TIMEOUT))))
            .collect();
        asked
            .into_iter()
            .map(|(i, h)| {
                let p = h.join();
                (
                    i,
                    p.unwrap_or_else(|_| Probe::Unknown("the check failed".into())),
                )
            })
            .collect()
    });
    let earlier = |svc: &str, image: &str| -> Option<String> {
        let old = old?;
        let same = old.file.services.get(svc).is_some_and(|s| s.image == image);
        old.images.get(svc).filter(|_| same).cloned()
    };
    let mut pins = BTreeMap::new();
    for (svc, spec) in &def.file.services {
        let digest = match answers.get(spec.image.as_str()) {
            None => continue,
            Some(Probe::Found(Some(d))) => Some(d.clone()),
            Some(Probe::NotFound(why)) => match earlier(svc, &spec.image) {
                Some(d) => Some(d),
                None => {
                    let registry = image_check::remote(&spec.image).map(|r| r.1);
                    let registry = registry.unwrap_or_default();
                    return Err(Error::invalid(format!(
                        "service {svc}: {}",
                        image_check::not_found(&spec.image, &registry, why, None)
                    )));
                }
            },
            Some(_) => earlier(svc, &spec.image),
        };
        if let Some(d) = digest {
            pins.insert(svc.clone(), d);
        }
    }
    Ok(pins)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    fn def(y: &str) -> StackDef {
        StackDef {
            source: None,
            domains: Default::default(),
            name: "app".into(),
            org: crate::org::OrgId::default_org(),
            file: serde_yaml_ng::from_str(y).unwrap(),
            base_dir: "/".into(),
            secrets: BTreeMap::new(),
            force: BTreeMap::new(),
            images: BTreeMap::new(),
            deployed_at: 0,
            deployed_by: String::new(),
            previous: None,
        }
    }

    const D1: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
    const D2: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";
    const YAML: &str = concat!(
        "services:\n",
        "  web: {image: 'docker:nginx:1.27'}\n",
        "  api: {image: 'docker:nginx:1.27'}\n",
        "  own: {image: 'registry:app:v1'}\n",
        "  base: {image: 'images:debian/12'}\n",
        "  local: {image: dev-base}\n",
        "  fixed: {image: 'ghcr:org/app@sha256:abc'}\n",
    );

    #[test]
    fn remote_tags_are_pinned_once_each_and_nothing_else_is_asked_about() {
        let asked = Mutex::new(Vec::new());
        let probe = |i: &str, _: Duration| {
            asked.lock().unwrap().push(i.to_string());
            Probe::Found(Some(D1.into()))
        };
        let d = def(YAML);
        let pins = remote_pins(&d, None, &probe).unwrap();
        assert_eq!(*asked.lock().unwrap(), ["docker:nginx:1.27"]);
        let want: BTreeMap<String, String> = [
            ("api".to_string(), D1.to_string()),
            ("web".to_string(), D1.to_string()),
        ]
        .into();
        assert_eq!(pins, want);

        // The pin is what the instances get and what the revision carries.
        let mut pinned = d.clone();
        pinned.images = pins;
        assert_eq!(
            pinned.instance_image("web", "docker:nginx:1.27"),
            format!("docker:nginx@{D1}")
        );
        assert_eq!(
            pinned.instance_image("base", "images:debian/12"),
            "images:debian/12"
        );
        assert_ne!(pinned.revision("web").unwrap(), d.revision("web").unwrap());
        assert_eq!(
            pinned.revision("base").unwrap(),
            d.revision("base").unwrap()
        );
        // A moved tag is a new revision.
        let mut moved = pinned.clone();
        moved.images.insert("web".into(), D2.into());
        assert_ne!(
            moved.revision("web").unwrap(),
            pinned.revision("web").unwrap()
        );
        assert_eq!(
            moved.revision("api").unwrap(),
            pinned.revision("api").unwrap()
        );
    }

    #[test]
    fn a_registry_that_gives_no_answer_keeps_the_pin_a_service_ran_with() {
        let mut old = def(YAML);
        old.images.insert("web".into(), D1.into());
        let web: BTreeMap<String, String> = [("web".to_string(), D1.to_string())].into();
        for p in [
            Probe::Unknown("offline".into()),
            Probe::Denied("denied".into()),
            Probe::Found(None),
        ] {
            let probe = move |_: &str, _: Duration| p.clone();
            // `api` never had a pin: it goes out by tag.
            assert_eq!(remote_pins(&def(YAML), Some(&old), &probe).unwrap(), web);
        }
        // Gone from the registry: what the stack runs stays, what it never
        // ran is refused.
        let gone = |_: &str, _: Duration| Probe::NotFound("manifest unknown".into());
        let e = remote_pins(&def(YAML), Some(&old), &gone).unwrap_err();
        let e = e.to_string();
        assert!(
            e.contains("service api: image docker:nginx:1.27 not found"),
            "{e}"
        );
        old.images.insert("api".into(), D1.into());
        assert_eq!(remote_pins(&def(YAML), Some(&old), &gone).unwrap().len(), 2);
        old.images.remove("api");
        // A different image is not the one the pin was for.
        let mut new = def(YAML);
        new.file.services.get_mut("web").unwrap().image = "docker:nginx:1.28".into();
        let probe = |_: &str, _: Duration| Probe::Unknown("offline".into());
        assert!(remote_pins(&new, Some(&old), &probe).unwrap().is_empty());
        assert!(!same_images(&new, &old));
        assert!(same_images(&def(YAML), &old));
    }

    #[test]
    fn an_image_its_registry_lacks_is_refused() {
        let probe = |_: &str, _: Duration| Probe::NotFound("manifest unknown".into());
        let e = remote_pins(&def(YAML), None, &probe)
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("image docker:nginx:1.27 not found on Docker Hub (manifest unknown)"),
            "{e}"
        );
    }
}
