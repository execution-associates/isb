//! How the `secret_*` tools reach what uses a secret: the stacks, through
//! the controller, which cycles each service per its `on_change`.

use std::sync::Arc;

use super::{Daemon, secrets};
use crate::error::Result;
use crate::server::Registry;

/// Register the `secret_*` tools, wired to this daemon's stacks.
pub(super) fn register(r: &mut Registry, d: &Arc<Daemon>) -> Result<()> {
    let ctl = d.ctl.clone();
    let bindings: secrets::Bindings = Arc::new(move |org: &crate::org::OrgId| {
        let mut out = Vec::new();
        for def in ctl.definitions().iter().filter(|def| def.org == *org) {
            let used = crate::stack::secrets::used_keys(&def.file);
            for (_, b) in def.secrets.iter().filter(|(k, _)| used.contains(*k)) {
                out.push(secrets::Binding {
                    name: b.name.clone(),
                    driver: b.driver.clone(),
                    version: b.version,
                    stack: def.name.clone(),
                });
            }
        }
        out
    });
    let ctl = d.ctl.clone();
    let in_use: secrets::InUse = Arc::new(move |org: &crate::org::OrgId, name: &str| {
        ctl.definitions()
            .iter()
            .filter(|def| def.org == *org && def.store_secrets().contains(name))
            .map(|def| def.name.clone())
            .collect()
    });
    let ctl = d.ctl.clone();
    let prepare: secrets::Prepare =
        Arc::new(move |org: &crate::org::OrgId, name: &str, value: &[u8]| {
            ctl.rotate_before_set(org, name, value)
        });
    let (ctl, apps) = (d.ctl.clone(), d.apps.clone());
    let changed: secrets::Changed = Arc::new(move |org: &crate::org::OrgId, name: &str| {
        let mut cycles = ctl.secret_changed(org, name);
        // A database app's password moved: its URL secrets, which carry
        // it, follow, and so do the apps that use them.
        match apps.database_password_changed(org, name) {
            Ok(urls) => {
                for url in urls {
                    cycles.extend(ctl.secret_changed(org, &url));
                }
            }
            Err(e) => ctl.note(
                "error",
                &crate::stack::qualified(org, "@secrets"),
                format!("secret {name}: the database URL secret was not updated: {e}"),
            ),
        }
        secrets::Outcome { cycles }
    });
    let ctl = d.ctl.clone();
    let refresh: secrets::Refresh = Arc::new(move |org: &crate::org::OrgId, name: &str| {
        let (found, cycles) = ctl.refresh_secret(org, name)?;
        Ok((found, secrets::Outcome { cycles }))
    });
    secrets::register(
        r,
        d.secrets.clone(),
        secrets::Hooks {
            prepare,
            in_use,
            changed,
            refresh,
            bindings,
        },
    )?;
    Ok(())
}
