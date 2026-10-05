//! The registry images a stack deploy names, looked up before it is
//! accepted: a service whose image its registry does not have is refused
//! (deployed, it would only fail to pull, again and again). Images the
//! stack already runs are not asked about again.

use crate::error::{Error, Result};
use crate::image_check::{self, Probe};
use crate::stack::StackDef;

/// Check `def`'s new or changed images against their registries. Returns
/// the warnings for those that could not be checked.
pub(super) fn check(def: &StackDef, current: Option<&StackDef>) -> Result<Vec<String>> {
    check_with(def, current, &|i: &str| {
        image_check::probe(i, image_check::CHECK_TIMEOUT)
    })
}

fn check_with(
    def: &StackDef,
    current: Option<&StackDef>,
    probe: &(dyn Fn(&str) -> Probe + Sync),
) -> Result<Vec<String>> {
    let new: Vec<(&String, &String)> = def
        .file
        .services
        .iter()
        .map(|(name, s)| (name, &s.image))
        .filter(|(name, image)| {
            let before = current.and_then(|c| c.file.services.get(*name));
            before.is_none_or(|b| b.image != **image)
        })
        .collect();
    // One registry round trip at a time would add up: ask in parallel.
    let answers: Vec<(String, Result<Option<String>>)> = std::thread::scope(|s| {
        let handles: Vec<_> = new
            .iter()
            .map(|(name, image)| {
                s.spawn(move || ((*name).clone(), image_check::check(image, probe)))
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("image check thread"))
            .collect()
    });
    let mut refused = Vec::new();
    let mut warnings = Vec::new();
    for (name, a) in answers {
        match a {
            Ok(Some(w)) => warnings.push(format!("service {name}: {w}")),
            Ok(None) => {}
            Err(e) => refused.push(format!("service {name}: {}", error_text(&e))),
        }
    }
    if refused.is_empty() {
        Ok(warnings)
    } else {
        Err(Error::invalid(refused.join("; ")))
    }
}

fn error_text(e: &Error) -> String {
    match e {
        Error::Invalid(m) => m.clone(),
        e => e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn def(yaml: &str) -> StackDef {
        StackDef {
            name: "s".into(),
            org: crate::org::OrgId::default_org(),
            file: serde_yaml_ng::from_str(yaml).unwrap(),
            base_dir: "/tmp".into(),
            secrets: BTreeMap::new(),
            force: BTreeMap::new(),
            images: BTreeMap::new(),
            deployed_at: 0,
            deployed_by: "t".into(),
            previous: None,
        }
    }

    fn registry(i: &str) -> Probe {
        match i {
            "docker:traefik/whoami" | "docker:nginx:1.27" => Probe::Found(None),
            "ghcr:o/private:v1" => Probe::Denied("denied".into()),
            _ => Probe::NotFound("manifest unknown".into()),
        }
    }

    #[test]
    fn a_service_whose_image_is_missing_is_refused_by_name() {
        let d = def(
            "services:\n  web: {image: 'docker:traefik:whoami'}\n  db: {image: 'docker:nginx:1.27'}\n",
        );
        let e = check_with(&d, None, &registry).unwrap_err().to_string();
        assert!(
            e.contains("service web: image docker:traefik:whoami not found on Docker Hub (manifest unknown): did you mean docker:traefik/whoami?"),
            "{e}"
        );
        assert!(!e.contains("service db"), "{e}");
    }

    #[test]
    fn unchanged_images_are_not_asked_about_and_private_ones_warn() {
        let before = def("services:\n  web: {image: 'docker:gone:1'}\n");
        let d = def(
            "services:\n  web: {image: 'docker:gone:1'}\n  api: {image: 'ghcr:o/private:v1'}\n  local: {image: dev-base}\n",
        );
        let w = check_with(&d, Some(&before), &registry).unwrap();
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].starts_with("service api: ") && w[0].contains("without credentials"));
    }
}
