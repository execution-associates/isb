//! `app_delete`, `project_delete`, `environment_delete` and what
//! `org_delete` empties: the deletes that take what runs inside along,
//! the monitors that follow it among them.

use serde::Deserialize;
use serde_json::{Value, json};

use super::super::{Daemon, args, obj};
use super::org_of;
use crate::error::{Error, Result};
use crate::monitor::auto::Target;
use crate::org::OrgId;
use crate::server::{Caller, Registry, Tool};

/// `app_delete`, `project_delete` and `environment_delete`. They live with
/// the daemon rather than over [`crate::app::Apps`] alone: they remove the
/// monitors that follow what they delete, with `force` the compose stacks
/// in the way too, and with `volumes` the stacks' named volumes, which take
/// the stack and volume tools' cleanup.
pub(in crate::daemon) fn delete_tools(r: &mut Registry, d: &std::sync::Arc<Daemon>) -> Result<()> {
    let destructive = json!({"destructiveHint": true, "openWorldHint": false});
    app_delete_tool(r, d, &destructive)?;
    let force = json!({"type": "boolean", "description": "Delete its apps and compose stacks too (default false)."});
    let volumes = json!({"type": "boolean", "description": "With force: delete their named volumes too, the data in them for good (default false: kept). A volume another stack still uses is kept. Org admins and owners."});
    let dry_run = json!({"type": "boolean", "description": "Only report what force (and volumes) would delete."});
    let keep_monitors = json!({"type": "boolean", "description": KEEP_MONITORS});
    tool!(
        r,
        d,
        "project_delete",
        "Delete a project",
        "Delete a project. Refused while it has apps or compose stacks, unless force=true: then every app in it is deleted (as app_delete) and every compose stack removed (as stack_remove), in every environment, before the project goes. Their named volumes are kept unless volumes=true. The monitors that follow its apps and compose stack services go too, unless keep_monitors=true. Returns what was deleted; dry_run=true only says what would be.",
        obj(
            json!({"name": {"type": "string"}, "force": force, "volumes": volumes, "keep_monitors": keep_monitors, "dry_run": dry_run}),
            &["name"]
        ),
        destructive,
        |d: &Daemon, a: Value, c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            let a: DeleteArgs = args(a)?;
            let name = a
                .name
                .clone()
                .ok_or_else(|| Error::invalid("name is required"))?;
            d.apps.project_get(&org, &name)?;
            let mut out = empty(d, c, &org, &name, None, &a)?;
            if !a.dry_run {
                d.apps.project_delete(&org, &name)?;
            }
            out["ok"] = json!(true);
            Ok(out)
        }
    );
    tool!(
        r,
        d,
        "environment_delete",
        "Delete an environment",
        "Remove an environment from a project. Refused while it has apps or compose stacks, unless force=true: then its apps are deleted (as app_delete) and its compose stacks removed (as stack_remove) first. Their named volumes are kept unless volumes=true. The monitors that follow its apps and compose stack services go too, unless keep_monitors=true. Returns the project and what was deleted; dry_run=true only says what would be.",
        obj(
            json!({"project": {"type": "string"}, "name": {"type": "string"}, "force": force, "volumes": volumes, "keep_monitors": keep_monitors, "dry_run": dry_run}),
            &["project", "name"]
        ),
        destructive,
        |d: &Daemon, a: Value, c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            let a: DeleteArgs = args(a)?;
            let project = a
                .project
                .clone()
                .ok_or_else(|| Error::invalid("project is required"))?;
            let name = a
                .name
                .clone()
                .ok_or_else(|| Error::invalid("name is required"))?;
            let p = d.apps.project_get(&org, &project)?;
            if !p.environments.contains(&name) {
                return Err(Error::NotFound(format!(
                    "environment {name} in project {project}"
                )));
            }
            let done = empty(d, c, &org, &project, Some(&name), &a)?;
            let mut out = if a.dry_run {
                json!(p)
            } else {
                json!(d.apps.environment_delete(&org, &project, &name)?)
            };
            for (k, v) in done.as_object().into_iter().flatten() {
                out[k] = v.clone();
            }
            Ok(out)
        }
    );
    Ok(())
}

/// `app_delete`: the app, then the monitors that follow it.
fn app_delete_tool(
    r: &mut Registry,
    d: &std::sync::Arc<Daemon>,
    destructive: &Value,
) -> Result<()> {
    let keep_monitors = json!({"type": "boolean", "description": KEEP_MONITORS});
    tool!(
        r,
        d,
        "app_delete",
        "Delete an app",
        "Delete an app: its service leaves the stack (the stack is removed with its last app), its deployments, checkout, webhook secret and deploy key go, and the monitors that follow it (its own and any app monitor made for it) unless keep_monitors=true. Named volumes are kept. Returns the monitors deleted.",
        obj(
            json!({"name": {"type": "string"}, "keep_monitors": keep_monitors}),
            &["name"]
        ),
        destructive,
        |d: &Daemon, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                name: String,
                #[serde(default)]
                keep_monitors: bool,
                #[serde(default)]
                #[allow(dead_code)]
                org: Option<String>,
            }
            let org = org_of(&a)?;
            let a: A = args(a)?;
            d.apps.delete(&org, &a.name)?;
            let gone = if a.keep_monitors {
                Vec::new()
            } else {
                d.monitors
                    .remove_following(&org, &[Target::App(a.name.clone())])?
            };
            Ok(json!({"ok": true, "monitors_deleted": gone}))
        }
    );
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteArgs {
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    force: bool,
    #[serde(default)]
    volumes: bool,
    #[serde(default)]
    keep_monitors: bool,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    #[allow(dead_code)]
    org: Option<String>,
}

/// With `force`, delete the apps and remove the compose stacks in a project,
/// or in one of its environments, then (with `volumes`) the named volumes
/// they leave that no other stack uses. Stops at the first app or stack that
/// fails (an app mid-deploy is refused), leaving the rest for another go; a
/// volume that cannot go is reported in `volumes_kept`. `dry_run` only
/// reports.
fn empty(
    d: &Daemon,
    c: &Caller,
    org: &OrgId,
    project: &str,
    env: Option<&str>,
    a: &DeleteArgs,
) -> Result<Value> {
    if a.volumes && !a.force {
        return Err(Error::invalid("volumes needs force"));
    }
    if a.volumes && !a.dry_run {
        super::super::volumes::require_admin(c, org, "deleting volumes")?;
    }
    let (apps, stacks) = if a.force || a.dry_run {
        d.apps.contents(org, project, env)?
    } else {
        Default::default()
    };
    // The environments' own stacks (their apps') and the compose stacks.
    let p = d.apps.project_get(org, project)?;
    let mut ours: Vec<String> = p
        .environments
        .iter()
        .filter(|e| env.is_none_or(|x| x == e.as_str()))
        .filter_map(|e| crate::app::stack_name(project, e).ok())
        .collect();
    ours.extend(stacks.iter().cloned());
    let defs = d.ctl.definitions();
    let mut volumes: Vec<String> = Vec::new();
    let mut shared: Vec<String> = Vec::new();
    for def in defs.iter().filter(|s| s.org == *org) {
        for v in named_volumes(def) {
            let list = if ours.contains(&def.name) {
                &mut volumes
            } else {
                &mut shared
            };
            if !list.contains(&v) {
                list.push(v);
            }
        }
    }
    let targets = following(d, org, &apps, &stacks);
    let monitors = if a.keep_monitors {
        Vec::new()
    } else {
        d.monitors.following(org, &targets)?
    };
    let (keep, volumes): (Vec<String>, Vec<String>) =
        volumes.into_iter().partition(|v| shared.contains(v));
    let mut kept: Vec<Value> = keep
        .iter()
        .map(|v| json!({"name": v, "reason": "another stack uses it"}))
        .collect();
    if a.dry_run {
        return Ok(json!({
            "dry_run": true, "apps": apps, "stacks": stacks, "volumes": volumes, "volumes_kept": kept,
            "monitors": monitors
        }));
    }
    for app in &apps {
        d.apps.delete(org, app)?;
    }
    for s in &stacks {
        super::super::tools::remove_stack(d, &crate::stack::qualified(org, s), false)?;
    }
    let monitors = if a.keep_monitors {
        Vec::new()
    } else {
        d.monitors.remove_following(org, &targets)?
    };
    let mut deleted = Vec::new();
    if a.volumes {
        for v in volumes {
            match d.volumes.delete(org, &v) {
                Ok(()) => deleted.push(v),
                Err(e) if e.is_not_found() => {}
                Err(e) => kept.push(json!({"name": v, "reason": e.to_string()})),
            }
        }
    }
    Ok(json!({
        "apps_deleted": apps, "stacks_removed": stacks, "volumes_deleted": deleted, "volumes_kept": kept,
        "monitors_deleted": monitors
    }))
}

/// What `keep_monitors` says, on every delete that takes monitors along.
pub(in crate::daemon) const KEEP_MONITORS: &str = "Keep the monitors that follow what is deleted (default false: they are deleted with their history). Kept, they fail until removed.";

/// What monitors may follow: the apps, and every service of the compose
/// stacks (read before the stacks go).
fn following(d: &Daemon, org: &OrgId, apps: &[String], stacks: &[String]) -> Vec<Target> {
    let mut out: Vec<Target> = apps.iter().cloned().map(Target::App).collect();
    for s in stacks {
        out.extend(stack_targets(d, &crate::stack::qualified(org, s)));
    }
    out
}

/// Every service of a stack, as monitors follow them.
pub(in crate::daemon) fn stack_targets(d: &Daemon, q: &str) -> Vec<Target> {
    d.ctl
        .definition(q)
        .map(|def| {
            def.file
                .services
                .keys()
                .map(|svc| Target::Service {
                    stack: def.name.clone(),
                    service: svc.clone(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Everything an org runs, for `org_delete` with force: its apps, then its
/// stacks, then its project records. Volumes are left to the org's incus
/// project, whose deletion takes them.
pub(in crate::daemon) fn empty_org(d: &Daemon, org: &OrgId, notes: &mut Vec<String>) -> Result<()> {
    for a in d.apps.list(org)? {
        notes.push(format!("{org}: deleting app {}", a.spec.name));
        d.apps.delete(org, &a.spec.name)?;
    }
    for def in d.ctl.definitions().iter().filter(|s| s.org == *org) {
        notes.push(format!("{org}: removing stack {}", def.name));
        super::super::tools::remove_stack(d, &def.qualified(), false)?;
    }
    for p in d.apps.project_list(org)? {
        d.apps.project_delete(org, &p.name)?;
    }
    Ok(())
}

/// The named volumes a stack's services mount and isb made (not `external`),
/// by incus name, as the controller resolves them when it removes a stack.
fn named_volumes(def: &crate::stack::StackDef) -> Vec<String> {
    let mut out = Vec::new();
    for spec in def.file.services.values() {
        for v in &spec.volumes {
            if v.mount_type != crate::spec::MountType::Volume || v.external {
                continue;
            }
            let top = def.file.volumes.get(&v.source);
            if top.is_some_and(|t| t.external) {
                continue;
            }
            let name = top
                .and_then(|t| t.name.clone())
                .unwrap_or_else(|| v.source.clone());
            if !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}
