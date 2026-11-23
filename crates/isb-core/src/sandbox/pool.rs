//! Plain sandboxes on [`SANDBOX_POOL`] ([`EnsureOptions::sandbox_pool`]).
//!
//! A new sandbox whose spec leaves `storage` at `auto` gets its root disk
//! there, and so do its new `auto` volumes. A sandbox that already exists
//! keeps the pool its root disk is on, and a volume that already exists on
//! another pool (a shared cache, an external volume) is used where it is.
//!
//! [`EnsureOptions::sandbox_pool`]: super::EnsureOptions::sandbox_pool

use std::path::Path;

use crate::client::Client;
use crate::error::Result;
use crate::plan::{self, Desired, SANDBOX_POOL, VolumeDefs};
use crate::spec::SandboxSpec;

/// [`super::resolve`], on [`SANDBOX_POOL`] when `sandbox_pool` and the pool
/// exists.
pub fn resolve_for(
    client: &Client,
    spec: &SandboxSpec,
    defs: &VolumeDefs,
    base: &Path,
    sandbox_pool: bool,
) -> Result<Desired> {
    let mut host = super::host_facts(client)?;
    if !sandbox_pool || !host.pools.iter().any(|p| p == SANDBOX_POOL) {
        return plan::resolve(spec, defs, &host, base);
    }
    host.prefer = Some(match &spec.name {
        Some(n) => root_pool(client, n)?.unwrap_or_else(|| SANDBOX_POOL.into()),
        None => SANDBOX_POOL.into(),
    });
    let mut d = plan::resolve(spec, defs, &host, base)?;
    let here = d.pool.clone();
    for v in &mut d.volumes {
        if v.pool != here || super::volume_exists(client, &here, &v.name)? {
            continue;
        }
        let mut found = None;
        for p in host.pools.iter().filter(|p| **p != here) {
            if super::volume_exists(client, p, &v.name)? {
                found = Some(p.clone());
                break;
            }
        }
        let Some(pool) = found else { continue };
        for dev in d.devices.values_mut() {
            let props = &mut dev.props;
            if props.get("source") == Some(&v.name) && props.get("pool") == Some(&here) {
                props.insert("pool".into(), pool.clone());
            }
        }
        v.pool = pool;
    }
    Ok(d)
}

/// The pool an existing instance's root disk is on.
fn root_pool(client: &Client, name: &str) -> Result<Option<String>> {
    Ok(client.get_opt(&super::inst_path(name))?.and_then(|i| {
        i["expanded_devices"]["root"]["pool"]
            .as_str()
            .map(str::to_string)
    }))
}
