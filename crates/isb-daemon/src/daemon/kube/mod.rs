//! `kubectl` for an isb org: the tools that look into and act on the
//! instances isb manages (docs/guides/kubectl.md).
//!
//! `instance_list` and `instance_get` are `get pods` and `describe pod`;
//! `app_exec`, `stack_exec` and `instance_exec` are `exec`; `app_logs` is `logs`;
//! `app_restart` and `instance_restart` are `rollout restart` and `delete pod`;
//! `app_scale`, `app_top` and `app_events` are `scale`, `top` and `get
//! events`; `instance_file_read` and `instance_file_write` are `cp`.
//!
//! They are ordinary registry tools, so the one authorizer judges them as it
//! does every other (members and up change things, viewers read, a token's
//! scopes narrow that), `--deny-tools` applies, the audit hook records them,
//! and a control plane forwards them to the server an org lives on.
//!
//! [`exec`] runs commands, [`files`] copies files, [`look`] describes and
//! reads, [`act`] restarts and scales.

mod act;
mod exec;
mod files;
mod look;

use super::*;
use crate::org::OrgId;
use crate::stack::controller::ServiceStatus;

#[cfg(test)]
use self::{exec::*, files::*, look::*};
pub(super) use look::{SINCE_NOTE, read_lines, since_cutoff, window_logs};

/// Tools that only read.
pub(super) const READS: &[&str] = &[
    "instance_list",
    "instance_get",
    "app_logs",
    "app_top",
    "app_events",
];
/// Tools that read what may be secret: refused to viewers and to `read` and
/// `deploy` tokens, and always recorded.
pub(super) const SECRET_READS: &[&str] = &["instance_file_read"];
/// Tools that change things or run code.
pub(super) const WRITES: &[&str] = &[
    "app_exec",
    "stack_exec",
    "instance_exec",
    "app_restart",
    "instance_restart",
    "app_scale",
    "instance_file_write",
];

/// How a tool is judged, as the registry annotates it.
pub(super) fn class_of(tool: &str) -> Option<audit::Class> {
    if SECRET_READS.contains(&tool) {
        Some(audit::Class {
            read_only: false,
            secret_read: true,
        })
    } else if READS.contains(&tool) {
        Some(audit::Class {
            read_only: true,
            secret_read: false,
        })
    } else if WRITES.contains(&tool) {
        Some(audit::Class::default())
    } else {
        None
    }
}

fn annotations(tool: &str, ann: &Ann) -> Value {
    match class_of(tool) {
        Some(c) if c.secret_read => {
            json!({"readOnlyHint": true, "isbSecretRead": true, "openWorldHint": false})
        }
        Some(c) if c.read_only => ann.ro.clone(),
        _ => ann.write.clone(),
    }
}

/// What an instance is to isb: `app`, `database`, `stack` (a compose
/// service's replica), `tunnel`, `workspace`, `build` or `sandbox`.
/// `app_db` is `Some(is_database)` when an app owns it.
pub(super) fn kind(
    labels: &BTreeMap<String, String>,
    tunnel: bool,
    app_db: Option<bool>,
) -> &'static str {
    match crate::workspace::kind_of(labels) {
        "replica" if tunnel => "tunnel",
        "replica" => match app_db {
            Some(true) => "database",
            Some(false) => "app",
            None => "stack",
        },
        k => k,
    }
}

/// What the audit row of one of these calls keeps besides the usual: the
/// argv, a file's path and size, the names (not values) of the environment,
/// the size (not the text) of stdin. Contents never.
pub(super) fn audit_details(action: &str, args: &Value) -> Option<Value> {
    if !WRITES.contains(&action) && !SECRET_READS.contains(&action) {
        return None;
    }
    let mut out = serde_json::Map::new();
    let clip = |s: &str| -> String { s.chars().take(256).collect() };
    if let Some(argv) = args.get("argv").and_then(Value::as_array) {
        let v: Vec<String> = argv
            .iter()
            .filter_map(Value::as_str)
            .take(64)
            .map(clip)
            .collect();
        out.insert("argv".into(), json!(v));
    }
    if let Some(p) = args.get("path").and_then(Value::as_str) {
        out.insert("path".into(), json!(clip(p)));
    }
    if let Some(s) = args.get("stdin").and_then(Value::as_str) {
        out.insert("stdin_bytes".into(), json!(s.len()));
    }
    if let Some(c) = args.get("content").and_then(Value::as_str) {
        out.insert("bytes".into(), json!(content_len(c, args)));
    }
    if let Some(env) = args.get("env").and_then(Value::as_object) {
        let keys: Vec<&String> = env.keys().take(64).collect();
        out.insert("env_keys".into(), json!(keys));
    }
    Some(Value::Object(out))
}

fn content_len(content: &str, args: &Value) -> usize {
    if args.get("encoding").and_then(Value::as_str) == Some("base64") {
        crate::rpc::b64_decode(content)
            .map(|b| b.len())
            .unwrap_or(content.len())
    } else {
        content.len()
    }
}

/// An app's stack, qualified, and its service's status.
fn app_service(
    d: &Daemon,
    org: &OrgId,
    app: &str,
) -> Result<(crate::app::App, String, ServiceStatus)> {
    let a = d.apps.get(org, app)?;
    let stack = crate::stack::qualified(org, &a.spec.stack()?);
    let st = d
        .ctl
        .status(&stack)
        .map_err(|_| Error::NotFound(format!("{app} is not deployed")))?;
    let svc = st
        .services
        .into_iter()
        .find(|s| s.service == app)
        .ok_or_else(|| Error::NotFound(format!("{app} is not deployed")))?;
    Ok((a, stack, svc))
}

/// The files isb delivers into an app's replicas: app files and secrets.
fn managed_files(
    d: &Daemon,
    stack: &str,
    service: &str,
    app: Option<&crate::app::App>,
) -> Vec<String> {
    let mut out: Vec<String> = app
        .map(|a| a.spec.files.iter().map(|f| f.path.clone()).collect())
        .unwrap_or_default();
    if let Ok(def) = d.ctl.definition(stack) {
        if let Ok(spec) = def.service(service) {
            out.extend(spec.secrets.iter().map(|s| s.guest_path()));
            out.extend(
                spec.env
                    .files
                    .values()
                    .map(|k| crate::spec::Environment::file_path(k)),
            );
        }
    }
    out
}

fn labels_of(info: &SandboxInfo) -> BTreeMap<String, String> {
    info.config
        .iter()
        .filter_map(|(k, v)| k.strip_prefix("user.").map(|k| (k.to_string(), v.clone())))
        .collect()
}

/// Restarting and scaling are what the `deploy` token scope allows of these.
pub(super) fn deploys(tool: &str) -> bool {
    ["app_scale", "app_restart", "instance_restart"].contains(&tool)
}

/// An audit row's `details`: the safe arguments, then what these tools add
/// (see [`audit_details`]), and the scope of a downscoped caller.
pub(super) fn details(a: &crate::server::mcp::Audited) -> Value {
    // app_apply's details (the app named in its document) come from the manifest helper.
    let mut out = super::apps::manifest::kept(a.action, a.args);
    if let (Some(extra), Some(o)) = (audit_details(a.action, a.args), out.as_object_mut()) {
        o.extend(extra.as_object().cloned().unwrap_or_default());
    }
    // A read says so, for the web UI's activity feed: a read is evidence, not news.
    let read = a
        .tool
        .is_some_and(|t| super::audit::class_for(t, a.args).read_only);
    if let (true, Some(o)) = (read, out.as_object_mut()) {
        o.insert("read_only".into(), json!(true));
    }
    super::authorize::scoped(out, a.caller)
}

/// The `kubectl`-shaped tools.
pub(super) fn register(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    look::register(r, d, ann)?;
    exec::register(r, d, ann)?;
    act::register(r, d, ann)?;
    files::register(r, d, ann)
}

#[cfg(test)]
mod tests;
