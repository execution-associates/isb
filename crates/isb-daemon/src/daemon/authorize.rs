//! Who may call which tool: the one authorizer every listener uses
//! ([`authorize_class`]), and the tool lists it judges by.

use serde_json::{Value, json};

use super::{accounts, audit, superadmin};
use crate::error::{Error, Result};
use crate::server::Caller;

/// Tools that reach across orgs: platform admins only.
pub(super) const PLATFORM_TOOLS: &[&str] = &[
    "server_status",
    "org_list",
    "org_create",
    "org_update",
    "org_delete",
    "registry_gc",
    "notification_settings",
    "template_catalog_add",
    "template_catalog_remove",
    "audit_verify",
    "server_add",
    "server_list",
    "server_show",
    "server_remove",
    "server_rotate_cert",
    "server_provision_get",
    "server_upgrade",
    "user_list",
    "user_update",
];

/// Read-only tools that span orgs: any signed-in user, filtered to their
/// orgs by the tool itself.
pub(super) const CROSS_ORG_READS: &[&str] = &["overview", "events", "stack_list", "ingress_status"];

/// Does `tools/list` show `tool`? An org-bound endpoint (`/orgs/<org>/mcp`,
/// `scope` set) is an org's: host, superadmin and platform tools are not on
/// it, for anyone but the unix socket. The unbound `/mcp` lists everything.
pub(super) fn tool_listed(c: &Caller, tool: &str, scope: Option<&crate::org::OrgId>) -> bool {
    scope.is_none()
        || c.is_local()
        || !(superadmin::TOOLS.contains(&tool) || PLATFORM_TOOLS.contains(&tool))
}

/// An audit row's `details`, saying `scope: org <org>` for a downscoped caller.
pub(super) fn scoped(mut details: Value, c: &Caller) -> Value {
    if let Some(org) = c.downscope() {
        details["scope"] = json!(format!("org {org}"));
    }
    details
}

/// The org a tool call names (`org`, default `default`).
pub(super) fn arg_org(args: &Value) -> Result<crate::org::OrgId> {
    match args.get("org").and_then(Value::as_str) {
        Some(o) => crate::org::OrgId::new(o),
        None => Ok(crate::org::OrgId::default_org()),
    }
}

/// May `c` call `tool` (of class `cls`) with `args`? The arguments to use
/// (an org-bound endpoint pins `org`), or the refusal. A token's scopes
/// narrow what its role allows; a viewer runs read-only tools only.
pub(super) fn authorize_class(
    c: &Caller,
    tool: &str,
    cls: audit::Class,
    mut args: Value,
    scope: Option<&crate::org::OrgId>,
    allow_anonymous: bool,
) -> Result<Value> {
    if let Some(org) = scope {
        match args.get("org").and_then(Value::as_str) {
            Some(o) if o != org.as_str() => {
                return Err(Error::Forbidden(format!(
                    "this endpoint acts in org {org}, not {o}"
                )));
            }
            _ => args["org"] = json!(org.as_str()),
        }
    }
    match c {
        Caller::Local { .. } | Caller::Superadmin(_) => Ok(args),
        _ if superadmin::TOOLS.contains(&tool) => Err(Error::Forbidden(format!(
            "{tool} is for superadmins (the unix socket, a superadmin token, or a listed tailnet or Access identity)"
        ))),
        Caller::Unauthenticated { .. } if allow_anonymous => Ok(args),
        Caller::Unauthenticated { .. } => Err(Error::Forbidden(
            "sign in: send an API token as Authorization: Bearer (isb token create)".into(),
        )),
        Caller::Access(id) => Err(Error::Forbidden(format!(
            "{} has no isb account; ask an org admin to invite you",
            id.name()
        ))),
        Caller::User { principal: p } => {
            if !audit::scope_allows(p.scopes(), tool, cls) {
                return Err(Error::Forbidden(format!(
                    "this token's scopes ({}) do not cover {tool}",
                    p.scopes().join(", ")
                )));
            }
            if accounts::TOOLS.contains(&tool) {
                // Judged per call by the account rules (crate::auth::ops),
                // as the identity endpoints are: not by a role in `org`.
                accounts::authorize(p, tool, cls)?;
                if PLATFORM_TOOLS.contains(&tool) && !p.platform_admin {
                    return Err(Error::Forbidden(format!("{tool} is for platform admins")));
                }
                return Ok(args);
            }
            if PLATFORM_TOOLS.contains(&tool) && !p.platform_admin {
                return Err(Error::Forbidden(format!("{} is for platform admins", tool)));
            }
            if tool == "secret_reencrypt"
                && args.get("all").and_then(Value::as_bool) == Some(true)
                && !p.platform_admin
            {
                return Err(Error::Forbidden(
                    "re-encrypting every org is for platform admins".into(),
                ));
            }
            if (CROSS_ORG_READS.contains(&tool) && scope.is_none())
                || tool == "audit_list"
                || tool == "history_query"
            {
                // They filter to what the caller may see themselves.
                return Ok(args);
            }
            let org = arg_org(&args)?;
            if p.platform_admin {
                return Ok(args);
            }
            match p.role_in(&org) {
                None => Err(Error::Forbidden(format!("no access to org {org}"))),
                Some(_) if cls.read_only => Ok(args),
                Some(_) if p.can_admin_org(&org) => Ok(args),
                Some(r) => Err(Error::Forbidden(format!(
                    "{tool} changes things or reads secrets; a {r} in org {org} only reads"
                ))),
            }
        }
    }
}
