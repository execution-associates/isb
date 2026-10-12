//! Every tool `isb serve` offers, and the skills beside them, in one
//! registry.

use std::sync::Arc;

use serde_json::json;

use super::tools::{self, Ann};
use super::{
    Daemon, accounts, apps, audit, builds, data, guide, host_monitor, monitors, notify, orgs,
    previews, sandboxes, secret_hooks, ssh, superadmin, templates, volumes,
};
use crate::error::Result;
use crate::server::{Registry, Skill};

/// Build the tool registry.
pub(super) fn registry(d: Arc<Daemon>, skills: &[Skill]) -> Result<Registry> {
    let mut r = Registry::new()
        .instructions(guide::INSTRUCTIONS)
        .skills(skills.iter().cloned())?;
    superadmin::register(&mut r, d.clone())?;
    host_monitor::register(&mut r, d.clone())?;
    let ann = Ann {
        ro: json!({"readOnlyHint": true, "openWorldHint": false}),
        destructive: json!({"destructiveHint": true, "openWorldHint": false}),
        write: json!({"destructiveHint": false, "openWorldHint": false}),
    };

    guide::register(&mut r, &d, &ann)?;
    tools::stack_deploy_tool(&mut r, &d, &ann)?;
    tools::overview_tool(&mut r, &d, &ann)?;
    tools::events_tool(&mut r, &d, &ann)?;
    tools::ingress_status_tool(&mut r, &d, &ann)?;
    tools::stack_list_tool(&mut r, &d, &ann)?;
    tools::stack_status_tool(&mut r, &d, &ann)?;
    tools::stack_config_tool(&mut r, &d, &ann)?;
    tools::stack_logs_tool(&mut r, &d, &ann)?;
    tools::stack_scale_tool(&mut r, &d, &ann)?;
    tools::stack_edit_tools(&mut r, &d, &ann)?;
    tools::sandbox_create_tool(&mut r, &d, &ann)?;
    secret_hooks::register(&mut r, &d)?;
    builds::register(
        &mut r,
        builds::Ctx {
            client: d.client.clone(),
            policy: d.policy.clone(),
            ctl: d.ctl.clone(),
        },
    )?;
    tools::sandbox_tools(&mut r, &d, &ann)?;
    sandboxes::register(&mut r, d.clone())?;
    apps::register(&mut r, d.apps.clone(), d.ingress.is_some())?;
    apps::delete_tools(&mut r, &d)?;
    previews::register(&mut r, d.apps.clone())?;
    let mut t = templates::Templates::new(
        &d.state_dir,
        d.apps.clone(),
        d.secrets.clone(),
        d.ingress.as_ref().and_then(|m| m.public_ip()),
    );
    t.catalogs = d.catalogs.clone();
    templates::register(&mut r, t)?;
    data::register(&mut r, d.data.clone())?;
    volumes::register(&mut r, d.volumes.clone())?;
    tools::server_status_tool(&mut r, &d, &ann)?;
    orgs::register(&mut r, d.clone())?;
    notify::register(
        &mut r,
        d.notifier.clone(),
        d.history.clone(),
        d.apps.clone(),
    )?;
    monitors::register(&mut r, d.monitors.clone())?;
    audit::register(&mut r, d.audit.clone())?;
    audit::register_history(&mut r, d.audit.clone())?;
    ssh::register(&mut r, d.clone())?;
    accounts::register(&mut r, d.clone())?;
    Ok(r)
}
