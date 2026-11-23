//! `isb down --volumes`: delete the named volumes a compose file makes.

use super::Project;
use crate::error::Result;
use crate::spec::MountType;

/// Delete every non-external named volume of `project`, on the pool each
/// would be on. A volume still in use is kept and reported.
pub(super) fn remove(
    c: &crate::Client,
    project: &Project,
    report: &mut dyn FnMut(&str),
) -> Result<()> {
    let facts = crate::sandbox::host_facts(c)?;
    let mut seen = std::collections::BTreeSet::new();
    // An `auto` volume of a new sandbox may be on the sandbox pool instead.
    let sandbox_pool = facts
        .pools
        .iter()
        .find(|p| *p == crate::plan::SANDBOX_POOL)
        .cloned();
    let vol_name = |key: &str| {
        project
            .file
            .volumes
            .get(key)
            .and_then(|d| d.name.clone())
            .unwrap_or_else(|| key.to_string())
    };
    for spec in project.file.services.values() {
        let pool = facts.pick_pool(spec.storage.as_deref())?;
        for v in &spec.volumes {
            if v.mount_type != MountType::Volume {
                continue;
            }
            let def = project.file.volumes.get(&v.source);
            if v.external || def.is_some_and(|d| d.external) {
                continue;
            }
            let vpool = match v.pool.as_deref().or(def.and_then(|d| d.pool.as_deref())) {
                Some(x) if x != "auto" => x.to_string(),
                _ => {
                    if let Some(sp) = &sandbox_pool {
                        seen.insert((sp.clone(), vol_name(&v.source)));
                    }
                    pool.clone()
                }
            };
            seen.insert((vpool, vol_name(&v.source)));
        }
    }
    for (key, def) in &project.file.volumes {
        let vname = vol_name(key);
        if !def.external && !seen.iter().any(|(_, n)| *n == vname) {
            if let (Some(sp), None | Some("auto")) = (&sandbox_pool, def.pool.as_deref()) {
                seen.insert((sp.clone(), vname.clone()));
            }
            seen.insert((facts.pick_pool(def.pool.as_deref())?, vname));
        }
    }
    for (pool, vname) in seen {
        match crate::volume::remove(c, &pool, &vname) {
            Ok(()) => report(&format!("volume {vname}: deleted")),
            Err(e) if e.is_not_found() => {}
            Err(e) => report(&format!("volume {vname}: kept ({e})")),
        }
    }
    Ok(())
}
