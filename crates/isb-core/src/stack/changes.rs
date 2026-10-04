//! What deploying a stack changes, per service.

use super::StackDef;
use super::controller::DeployChange;
use crate::error::Result;

/// What deploying `new` over `old` changes, per service.
pub(super) fn diff(old: Option<&StackDef>, new: &StackDef) -> Result<Vec<DeployChange>> {
    let mut out = Vec::new();
    for (svc, spec) in &new.file.services {
        let rev = new.revision(svc)?;
        let replicas = spec.replicas();
        let change = match old.and_then(|o| o.file.services.get(svc).map(|s| (o, s))) {
            None => "create",
            Some((o, os)) => {
                if o.revision(svc)? != rev {
                    "update"
                } else if os.replicas() != replicas {
                    "scale"
                } else {
                    "unchanged"
                }
            }
        };
        out.push(DeployChange {
            service: svc.clone(),
            change: change.into(),
            rev,
            replicas,
        });
    }
    if let Some(o) = old {
        for svc in o.file.services.keys() {
            if !new.file.services.contains_key(svc) {
                out.push(DeployChange {
                    service: svc.clone(),
                    change: "remove".into(),
                    rev: String::new(),
                    replicas: 0,
                });
            }
        }
    }
    Ok(out)
}
