//! How the `secret_*` tools reach what uses a secret: the stacks (through
//! the controller, which cycles each service per its `on_change`) and the
//! workspaces (which get the new file and are never restarted).

use std::sync::Arc;

use super::{Daemon, secrets, workspaces};
use crate::error::Result;
use crate::server::Registry;

/// Register the `secret_*` tools, wired to this daemon's stacks and
/// workspaces.
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
    let (ctl, wsm, apps) = (d.ctl.clone(), d.workspaces.clone(), d.apps.clone());
    let changed: secrets::Changed = Arc::new(move |org: &crate::org::OrgId, name: &str| {
        let mut cycles = ctl.secret_changed(org, name);
        let mut skipped = workspace_secret_changed(&ctl, &wsm, org, name, true);
        // A database app's password moved: its URL secrets, which carry
        // it, follow, and so do the apps that use them.
        match apps.database_password_changed(org, name) {
            Ok(urls) => {
                for url in urls {
                    cycles.extend(ctl.secret_changed(org, &url));
                    skipped.extend(workspace_secret_changed(&ctl, &wsm, org, &url, true));
                }
            }
            Err(e) => ctl.note(
                "error",
                &crate::stack::qualified(org, "@secrets"),
                format!("secret {name}: the database URL secret was not updated: {e}"),
            ),
        }
        secrets::Outcome { cycles, skipped }
    });
    let (ctl, wsm, store) = (d.ctl.clone(), d.workspaces.clone(), d.secrets.clone());
    let refresh: secrets::Refresh = Arc::new(move |org: &crate::org::OrgId, name: &str| {
        let (mut found, cycles) = ctl.refresh_secret(org, name)?;
        // A reference only workspaces take: re-read it here, before it is
        // delivered.
        if found.is_empty() && name.contains('/') && wsm.uses(org, name) {
            let m = store.refresh(org, name)?;
            found.push((m.driver, m.version));
        }
        let skipped = workspace_secret_changed(&ctl, &wsm, org, name, false);
        Ok((found, secrets::Outcome { cycles, skipped }))
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

/// A secret workspaces take as a file was set (or refreshed): deliver it to
/// them without a restart, and report each as not cycled. With `event`, a
/// `secret.rotated` event per workspace (stack `<org>/@workspaces`).
fn workspace_secret_changed(
    ctl: &crate::stack::Controller,
    wsm: &workspaces::Workspaces,
    org: &crate::org::OrgId,
    name: &str,
    event: bool,
) -> Vec<secrets::Skipped> {
    let stack = crate::stack::qualified(org, "@workspaces");
    wsm.secret_changed(org, name)
        .into_iter()
        .map(|(w, reason)| {
            if event {
                ctl.event(
                    "secret.rotated",
                    "warn",
                    &stack,
                    &w,
                    format!("new value of secret {name}: workspace {w} {reason}"),
                );
            }
            secrets::Skipped {
                kind: "workspace".into(),
                name: w,
                reason,
            }
        })
        .collect()
}
