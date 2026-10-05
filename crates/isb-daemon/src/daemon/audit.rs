//! The daemon's side of the audit log ([`crate::audit`]): which calls are
//! recorded and what of them is kept, the tool classes that viewers and
//! token scopes are judged by, the `audit_list` and `audit_verify` tools, the
//! live tail (`GET /api/v1/audit/stream`) and webhook deliveries.
//!
//! Recording happens at the dispatch hook ([`crate::server::mcp::Audit`]),
//! so every tool is covered, whoever registered it.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::audit::{Actor, AuditLog, NewEntry, Origin, Query, Visibility};
use crate::auth::{AuthStore, PrincipalKind};
use crate::error::{Error, Result};
use crate::server::http::{Request, Response};
use crate::server::mcp::{Audited, glob_match};
use crate::server::{Caller, Registry, Tool};

/// Read-only tools that hand out secret material. Always recorded, refused
/// to viewers and to `read`/`deploy` tokens. A tool can also say so itself
/// with the annotation `"isbSecretRead": true`.
pub const SECRET_READS: &[&str] = &["secret_get", "secret_resolve", "app_webhook"];

/// What the `deploy` scope adds to `read`.
pub const DEPLOY_TOOLS: &[&str] = &[
    "stack_deploy",
    "stack_redeploy",
    "stack_rollback",
    "stack_scale",
    "app_deploy",
    "app_rollback",
    "preview_redeploy",
    "build_run",
    "sandbox_start",
    "sandbox_stop",
];

/// Argument keys whose scalar values may be kept: names and identifiers,
/// never values. Anything else (`value`, `password`, `env`, `vars`,
/// `compose`, `secrets`, `argv`, `stdin`, URLs) is dropped.
const SAFE_KEYS: &[&str] = &[
    "org",
    "name",
    "app",
    "stack",
    "project",
    "environment",
    "service",
    "slot",
    "replica",
    "replicas",
    "version",
    "driver",
    "role",
    "id",
    "user_id",
    "email",
    "user",
    "platform_admin",
    "all_orgs",
    "deployment",
    "builder",
    "tag",
    "rotate",
    "volumes",
    "all",
    "wait",
    "dry_run",
    "duration_s",
    "kind",
    "provider",
    "event",
    "status",
    "scopes_count",
    "ssh_user",
    "fingerprint",
    "volume",
    "snapshot",
    "backup",
    "instance",
    "stamp",
    "port",
    "device",
    "allow_nesting",
];

/// The target, in order of preference.
const TARGET_KEYS: &[&str] = &[
    "name",
    "app",
    "stack",
    "project",
    "service",
    "environment",
    "id",
    "user_id",
    "email",
];

/// How a tool is judged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Class {
    /// Only reads, and no secret material.
    pub read_only: bool,
    /// Hands out secret material.
    pub secret_read: bool,
}

pub fn class(tool: &Tool) -> Class {
    let ann = |k: &str| {
        tool.annotations
            .as_ref()
            .and_then(|a| a.get(k))
            .and_then(Value::as_bool)
            == Some(true)
    };
    let secret_read = SECRET_READS.contains(&tool.name.as_str()) || ann("isbSecretRead");
    Class {
        read_only: ann("readOnlyHint") && !secret_read,
        secret_read,
    }
}

/// One call's class: [`class`], made a secret read when the tool names a
/// boolean argument (annotation `isbSecretReadArg`) that the call sets,
/// such as `database_get`'s `reveal`. Viewers and `read` tokens then get the
/// tool without the values, and every reveal is recorded.
pub fn class_for(tool: &Tool, args: &Value) -> Class {
    let c = class(tool);
    let arg = tool
        .annotations
        .as_ref()
        .and_then(|a| a.get("isbSecretReadArg"))
        .and_then(Value::as_str);
    match arg {
        Some(k) if args.get(k).and_then(Value::as_bool) == Some(true) => Class {
            read_only: false,
            secret_read: true,
        },
        _ => c,
    }
}

/// May a token with `scopes` call `tool`? Empty scopes: anything the role allows.
pub fn scope_allows(scopes: &[String], tool: &str, c: Class) -> bool {
    scopes.is_empty()
        || scopes.iter().any(|s| match s.as_str() {
            "admin" => true,
            "read" => c.read_only,
            "deploy" => c.read_only || DEPLOY_TOOLS.contains(&tool) || super::kube::deploys(tool),
            s => s.strip_prefix("tool:").is_some_and(|g| glob_match(g, tool)),
        })
}

/// Who a caller is, for a row.
pub fn actor(c: &Caller) -> Actor {
    match c {
        Caller::Local { uid } => Actor::local(*uid),
        Caller::Access(id) => {
            let mut a = Actor::anonymous(format!("access:{}", id.name()));
            a.email = id.email.clone();
            a
        }
        Caller::Unauthenticated { .. } => Actor::anonymous("anonymous"),
        Caller::Superadmin(s) => Actor::from_principal(&s.principal),
        Caller::User { principal } => {
            let mut a = Actor::from_principal(principal);
            if principal.kind == PrincipalKind::Access {
                a.name = format!("access:{}", principal.user.email);
            }
            a
        }
    }
}

/// The scalar values of whitelisted keys, each at most 128 characters.
pub fn safe_details(args: &Value) -> Value {
    let mut out = Map::new();
    if let Some(o) = args.as_object() {
        for k in SAFE_KEYS {
            let v = match o.get(*k) {
                Some(Value::String(s))
                    if !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control) =>
                {
                    json!(s)
                }
                Some(v @ (Value::Number(_) | Value::Bool(_))) => v.clone(),
                _ => continue,
            };
            out.insert((*k).to_string(), v);
        }
    }
    Value::Object(out)
}

fn target(details: &Value) -> Option<String> {
    TARGET_KEYS.iter().find_map(|k| match details.get(*k)? {
        Value::String(s) => Some(s.clone()),
        v @ Value::Number(_) => Some(v.to_string()),
        _ => None,
    })
}

/// `ok`, or the error's code.
pub fn outcome(r: std::result::Result<(), &Error>) -> String {
    match r {
        Ok(()) => "ok".into(),
        Err(e) => crate::rpc::error_json(e)["code"]
            .as_str()
            .unwrap_or("error")
            .to_string(),
    }
}

/// The row's org: the one a call acts in; `None` for org-less calls.
fn row_org(action: &str, args: &Value) -> Option<String> {
    let named = args.get("org").and_then(Value::as_str);
    if super::PLATFORM_TOOLS.contains(&action)
        || super::CROSS_ORG_READS.contains(&action)
        || super::superadmin::TOOLS.contains(&action)
        || super::accounts::USER_TOOLS.contains(&action)
        || action.starts_with("superadmin.")
    {
        return named.and_then(|o| crate::org::OrgId::new(o).ok().map(|o| o.to_string()));
    }
    super::arg_org(args).ok().map(|o| o.to_string())
}

/// The row for a call, or `None` when it is not worth recording: reads are
/// skipped unless `record_all`, but secret reads and refusals never are.
pub fn entry(a: &Audited, record_all: bool) -> Option<NewEntry> {
    let terminal = a.action.starts_with("terminal.") || a.action.starts_with("ssh.");
    let cls = a.tool.map(|t| class_for(t, a.args)).unwrap_or_default();
    let refused = matches!(a.outcome, Err(Error::Forbidden(_)));
    // A superadmin over HTTP is recorded whatever it does, reads included.
    let superadmin = matches!(a.caller, Caller::Superadmin(_)) || a.caller.downscope().is_some();
    if !(terminal || !cls.read_only || record_all || refused || superadmin) {
        return None;
    }
    let details = super::kube::details(a);
    // The org tools act on the org they name.
    let target = if a.action.starts_with("org_") {
        details.get("org").and_then(Value::as_str).map(String::from)
    } else {
        target(&details)
    };
    Some(NewEntry {
        org: row_org(a.action, a.args),
        actor: actor(a.caller),
        origin: a.origin.clone(),
        action: a.action.to_string(),
        target,
        details,
        outcome: outcome(a.outcome),
    })
}

/// The dispatch hook: append, and never fail the call.
pub fn hook(log: Arc<AuditLog>, record_all: bool) -> crate::server::mcp::Audit {
    Arc::new(move |a: &Audited| {
        if let Some(e) = entry(a, record_all) {
            if let Err(err) = log.append(e) {
                eprintln!("isb serve: {err}");
            }
        }
    })
}

/// What a caller may read of the log, for `audit_list` and the tail:
/// platform admins and local callers everything; an org's owners and admins
/// that org; nobody else anything.
pub fn visibility(c: &Caller, org: Option<&str>) -> Result<Visibility> {
    match c {
        Caller::Local { .. } | Caller::Superadmin(_) => Ok(Visibility::All),
        Caller::User { principal: p } if p.platform_admin => Ok(Visibility::All),
        Caller::User { principal: p } => {
            let Some(o) = org else {
                // Without an org: every org this principal manages.
                let orgs: Vec<String> = p
                    .orgs
                    .iter()
                    .filter(|(o, _)| p.can_manage_members(o))
                    .map(|(o, _)| o.to_string())
                    .collect();
                if orgs.is_empty() {
                    return Err(Error::Forbidden(
                        "the audit log is for org owners and admins".into(),
                    ));
                }
                return Ok(Visibility::Orgs(orgs));
            };
            let o = crate::org::OrgId::new(o)?;
            if p.can_manage_members(&o) {
                Ok(Visibility::Orgs(vec![o.to_string()]))
            } else {
                Err(Error::Forbidden(format!(
                    "org {o}'s audit log is for its owners and admins"
                )))
            }
        }
        _ => Err(Error::Forbidden("sign in to read the audit log".into())),
    }
}

/// `audit_list` and `audit_verify`.
pub fn register(r: &mut Registry, log: Arc<AuditLog>) -> Result<()> {
    let l = log.clone();
    r.register(
        Tool::new(
            "audit_list",
            "Who did what: audit log entries, newest first (or oldest first after `after`, for tailing). Org owners and admins see their org's entries; platform admins see every org and platform-level entries (sign-ins, users, org changes). Filters: actor (glob on name or email), action (glob: `secret_*`, `auth.*`), target (glob), outcome (`ok`, `error`, or a code), surface, since/until (unix ms). Page with `before` = the last id you got.",
            json!({
                "type": "object",
                "properties": {
                    "org": {"type": "string", "description": "One org's entries (an org admin's own when omitted)."},
                    "platform": {"type": "boolean", "description": "Only platform-level entries (platform admins)."},
                    "actor": {"type": "string"},
                    "user_id": {"type": "integer"},
                    "token_id": {"type": "integer"},
                    "action": {"type": "string"},
                    "target": {"type": "string"},
                    "outcome": {"type": "string"},
                    "surface": {"type": "string", "enum": ["mcp", "rest", "cli", "web", "webhook"]},
                    "since": {"type": "integer", "description": "Unix milliseconds, inclusive."},
                    "until": {"type": "integer", "description": "Unix milliseconds, exclusive."},
                    "before": {"type": "integer", "description": "Entries older than this id."},
                    "after": {"type": "integer", "description": "Entries newer than this id, oldest first."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 1000, "description": "Default 100."}
                },
                "additionalProperties": false
            }),
            move |a: Value, c: &Caller| -> Result<Value> {
                let q: Query = serde_json::from_value(a)
                    .map_err(|e| Error::invalid(format!("bad arguments: {e}")))?;
                let vis = visibility(c, q.org.as_deref())?;
                if q.platform && vis != Visibility::All {
                    return Err(Error::Forbidden(
                        "platform-level entries are for platform admins".into(),
                    ));
                }
                let entries = l.list(&q, &vis)?;
                let limit = q.limit.unwrap_or(100).clamp(1, 1000);
                let next = (entries.len() == limit && q.after.is_none())
                    .then(|| entries.last().map(|e| e.id))
                    .flatten();
                Ok(json!({"entries": entries, "next_before": next, "head": l.head()?}))
            },
        )
        .title("Audit log")
        .annotations(json!({"readOnlyHint": true, "openWorldHint": false})),
    )?;
    r.register(
        Tool::new(
            "audit_verify",
            "Walk the audit log's hash chain: ok, how many rows, the head (id and hash: keep a copy elsewhere to pin the log), and the first row that does not check out. Platform admins.",
            json!({"type": "object", "properties": {}, "additionalProperties": false}),
            move |_a: Value, _c: &Caller| -> Result<Value> {
                let a = log.verify()?;
                let h = log.history_verify()?;
                Ok(json!({"ok": a.ok && h.ok, "audit": a, "history": h}))
            },
        )
        .title("Verify the audit log")
        .annotations(json!({"readOnlyHint": true, "openWorldHint": false})),
    )?;
    Ok(())
}

/// What of the history a caller may read: platform admins and local
/// callers everything; anyone else the orgs they belong to (any role),
/// never host-level rows.
pub fn history_visibility(c: &Caller, org: Option<&str>) -> Result<Visibility> {
    match c {
        Caller::Local { .. } | Caller::Superadmin(_) => Ok(Visibility::All),
        Caller::User { principal: p } if p.platform_admin => Ok(Visibility::All),
        Caller::User { principal: p } => match org {
            Some(o) => {
                let o = crate::org::OrgId::new(o)?;
                if p.role_in(&o).is_some() {
                    Ok(Visibility::Orgs(vec![o.to_string()]))
                } else {
                    Err(Error::Forbidden(format!("no access to org {o}")))
                }
            }
            None => Ok(Visibility::Orgs(
                p.orgs.iter().map(|(o, _)| o.to_string()).collect(),
            )),
        },
        _ => Err(Error::Forbidden("sign in to read the history".into())),
    }
}

/// `history_query`.
pub fn register_history(r: &mut Registry, log: Arc<AuditLog>) -> Result<()> {
    r.register(
        Tool::new(
            "history_query",
            "What happened, merged into one timeline: the stack controller's events (deploys, rollouts, health, restarts), incus lifecycle events in every project including changes made outside isb (instance-created/deleted, image-alias-deleted, ...) with who requested them, audit rows (tool calls, sign-ins), and markers for when isb was not watching (serve.started, serve.stopped, incus.gap). Newest first, or oldest first with ascending=true; page with `before` = the `next` you got. `object` finds everything about an instance, image, volume, stack, app or service (substring, or exact=true). correlate=true links incus instance events to the audit row that likely caused them (inferred, by time and name). Org members see their orgs; host-level rows (images, pools, other projects) are for platform admins; audit rows for org owners and admins.",
            json!({
                "type": "object",
                "properties": {
                    "org": {"type": "string"},
                    "platform": {"type": "boolean", "description": "Only host-level rows (platform admins)."},
                    "object": {"type": "string"},
                    "exact": {"type": "boolean"},
                    "kind": {"type": "string", "description": "Glob on the kind or audit action: instance-*, deploy.*, secret_*."},
                    "source": {"type": "string", "description": "audit, controller, incus, marker; comma-separated. Default all."},
                    "actor": {"type": "string", "description": "Glob on who."},
                    "since": {"type": "integer", "description": "Unix milliseconds, inclusive."},
                    "until": {"type": "integer", "description": "Unix milliseconds, exclusive."},
                    "before": {"type": "string", "description": "The `next` of the previous page."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 1000},
                    "ascending": {"type": "boolean"},
                    "correlate": {"type": "boolean"}
                },
                "additionalProperties": false
            }),
            move |a: Value, c: &Caller| -> Result<Value> {
                let q: crate::history::HistoryQuery = serde_json::from_value(a)
                    .map_err(|e| Error::invalid(format!("bad arguments: {e}")))?;
                let hvis = history_visibility(c, q.org.as_deref())?;
                if q.platform && hvis != Visibility::All {
                    return Err(Error::Forbidden(
                        "host-level history is for platform admins".into(),
                    ));
                }
                let avis = visibility(c, q.org.as_deref()).ok();
                Ok(serde_json::to_value(log.timeline(&q, &hvis, avis.as_ref())?)?)
            },
        )
        .title("History")
        .annotations(json!({"readOnlyHint": true, "openWorldHint": false})),
    )
}

fn param(req: &Request, key: &str) -> Option<String> {
    req.query.as_deref()?.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        (k == key).then(|| v.to_string())
    })
}

/// `GET /api/v1/audit/stream?org=ORG&after=ID`: new entries as server-sent
/// events (`event: audit`), only those the caller may read. Signed in with a session or a token.
pub fn stream_route(log: Arc<AuditLog>, users: Arc<AuthStore>) -> crate::server::Routes {
    Arc::new(move |req: &Request| {
        if req.path == "/api/v1/history/stream" {
            return Some(history_stream(req, &log, &users));
        }
        if req.path != "/api/v1/audit/stream" {
            return None;
        }
        if req.method != "GET" {
            return Some(Response::text(405, "method not allowed").header("Allow", "GET"));
        }
        let err = |status: u16, code: &str, m: &str| {
            Response::json(status, &json!({"error": code, "message": m}))
        };
        let Some(p) = users.principal_from_request(req) else {
            return Some(err(401, "unauthenticated", "sign in first"));
        };
        let caller = Caller::User {
            principal: Arc::new(p),
        };
        let org = param(req, "org").filter(|o| !o.is_empty());
        let vis = match visibility(&caller, org.as_deref()) {
            Ok(v) => v,
            Err(e) => return Some(err(403, "forbidden", &e.to_string())),
        };
        let mut q = Query {
            org,
            ..Default::default()
        };
        let start = req
            .header("last-event-id")
            .map(String::from)
            .or_else(|| param(req, "after"))
            .and_then(|s| s.parse::<i64>().ok());
        let log = log.clone();
        let mut after = match start {
            Some(a) => a,
            None => log.head().unwrap_or(0),
        };
        let mut seen = log.generation();
        Some(Response::stream(
            200,
            "text/event-stream",
            Box::new(move |w: &mut dyn std::io::Write| {
                loop {
                    q.after = Some(after);
                    q.limit = Some(200);
                    let rows = log.list(&q, &vis).map_err(std::io::Error::other)?;
                    for e in &rows {
                        let data = serde_json::to_string(e).unwrap_or_default();
                        write!(w, "id: {}\nevent: audit\ndata: {data}\n\n", e.id)?;
                        after = e.id;
                    }
                    if rows.is_empty() {
                        w.write_all(b": keepalive\n\n")?;
                    }
                    w.flush()?;
                    if rows.len() < 200 {
                        seen = log.wait_change(seen, Duration::from_secs(15));
                    }
                }
            }),
        ))
    })
}

/// `GET /api/v1/history/stream?org=ORG&after=AUDIT.HISTORY`: new timeline
/// items (`event: history`), only what the caller may read; the event id is
/// the cursor to resume from.
fn history_stream(req: &Request, log: &Arc<AuditLog>, users: &Arc<AuthStore>) -> Response {
    use crate::history::Item;
    if req.method != "GET" {
        return Response::text(405, "method not allowed").header("Allow", "GET");
    }
    let err = |status: u16, code: &str, m: &str| {
        Response::json(status, &json!({"error": code, "message": m}))
    };
    let Some(p) = users.principal_from_request(req) else {
        return err(401, "unauthenticated", "sign in first");
    };
    let caller = Caller::User {
        principal: Arc::new(p),
    };
    let org = param(req, "org").filter(|o| !o.is_empty());
    let hvis = match history_visibility(&caller, org.as_deref()) {
        Ok(v) => v,
        Err(e) => return err(403, "forbidden", &e.to_string()),
    };
    let avis = visibility(&caller, org.as_deref()).ok();
    let start = req
        .header("last-event-id")
        .map(String::from)
        .or_else(|| param(req, "after"))
        .and_then(|s| {
            let (a, h) = s.split_once('.')?;
            Some((a.parse::<i64>().ok()?, h.parse::<i64>().ok()?))
        });
    let (mut a_after, mut h_after) = match start {
        Some(x) => x,
        None => (log.head().unwrap_or(0), log.history_head().unwrap_or(0)),
    };
    let log = log.clone();
    let mut seen = log.generation();
    Response::stream(
        200,
        "text/event-stream",
        Box::new(move |w: &mut dyn std::io::Write| {
            loop {
                let mut items: Vec<Item> = Vec::new();
                let rows = log
                    .history_after(h_after, &hvis, 200)
                    .map_err(std::io::Error::other)?;
                if let Some(l) = rows.last() {
                    h_after = l.id;
                }
                items.extend(
                    rows.into_iter()
                        .filter(|r| org.is_none() || r.org == org)
                        .map(Item::from_record),
                );
                if let Some(av) = &avis {
                    let q = Query {
                        org: org.clone(),
                        after: Some(a_after),
                        limit: Some(200),
                        ..Default::default()
                    };
                    let rows = log.list(&q, av).map_err(std::io::Error::other)?;
                    if let Some(l) = rows.last() {
                        a_after = l.id;
                    }
                    items.extend(rows.into_iter().map(Item::from_audit));
                }
                items.sort_by_key(|i| i.time);
                for it in &items {
                    let data = serde_json::to_string(it).unwrap_or_default();
                    write!(
                        w,
                        "id: {a_after}.{h_after}\nevent: history\ndata: {data}\n\n"
                    )?;
                }
                if items.is_empty() {
                    w.write_all(b": keepalive\n\n")?;
                }
                w.flush()?;
                seen = log.wait_change(seen, Duration::from_secs(15));
            }
        }),
    )
}

/// Records every delivery to `/api/v1/webhooks/<org>/<app>` (actor
/// `webhook:<provider>`): deployed, ignored or refused.
pub fn audited_webhooks(inner: crate::server::Routes, log: Arc<AuditLog>) -> crate::server::Routes {
    Arc::new(move |req: &Request| {
        let r = inner(req)?;
        let Some((org, app)) = req
            .path
            .strip_prefix("/api/v1/webhooks/")
            .and_then(|p| p.split_once('/'))
        else {
            return Some(r);
        };
        if req.method != "POST" || r.status == 404 && crate::org::OrgId::new(org).is_err() {
            return Some(r);
        }
        let h = |n: &str| req.header(n).is_some();
        let provider = if h("x-hub-signature-256") {
            "github"
        } else if h("x-gitea-signature") || h("x-forgejo-signature") {
            "gitea"
        } else if h("x-gitlab-token") {
            "gitlab"
        } else {
            "token"
        };
        let body: Value = serde_json::from_slice(&r.body).unwrap_or(Value::Null);
        let outcome = match r.status {
            202 => "ok",
            200 if body.get("ignored").is_some() => "ignored",
            200 => "ok",
            401 => "unauthorized",
            404 => "not_found",
            400 => "invalid",
            _ => "error",
        };
        let event = req
            .header("x-github-event")
            .or_else(|| req.header("x-gitea-event"))
            .or_else(|| req.header("x-forgejo-event"))
            .or_else(|| req.header("x-gitlab-event"));
        let mut args = json!({"org": org, "app": app, "status": r.status});
        if let Some(e) = event {
            args["event"] = json!(e);
        }
        if let Some(d) = body.get("deployment") {
            args["deployment"] = d.clone();
        }
        let details = safe_details(&args);
        let e = NewEntry {
            org: crate::org::OrgId::new(org).ok().map(|o| o.to_string()),
            actor: Actor::webhook(provider),
            origin: Origin {
                surface: "webhook".into(),
                ip: crate::auth::http::client_ip(req),
                user_agent: req.header("user-agent").map(String::from),
                request_id: req
                    .header("x-github-delivery")
                    .or_else(|| req.header("x-gitea-delivery"))
                    .or_else(|| req.header("x-gitlab-event-uuid"))
                    .map(String::from),
            },
            action: "webhook.deploy".into(),
            target: Some(app.to_string()),
            details,
            outcome: outcome.into(),
        };
        if let Err(err) = log.append(e) {
            eprintln!("isb serve: {err}");
        }
        Some(r)
    })
}

#[cfg(test)]
#[path = "audit_tests.rs"]
mod tests;
