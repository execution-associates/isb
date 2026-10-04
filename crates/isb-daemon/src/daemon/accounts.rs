//! The account tools: who is calling (`whoami`), an org's members,
//! invitations and tokens, the caller's own API tokens, SSH keys and
//! sessions, and (platform admins) users. The same operations as the
//! identity endpoints under `/api/v1/auth/` ([`crate::auth::ops`]), so MCP
//! and REST callers get the web UI's account pages with the same rules:
//!
//! - A workspace token reaches none of them but `whoami`.
//! - A token scoped short of `admin` only reads them.
//! - A token never mints tokens (`token_create` needs a browser session, an
//!   Access or tailnet identity; the host CLI mints too).
//! - Org rules (members see their org; owners and admins manage it; only an
//!   owner touches an owner) are [`crate::auth::ops`]'s.
//!
//! The control plane runs them itself (the identity store is its own); an
//! agent refuses them.

use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{Daemon, args, audit};
use crate::auth::{AuthError, Principal, Role, Superadmin, SuperadminSource, ops};
use crate::error::{Error, Result};
use crate::org::OrgId;
use crate::server::{Caller, Registry, Tool};

/// Every account tool.
pub const TOOLS: &[&str] = &[
    "whoami",
    "member_list",
    "member_update",
    "member_remove",
    "invitation_list",
    "invitation_create",
    "invitation_revoke",
    "agent_identity_list",
    "agent_identity_set",
    "agent_identity_remove",
    "token_list",
    "token_create",
    "token_revoke",
    "ssh_key_list",
    "ssh_key_add",
    "ssh_key_remove",
    "session_list",
    "session_revoke",
    "user_list",
    "user_update",
];

/// The ones about the caller (or the platform), not one org: no org role
/// is needed to call them, and their audit rows name no org unless the
/// call did.
pub const USER_TOOLS: &[&str] = &[
    "whoami",
    "token_list",
    "token_revoke",
    "ssh_key_list",
    "ssh_key_add",
    "ssh_key_remove",
    "session_list",
    "session_revoke",
    "user_list",
    "user_update",
];

/// The authorizer's part for a signed-in user calling an account tool,
/// after its scopes: a workspace only asks `whoami`, and a token scoped
/// short of `admin` only reads. The rest is judged per call, by
/// [`crate::auth::ops`].
pub fn authorize(p: &Principal, tool: &str, cls: audit::Class) -> Result<()> {
    if tool != "whoami" {
        ops::account_holder(p).map_err(err)?;
    }
    if !cls.read_only {
        ops::may_change_accounts(p).map_err(err)?;
    }
    Ok(())
}

/// The identity store's refusals as tool errors, with the same codes as
/// the endpoints (`forbidden`, `not_found`, ...).
fn err(e: AuthError) -> Error {
    match e {
        AuthError::Forbidden(m) => Error::Forbidden(m),
        AuthError::NotFound(m) => Error::NotFound(m),
        AuthError::Conflict(m) => Error::AlreadyExists(m),
        AuthError::Refused { message, .. } => Error::Forbidden(message),
        e @ (AuthError::Internal(_) | AuthError::Db(_)) => {
            eprintln!("isb serve: accounts: {e}");
            Error::invalid("the identity store failed; see the daemon's journal")
        }
        e => Error::invalid(e.to_string()),
    }
}

/// Who a call acts as: the signed-in user, a superadmin's principal, or
/// (the unix socket) a platform-wide principal with no account.
fn acting(d: &Daemon, c: &Caller) -> Result<Principal> {
    if d.servers.is_none() {
        return Err(Error::Forbidden(
            "accounts are kept by the control plane: call it, not this server".into(),
        ));
    }
    match c {
        Caller::User { principal } => Ok((**principal).clone()),
        Caller::Superadmin(s) => Ok(s.principal.clone()),
        Caller::Local { .. } => Ok(Superadmin::synthetic(SuperadminSource::Token {
            id: 0,
            name: "unix-socket".into(),
        })
        .principal),
        Caller::Access(id) => Err(Error::Forbidden(format!(
            "{} has no isb account; ask an org admin to invite you",
            id.name()
        ))),
        Caller::Unauthenticated { .. } => Err(Error::Forbidden(
            "sign in: send an API token as Authorization: Bearer".into(),
        )),
    }
}

fn org_of(a: &Value) -> Result<OrgId> {
    super::arg_org(a)
}

/// A member named by `user_id` or `email`.
fn member_id(d: &Daemon, user_id: Option<i64>, email: Option<&str>) -> Result<i64> {
    match (user_id, email) {
        (Some(id), _) => Ok(id),
        (None, Some(e)) => d
            .users
            .user_by_email(e)
            .map_err(err)?
            .map(|u| u.id)
            .ok_or_else(|| Error::NotFound(format!("user {e}"))),
        (None, None) => Err(Error::invalid("pass user_id or email")),
    }
}

fn schema(props: Value, required: &[&str], org: &str) -> Value {
    let mut props = props;
    props["org"] = json!({"type": "string", "description": org});
    json!({"type": "object", "properties": props, "required": required, "additionalProperties": false})
}

const ORG: &str = "The org (default: default).";
const NO_ORG: &str = "Ignored: this is about the caller, not an org.";

fn who() -> Value {
    json!({
        "user_id": {"type": "integer", "description": "The member's user id (member_list shows it)."},
        "email": {"type": "string", "description": "Or the member's email."},
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Id {
    id: i64,
    #[serde(default)]
    #[allow(dead_code)]
    org: Option<String>,
}

/// Register one account tool: its handler gets who the call acts as
/// ([`acting`]) besides the arguments and the caller.
macro_rules! account_tool {
    ($r:expr, $d:expr, $name:expr, $title:expr, $desc:expr, $schema:expr, $ann:expr, $f:expr) => {{
        let d = $d.clone();
        let f = $f;
        $r.register(
            Tool::new($name, $desc, $schema, move |a, c| {
                let p = acting(&d, c)?;
                f(&d, &p, a, c)
            })
            .title($title)
            .annotations($ann.clone()),
        )?;
    }};
}

/// The MCP annotations the account tools share.
struct Ann {
    ro: Value,
    write: Value,
    destructive: Value,
}

pub(super) fn register(r: &mut Registry, d: Arc<Daemon>) -> Result<()> {
    let ann = Ann {
        ro: json!({"readOnlyHint": true, "openWorldHint": false}),
        write: json!({"destructiveHint": false, "openWorldHint": false}),
        destructive: json!({"destructiveHint": true, "openWorldHint": false}),
    };
    account_tool!(
        r,
        d,
        "whoami",
        "Who am I",
        "The caller: its user, platform admin flag, orgs and roles, how it signed in (session, API token with its org and scopes, workspace, superadmin), and the orgs it can open. Every caller may ask, a workspace included.",
        schema(json!({}), &[], NO_ORG),
        ann.ro,
        |d: &Daemon, p: &Principal, _a: Value, c: &Caller| -> Result<Value> {
            let mut v = ops::me(&d.users, p).map_err(err)?;
            if c.is_local() {
                v["user"] = Value::Null;
                v["auth"] = json!({"kind": "local"});
                v["superadmin"] = json!({"source": "local", "account": false});
            }
            Ok(v)
        }
    );
    register_members(r, &d, &ann)?;
    register_invitations(r, &d, &ann)?;
    register_agent_identities(r, &d, &ann)?;
    register_tokens(r, &d, &ann)?;
    register_sessions(r, &d, &ann)?;
    register_keys(r, &d, &ann)?;
    register_users(r, &d, &ann)?;
    Ok(())
}

/// An org's members.
fn register_members(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    account_tool!(
        r,
        d,
        "member_list",
        "List members",
        "The org's members: each user (id, email, name), their role (owner, admin, member, viewer) and when they were last active (unix seconds). Any member may list; others get not_found.",
        schema(json!({}), &[], ORG),
        ann.ro,
        |d: &Daemon, p: &Principal, a: Value, _c: &Caller| -> Result<Value> {
            ops::members(&d.users, p, &org_of(&a)?).map_err(err)
        }
    );
    account_tool!(
        r,
        d,
        "member_update",
        "Change a member's role",
        "Set a member's role in the org. Owners and admins; only an owner changes an owner or makes one.",
        schema(
            {
                let mut w = who();
                w["role"] =
                    json!({"type": "string", "enum": ["owner", "admin", "member", "viewer"]});
                w
            },
            &["role"],
            ORG
        ),
        ann.write,
        |d: &Daemon, p: &Principal, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                user_id: Option<i64>,
                email: Option<String>,
                role: Role,
                #[allow(dead_code)]
                org: Option<String>,
            }
            let org = org_of(&a)?;
            let a: A = args(a)?;
            let uid = member_id(d, a.user_id, a.email.as_deref())?;
            ops::set_role(&d.users, p, &org, uid, a.role).map_err(err)
        }
    );
    account_tool!(
        r,
        d,
        "member_remove",
        "Remove a member",
        "Remove a member from the org (their account stays; their tokens for the org stop working). Owners and admins remove others; anyone may remove themselves (leave).",
        schema(who(), &[], ORG),
        ann.destructive,
        |d: &Daemon, p: &Principal, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                user_id: Option<i64>,
                email: Option<String>,
                #[allow(dead_code)]
                org: Option<String>,
            }
            let org = org_of(&a)?;
            let a: A = args(a)?;
            let uid = member_id(d, a.user_id, a.email.as_deref())?;
            ops::remove_member(&d.users, p, &org, uid).map_err(err)?;
            Ok(json!({"removed": uid, "org": org}))
        }
    );
    Ok(())
}

/// An org's invitations.
fn register_invitations(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    account_tool!(
        r,
        d,
        "invitation_list",
        "List invitations",
        "The org's pending invitations (id, email, role, expiry). Owners and admins.",
        schema(json!({}), &[], ORG),
        ann.ro,
        |d: &Daemon, p: &Principal, a: Value, _c: &Caller| -> Result<Value> {
            ops::invitations(&d.users, p, &org_of(&a)?).map_err(err)
        }
    );
    account_tool!(
        r,
        d,
        "invitation_create",
        "Invite someone",
        "Invite an email address to the org with a role (default member; at most your own). Owners and admins. Returns the invitation, its token (shown once) and, when the server knows its public URL, the link to send (<public-url>/invite#<token>). Inviting the same address again replaces the invitation. isb sends no mail: hand the link over yourself.",
        schema(
            json!({
                "email": {"type": "string"},
                "role": {"type": "string", "enum": ["owner", "admin", "member", "viewer"], "description": "Default member."},
            }),
            &["email"],
            ORG
        ),
        ann.write,
        |d: &Daemon, p: &Principal, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                email: String,
                role: Option<Role>,
                #[allow(dead_code)]
                org: Option<String>,
            }
            let org = org_of(&a)?;
            let a: A = args(a)?;
            let url = d.public_url.as_deref();
            ops::invite(&d.users, p, &org, &a.email, a.role, url).map_err(err)
        }
    );
    account_tool!(
        r,
        d,
        "invitation_revoke",
        "Revoke an invitation",
        "Withdraw a pending invitation by id (invitation_list). Owners and admins.",
        schema(json!({"id": {"type": "integer"}}), &["id"], ORG),
        ann.destructive,
        |d: &Daemon, p: &Principal, a: Value, _c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            let a: Id = args(a)?;
            ops::revoke_invitation(&d.users, p, &org, a.id).map_err(err)?;
            Ok(json!({"revoked": a.id, "org": org}))
        }
    );
    Ok(())
}

/// The tailnet and Access identities an org lets in as its agents.
fn register_agent_identities(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    account_tool!(
        r,
        d,
        "agent_identity_list",
        "List agent identities",
        "The org's tailnet and Cloudflare Access agent identities: each a tailnet login or tag, an Access email (of someone who is not an isb user) or a service token's client id, with the role it gets in this org (viewer, member or admin), plus which front doors this server has (`available`: the tailnet listen addresses, whether Access guards a listener). Any member may list.",
        schema(json!({}), &[], ORG),
        ann.ro,
        |d: &Daemon, p: &Principal, a: Value, _c: &Caller| -> Result<Value> {
            ops::agent_identities(&d.users, p, &org_of(&a)?, &d.gate.agent_ways()).map_err(err)
        }
    );
    account_tool!(
        r,
        d,
        "agent_identity_set",
        "Map an identity to a role",
        "Let a tailnet or Access caller in as an agent of this org with a role: tailnet `subject` is a login (someone@example.com) or a node tag (tag:agents; a tagged node matches its tags only, never its owner's login); access `subject` is the email of someone who is not an isb user, or a service token's client id. Roles viewer, member or admin, never owner and at most your own. Setting an existing subject changes its role. Owners and admins. The identity gets this org only, never superadmin.",
        schema(
            json!({
                "kind": {"type": "string", "enum": ["tailnet", "access"]},
                "subject": {"type": "string"},
                "role": {"type": "string", "enum": ["admin", "member", "viewer"]},
                "note": {"type": "string", "description": "What it is for (at most 100 characters)."},
            }),
            &["kind", "subject", "role"],
            ORG
        ),
        ann.write,
        |d: &Daemon, p: &Principal, a: Value, _c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            let b: ops::NewAgentIdentity = args(a)?;
            ops::set_agent_identity(&d.users, p, &org, &b).map_err(err)
        }
    );
    account_tool!(
        r,
        d,
        "agent_identity_remove",
        "Remove an agent identity",
        "Remove an agent identity by id (agent_identity_list); the caller loses its access at once. Owners and admins.",
        schema(json!({"id": {"type": "integer"}}), &["id"], ORG),
        ann.destructive,
        |d: &Daemon, p: &Principal, a: Value, _c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            let a: Id = args(a)?;
            ops::remove_agent_identity(&d.users, p, &org, a.id).map_err(err)?;
            Ok(json!({"removed": a.id, "org": org}))
        }
    );
    Ok(())
}

/// The caller's API tokens, and an org's.
fn register_tokens(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    account_tool!(
        r,
        d,
        "token_list",
        "List API tokens",
        "Your API tokens' metadata (id, name, org, scopes, created, last used, expiry; never the secret), only one org's when `org` is given (an org token sees only its org's). With all=true, every token in `org`, with who holds each: owners and admins.",
        schema(
            json!({"all": {"type": "boolean", "description": "Every token in the org, not just yours (owners and admins)."}}),
            &[],
            "Only this org's tokens (all: the org to list; default: default)."
        ),
        ann.ro,
        |d: &Daemon, p: &Principal, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                #[serde(default)]
                all: bool,
                org: Option<String>,
            }
            let a: A = args(a)?;
            let org = a.org.map(OrgId::new).transpose()?;
            if a.all {
                let org = org.unwrap_or_else(OrgId::default_org);
                return ops::org_tokens(&d.users, p, &org).map_err(err);
            }
            ops::tokens(&d.users, p, org.as_ref()).map_err(err)
        }
    );
    account_tool!(
        r,
        d,
        "token_create",
        "Create an API token",
        "Mint an API token for yourself, confined to the org, optionally narrowed by scopes (read, deploy, admin, tool:GLOB; none: your whole role there) and with an expiry (90d, 12h; none: never). Returns {token, info}; the token is shown only this once. A token cannot mint tokens: this needs a signed-in browser session or an Access or tailnet identity (or `isb token create` on the host), so revoking a leaked token always ends it. Superadmin tokens are minted on the host only.",
        schema(
            json!({
                "name": {"type": "string", "description": "1 to 100 characters."},
                "expires": {"type": "string", "description": "A lifetime such as 90d or 12h; omit for none."},
                "scopes": {"type": "array", "items": {"type": "string"}, "description": "read, deploy, admin, or tool:GLOB (e.g. tool:app_*)."},
            }),
            &["name"],
            "The org the token is confined to (default: default)."
        ),
        ann.write,
        |d: &Daemon, p: &Principal, a: Value, _c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            let mut b: ops::NewToken = args(a)?;
            b.org = Some(org);
            ops::create_token(&d.users, p, b).map_err(err)
        }
    );
    account_tool!(
        r,
        d,
        "token_revoke",
        "Revoke an API token",
        "Revoke an API token by id: your own, or any token in an org you own or administer. It stops working at once.",
        schema(json!({"id": {"type": "integer"}}), &["id"], NO_ORG),
        ann.destructive,
        |d: &Daemon, p: &Principal, a: Value, _c: &Caller| -> Result<Value> {
            let a: Id = args(a)?;
            ops::revoke_token(&d.users, p, a.id).map_err(err)?;
            Ok(json!({"revoked": a.id}))
        }
    );
    Ok(())
}

/// The caller's browser sessions.
fn register_sessions(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    account_tool!(
        r,
        d,
        "session_list",
        "List your sessions",
        "Your signed-in browser sessions: created, last seen, expiry, user agent and address.",
        schema(json!({}), &[], NO_ORG),
        ann.ro,
        |d: &Daemon, p: &Principal, _a: Value, _c: &Caller| -> Result<Value> {
            ops::sessions(&d.users, p).map_err(err)
        }
    );
    account_tool!(
        r,
        d,
        "session_revoke",
        "End a session",
        "Sign one of your browser sessions out, by id (session_list).",
        schema(json!({"id": {"type": "integer"}}), &["id"], NO_ORG),
        ann.destructive,
        |d: &Daemon, p: &Principal, a: Value, _c: &Caller| -> Result<Value> {
            let a: Id = args(a)?;
            ops::revoke_session(&d.users, p, a.id).map_err(err)?;
            Ok(json!({"revoked": a.id}))
        }
    );
    Ok(())
}

/// The caller's SSH keys.
fn register_keys(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    account_tool!(
        r,
        d,
        "ssh_key_list",
        "List your SSH keys",
        "The SSH public keys on your account: what `isb ssh-proxy` lets in to your orgs' workspaces and sandboxes (id, name, algorithm, fingerprint, last use).",
        schema(json!({}), &[], NO_ORG),
        ann.ro,
        |d: &Daemon, p: &Principal, _a: Value, _c: &Caller| -> Result<Value> {
            ops::ssh_keys(&d.users, p).map_err(err)
        }
    );
    account_tool!(
        r,
        d,
        "ssh_key_add",
        "Add an SSH key",
        "Add an SSH public key (an OpenSSH line: ssh-ed25519, ecdsa-sha2-*, ssh-rsa of 2048 bits or more) to your account.",
        schema(
            json!({
                "public_key": {"type": "string", "description": "One OpenSSH public key line."},
                "name": {"type": "string", "description": "Default: the key's comment."},
            }),
            &["public_key"],
            NO_ORG
        ),
        ann.write,
        |d: &Daemon, p: &Principal, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                public_key: String,
                name: Option<String>,
                #[allow(dead_code)]
                org: Option<String>,
            }
            let a: A = args(a)?;
            ops::add_ssh_key(&d.users, p, &a.public_key, a.name.as_deref()).map_err(err)
        }
    );
    account_tool!(
        r,
        d,
        "ssh_key_remove",
        "Remove an SSH key",
        "Remove one of your SSH keys by id; sessions it opened end within seconds.",
        schema(json!({"id": {"type": "integer"}}), &["id"], NO_ORG),
        ann.destructive,
        |d: &Daemon, p: &Principal, a: Value, _c: &Caller| -> Result<Value> {
            let a: Id = args(a)?;
            ops::delete_ssh_key(&d.users, p, a.id).map_err(err)?;
            Ok(json!({"removed": a.id}))
        }
    );
    Ok(())
}

/// Every user (platform admins).
fn register_users(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    account_tool!(
        r,
        d,
        "user_list",
        "List users",
        "Platform admins: every user (id, email, name, platform admin, disabled, password or not), with their orgs and when they were last active.",
        schema(json!({}), &[], NO_ORG),
        ann.ro,
        |d: &Daemon, p: &Principal, _a: Value, _c: &Caller| -> Result<Value> {
            ops::users(&d.users, p).map_err(err)
        }
    );
    account_tool!(
        r,
        d,
        "user_update",
        "Change a user",
        "Platform admins: disable or enable a user (disabling ends their sessions), or grant or revoke platform admin. Nobody does either to themselves, and the last enabled platform admin stays one.",
        schema(
            {
                let mut w = who();
                w["user_id"] = json!({"type": "integer", "description": "The user's id (user_list shows it)."});
                w["email"] = json!({"type": "string", "description": "Or the user's email."});
                w["disabled"] = json!({"type": "boolean"});
                w["platform_admin"] = json!({"type": "boolean"});
                w
            },
            &[],
            NO_ORG
        ),
        ann.write,
        |d: &Daemon, p: &Principal, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                user_id: Option<i64>,
                email: Option<String>,
                #[serde(default)]
                disabled: Option<bool>,
                #[serde(default)]
                platform_admin: Option<bool>,
                #[allow(dead_code)]
                org: Option<String>,
            }
            let a: A = args(a)?;
            let uid = member_id(d, a.user_id, a.email.as_deref())?;
            let change = ops::UserChange {
                disabled: a.disabled,
                platform_admin: a.platform_admin,
            };
            ops::update_user(&d.users, p, uid, &change).map_err(err)
        }
    );
    Ok(())
}

#[cfg(test)]
#[path = "accounts_tests.rs"]
mod tests;
