//! Account operations judged against a [`Principal`]: who is in an org,
//! invitations, API tokens, SSH keys, sessions and users. The identity
//! endpoints ([`super::http`]) and the daemon's account tools
//! (`member_list`, `token_create`, ...) both call these, so a rule holds
//! the same on every surface. Answers are the JSON the endpoints return.
//!
//! The rules on top of each role ([`Role::permissions`]):
//! - Nobody outside an org learns about it: its `members`, `invitations`
//!   and `tokens` answer `404` to non-members (platform admins excepted).
//! - Only an owner (or platform admin) touches an owner or makes one.
//! - A workspace (its `isb_ws_` token) is nobody's account: it reaches
//!   none of this.
//! - **A token cannot mint tokens** ([`may_mint_tokens`]): API tokens,
//!   workspace tokens and superadmin tokens are refused, so revoking a
//!   leaked token always ends what it could do. New tokens come from a
//!   browser session, an Access or tailnet identity, or the host CLI.

use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

#[cfg(test)]
use super::Superadmin;
use super::agent_identities::{AgentKind, AgentWays};
use super::{AuthError, AuthStore, Principal, PrincipalKind, Role, SuperadminSource};
use crate::org::OrgId;

type R<T> = Result<T, AuthError>;

/// Refuse a workspace: account operations are for people and their tokens.
pub fn account_holder(p: &Principal) -> R<()> {
    if p.is_workspace() {
        return Err(AuthError::Forbidden(
            "a workspace token has no reach into accounts, members, invitations, tokens or keys"
                .into(),
        ));
    }
    Ok(())
}

/// Refuse a token scoped short of `admin` (and a workspace) a change to
/// accounts, tokens, keys, invitations or members.
pub fn may_change_accounts(p: &Principal) -> R<()> {
    account_holder(p)?;
    if p.restricted() {
        return Err(AuthError::Forbidden(
            "this token's scopes do not cover changing accounts, tokens or members (it needs admin)"
                .into(),
        ));
    }
    Ok(())
}

/// May `p` mint an API token? Not with a token of any kind: a token that
/// could mint another would survive its own revocation through the copy,
/// and a scope or expiry bound on the copy would not change that.
pub fn may_mint_tokens(p: &Principal) -> R<()> {
    if p.is_agent() {
        return Err(AuthError::Forbidden(
            "a tailnet or Access agent identity has no tokens of its own: create one from a \
             signed-in browser session, or on the host with `isb token create`"
                .into(),
        ));
    }
    let by_token = match &p.kind {
        PrincipalKind::ApiToken { .. } | PrincipalKind::Workspace { .. } => true,
        PrincipalKind::Superadmin { source } => matches!(source, SuperadminSource::Token { .. }),
        PrincipalKind::Session { .. } | PrincipalKind::Access | PrincipalKind::Agent { .. } => {
            false
        }
    };
    if by_token {
        return Err(AuthError::Forbidden(
            "a token cannot mint tokens: create one from a signed-in browser session (Account, \
             API tokens), or on the host with `isb token create`"
                .into(),
        ));
    }
    Ok(())
}

/// The orgs that exist, as the runtime knows them (the daemon asks incus
/// and its servers). The store's org rows anchor memberships and tokens but
/// are not the truth: an org made or removed past the daemon (`isb org`
/// against incus with another state directory, a test) leaves them behind.
pub type OrgsFn = std::sync::Arc<dyn Fn() -> Result<Vec<OrgId>, String> + Send + Sync>;

/// Who is calling: the user, their orgs, and how they signed in.
///
/// `orgs` and `memberships` name only orgs that exist (`existing`, when
/// given): every one for a platform admin (unless the credential is an org
/// token), else the caller's memberships among them. A membership row for
/// an org that is gone never shows. Without `existing` (or when asking it
/// failed) the store's org rows stand in.
pub fn me(store: &AuthStore, p: &Principal, existing: Option<&OrgsFn>) -> R<Value> {
    let real: Option<Vec<OrgId>> = existing.and_then(|f| match f() {
        Ok(v) => Some(v),
        Err(e) => {
            eprintln!("isb serve: whoami: listing orgs: {e}; using the identity store's");
            None
        }
    });
    let exists = |o: &OrgId| real.as_ref().is_none_or(|r| r.contains(o));
    let memberships: Vec<Value> = p
        .orgs
        .iter()
        .filter(|(o, _)| exists(o))
        .map(|(o, r)| json!({"org": o, "role": r}))
        .collect();
    let mut orgs: Vec<OrgId> = if p.platform_admin {
        match &real {
            Some(r) => r.clone(),
            None => store.list_orgs()?,
        }
    } else {
        p.orgs
            .iter()
            .map(|(o, _)| o.clone())
            .filter(|o| exists(o))
            .collect()
    };
    orgs.sort();
    orgs.dedup();
    // A superadmin: where its power comes from, and whether it has an isb
    // account (sessions, passkeys and tokens of its own).
    let superadmin = match &p.kind {
        PrincipalKind::Superadmin { source } => json!({
            "source": source.label(),
            "via": source,
            "account": p.user.id > 0,
        }),
        _ => Value::Null,
    };
    Ok(json!({
        "user": p.user,
        "platform_admin": p.platform_admin,
        "memberships": memberships,
        "orgs": orgs,
        "auth": p.kind,
        "superadmin": superadmin,
    }))
}

// ---- sessions ----

pub fn sessions(store: &AuthStore, p: &Principal) -> R<Value> {
    account_holder(p)?;
    let current = p.session_id();
    let list: Vec<Value> = store
        .list_sessions(p.user.id)?
        .into_iter()
        .map(|s| {
            let mut v = serde_json::to_value(&s).unwrap_or_default();
            v["current"] = json!(Some(s.id) == current);
            v
        })
        .collect();
    Ok(json!({"sessions": list}))
}

pub fn revoke_session(store: &AuthStore, p: &Principal, id: i64) -> R<()> {
    may_change_accounts(p)?;
    if !store.revoke_session(p.user.id, id)? {
        return Err(AuthError::NotFound(format!("session {id}")));
    }
    Ok(())
}

// ---- invitations ----

/// Invite `email` to `org` as `role` (default member). The answer carries
/// the invitation token once, and a link when `public_url` is known.
pub fn invite(
    store: &AuthStore,
    p: &Principal,
    org: &OrgId,
    email: &str,
    role: Option<Role>,
    public_url: Option<&str>,
) -> R<Value> {
    may_change_accounts(p)?;
    let role = role.unwrap_or(Role::Member);
    match p.max_grant(org) {
        Some(max) if role <= max => {}
        Some(_) => {
            return Err(AuthError::Forbidden(format!(
                "you cannot invite someone as {role} in org {org}"
            )));
        }
        None => {
            return Err(AuthError::Forbidden(format!(
                "inviting to org {org} needs owner or admin"
            )));
        }
    }
    let n = store.create_invitation((p.user.id > 0).then_some(p.user.id), org, email, role)?;
    Ok(json!({
        "invitation": n.invitation,
        "token": n.token,
        "link": link(public_url, "invite", &n.token),
    }))
}

/// `<public_url>/<page>#<token>`: the token goes in the fragment, which
/// browsers never send to a server or put in a Referer.
pub fn link(public_url: Option<&str>, page: &str, token: &str) -> Option<String> {
    public_url.map(|u| format!("{}/{page}#{token}", u.trim_end_matches('/')))
}

// ---- API tokens ----

/// What `POST tokens` and `token_create` take.
#[derive(Debug, Deserialize)]
pub struct NewToken {
    pub name: String,
    #[serde(default)]
    pub org: Option<OrgId>,
    /// `90d`, `12h`; absent or null never expires.
    #[serde(default)]
    pub expires: Option<String>,
    /// `read`, `deploy`, `admin`, `tool:GLOB`; empty: the role's reach.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Never honoured: refused, so nobody mistakes the token they get for
    /// one.
    #[serde(default)]
    pub superadmin: bool,
}

/// The caller's own tokens (an org token sees only its org's), or only
/// `org`'s when given.
pub fn tokens(store: &AuthStore, p: &Principal, org: Option<&OrgId>) -> R<Value> {
    account_holder(p)?;
    let list = store.list_api_tokens(p.user.id)?;
    let pinned = match &p.kind {
        PrincipalKind::ApiToken { org: Some(o), .. } => Some(o),
        _ => org,
    };
    let list: Vec<_> = match pinned {
        Some(o) => list
            .into_iter()
            .filter(|t| t.org.as_ref() == Some(o))
            .collect(),
        None => list,
    };
    Ok(json!({"tokens": list}))
}

/// Mint an API token for the caller: `{token, info}`, the token shown once.
pub fn create_token(store: &AuthStore, p: &Principal, b: NewToken) -> R<Value> {
    // Minted on the host only, so a stolen HTTP credential (a superadmin's
    // included) cannot mint a durable one.
    if b.superadmin {
        return Err(AuthError::Forbidden(
            "superadmin tokens are minted on the host only: isb token create NAME --superadmin"
                .into(),
        ));
    }
    may_change_accounts(p)?;
    may_mint_tokens(p)?;
    if p.user.id <= 0 {
        return Err(AuthError::Forbidden(
            "a superadmin without an isb account has no tokens of its own".into(),
        ));
    }
    // Judged by what the caller can reach, not what the user can.
    match &b.org {
        Some(o) if !(p.platform_admin || p.role_in(o).is_some()) => {
            return Err(AuthError::Forbidden(format!(
                "you are not a member of org {o}"
            )));
        }
        None if !p.platform_admin => {
            return Err(AuthError::Forbidden(
                "a token without an org needs a platform admin; pass an org".into(),
            ));
        }
        _ => {}
    }
    let expires: Option<Duration> = b
        .expires
        .filter(|s| !s.trim().is_empty())
        .map(|s| crate::parse_duration(&s).map_err(AuthError::Invalid))
        .transpose()?;
    let t =
        store.create_api_token_scoped(p.user.id, b.org.as_ref(), &b.name, expires, &b.scopes)?;
    Ok(json!({"token": t.token, "info": t.info}))
}

/// Revoke token `id`: its holder's own, or any in an org the caller
/// manages. Anything else is indistinguishable from a token that does not
/// exist.
pub fn revoke_token(store: &AuthStore, p: &Principal, id: i64) -> R<()> {
    may_change_accounts(p)?;
    let hidden = || AuthError::NotFound(format!("token {id}"));
    let t = store.api_token(id).map_err(|e| match e {
        AuthError::NotFound(_) => hidden(),
        e => e,
    })?;
    let mine = t.user_id == p.user.id
        && match &p.kind {
            PrincipalKind::ApiToken { org: Some(o), .. } => t.org.as_ref() == Some(o),
            _ => true,
        };
    let org_admin = t.org.as_ref().is_some_and(|o| p.can_manage_members(o));
    if !(mine || org_admin || p.platform_admin) {
        return Err(hidden());
    }
    store.revoke_api_token(id)?;
    Ok(())
}

// ---- SSH keys ----

pub fn ssh_keys(store: &AuthStore, p: &Principal) -> R<Value> {
    account_holder(p)?;
    let list = if p.user.id > 0 {
        store.list_ssh_keys(p.user.id)?
    } else {
        Vec::new()
    };
    Ok(json!({"ssh_keys": list}))
}

pub fn add_ssh_key(store: &AuthStore, p: &Principal, key: &str, name: Option<&str>) -> R<Value> {
    may_change_accounts(p)?;
    if p.user.id <= 0 {
        return Err(AuthError::Forbidden(
            "a superadmin without an isb account has no SSH keys of its own".into(),
        ));
    }
    let k = store.add_ssh_key(p.user.id, key, name.filter(|n| !n.trim().is_empty()))?;
    Ok(json!({"ssh_key": k}))
}

pub fn delete_ssh_key(store: &AuthStore, p: &Principal, id: i64) -> R<()> {
    may_change_accounts(p)?;
    if !store.delete_ssh_key(p.user.id, id)? {
        return Err(AuthError::NotFound(format!("SSH key {id}")));
    }
    Ok(())
}

// ---- org administration ----

/// Members see who else is in their org; nobody else learns it exists.
pub fn visible_org(p: &Principal, org: &OrgId) -> R<()> {
    account_holder(p)?;
    if p.role_in(org).is_none() && !p.platform_admin {
        return Err(AuthError::NotFound(format!("org {org}")));
    }
    Ok(())
}

fn manage(p: &Principal, org: &OrgId) -> R<()> {
    visible_org(p, org)?;
    if p.can_manage_members(org) {
        Ok(())
    } else {
        Err(AuthError::Forbidden(format!(
            "managing org {org} needs owner or admin"
        )))
    }
}

pub fn members(store: &AuthStore, p: &Principal, org: &OrgId) -> R<Value> {
    visible_org(p, org)?;
    let list: Vec<Value> = store
        .list_members(org)?
        .into_iter()
        .map(|(u, r)| {
            let last = store.last_active(u.id)?;
            Ok(json!({"user": u, "role": r, "last_active": last}))
        })
        .collect::<R<_>>()?;
    Ok(json!({"members": list}))
}

pub fn set_role(store: &AuthStore, p: &Principal, org: &OrgId, uid: i64, role: Role) -> R<Value> {
    manage(p, org)?;
    may_change_accounts(p)?;
    check_role_change(store, p, org, uid, role)?;
    store.set_member(org, uid, role)?;
    Ok(json!({"user_id": uid, "role": role}))
}

/// Remove `uid` from `org`: anyone may leave; removing others needs the
/// right to manage.
pub fn remove_member(store: &AuthStore, p: &Principal, org: &OrgId, uid: i64) -> R<()> {
    visible_org(p, org)?;
    may_change_accounts(p)?;
    if uid != p.user.id {
        manage(p, org)?;
        check_role_change(store, p, org, uid, Role::Member)?;
    }
    if !store.remove_member(org, uid)? {
        return Err(AuthError::NotFound(format!("member {uid}")));
    }
    Ok(())
}

pub fn invitations(store: &AuthStore, p: &Principal, org: &OrgId) -> R<Value> {
    manage(p, org)?;
    Ok(json!({"invitations": store.list_invitations(org)?}))
}

pub fn revoke_invitation(store: &AuthStore, p: &Principal, org: &OrgId, id: i64) -> R<()> {
    manage(p, org)?;
    may_change_accounts(p)?;
    if !store.revoke_invitation(org, id)? {
        return Err(AuthError::NotFound(format!("invitation {id}")));
    }
    Ok(())
}

// ---- agent identities ----

/// What `PUT orgs/{org}/agent-identities` and `agent_identity_set` take.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewAgentIdentity {
    pub kind: AgentKind,
    pub subject: String,
    pub role: Role,
    #[serde(default)]
    pub note: Option<String>,
    /// Ignored: the org is the endpoint's or the tool's `org`.
    #[serde(default)]
    pub org: Option<String>,
}

/// The org's tailnet and Access mappings, and which front doors this
/// server has. Any member may read.
pub fn agent_identities(
    store: &AuthStore,
    p: &Principal,
    org: &OrgId,
    ways: &AgentWays,
) -> R<Value> {
    visible_org(p, org)?;
    // Who gets in without a mapping: the other half of "can an agent sign
    // in here". Counts for every member; the names only for those who
    // manage the org, so a viewer learns nobody's email.
    let names = p.can_manage_members(org);
    let me = p.user.email.as_str();
    let has = |list: &[String]| list.iter().any(|x| x.eq_ignore_ascii_case(me));
    let admins: Vec<String> = store
        .list_users()?
        .into_iter()
        .filter(|u| u.platform_admin && !u.disabled)
        .map(|u| u.email)
        .collect();
    let who = |l: &[String]| if names { l.to_vec() } else { Vec::new() };
    // The flags' superadmins and isb.db's, where this server can match them.
    let (mut sa_access, mut sa_tailnet) = (
        ways.superadmin_access.clone(),
        ways.superadmin_tailnet.clone(),
    );
    for i in store.list_superadmin_identities()? {
        let list = match i.kind {
            AgentKind::Access if ways.access && ways.public_url.is_some() => &mut sa_access,
            AgentKind::Tailnet if !ways.tailnet_listen.is_empty() => &mut sa_tailnet,
            _ => continue,
        };
        if !list.contains(&i.value) {
            list.push(i.value);
        }
    }
    Ok(json!({
        "identities": store.list_agent_identities(org)?,
        "available": {
            "tailnet_listen": ways.tailnet_listen,
            "access": ways.access,
            "public_url": ways.public_url,
            "reach": {
                "platform_admins": {"count": admins.len(), "who": who(&admins)},
                "access_superadmins": {"count": sa_access.len(), "who": who(&sa_access), "you": has(&sa_access)},
                "tailnet_superadmins": {"count": sa_tailnet.len(), "who": who(&sa_tailnet), "you": has(&sa_tailnet)},
            },
        },
    }))
}

/// Map a tailnet login or tag, an Access email or a service token to a
/// role (never owner, and at most the caller's own) in `org`.
pub fn set_agent_identity(
    store: &AuthStore,
    p: &Principal,
    org: &OrgId,
    b: &NewAgentIdentity,
) -> R<Value> {
    manage(p, org)?;
    may_change_accounts(p)?;
    match p.max_grant(org) {
        Some(max) if b.role <= max => {}
        _ => {
            return Err(AuthError::Forbidden(format!(
                "you cannot map an identity to {} in org {org}",
                b.role
            )));
        }
    }
    let by: String = p.user.email.chars().take(100).collect();
    let i = store.set_agent_identity(
        org,
        b.kind,
        &b.subject,
        b.role,
        b.note.as_deref().unwrap_or(""),
        &by,
    )?;
    Ok(json!({"identity": i}))
}

/// Remove a mapping (owners and admins; a mapping is never an owner's).
pub fn remove_agent_identity(store: &AuthStore, p: &Principal, org: &OrgId, id: i64) -> R<()> {
    manage(p, org)?;
    may_change_accounts(p)?;
    if !store.remove_agent_identity(org, id)? {
        return Err(AuthError::NotFound(format!("agent identity {id}")));
    }
    Ok(())
}

/// Every token in `org`, with who holds each: a platform admin's token in
/// an org they are not a member of has no member row to name it.
pub fn org_tokens(store: &AuthStore, p: &Principal, org: &OrgId) -> R<Value> {
    manage(p, org)?;
    let list: Vec<Value> = store
        .list_org_api_tokens(org)?
        .into_iter()
        .map(|t| {
            let u = store.user(t.user_id)?;
            let mut v = serde_json::to_value(&t).unwrap_or_default();
            v["user"] = json!({"id": u.id, "email": u.email, "name": u.name});
            Ok(v)
        })
        .collect::<R<_>>()?;
    Ok(json!({"tokens": list}))
}

/// Only an owner (or platform admin) touches an owner or makes one; an
/// admin manages members and admins.
fn check_role_change(
    store: &AuthStore,
    p: &Principal,
    org: &OrgId,
    target: i64,
    new: Role,
) -> R<()> {
    let max = p.max_grant(org).unwrap_or(Role::Member);
    let current = store
        .memberships(target)?
        .into_iter()
        .find(|m| &m.org == org)
        .map(|m| m.role);
    if new > max || current.is_some_and(|c| c > max) {
        return Err(AuthError::Forbidden(format!(
            "only an owner can change an owner, or make one, in org {org}"
        )));
    }
    Ok(())
}

// ---- platform administration ----

fn platform_admin(p: &Principal) -> R<()> {
    account_holder(p)?;
    if p.platform_admin {
        Ok(())
    } else {
        Err(AuthError::Forbidden("this is for platform admins".into()))
    }
}

/// Every user, with their orgs and when they were last active.
pub fn users(store: &AuthStore, p: &Principal) -> R<Value> {
    platform_admin(p)?;
    let list: Vec<Value> = store
        .list_users()?
        .into_iter()
        .map(|u| {
            let memberships = store.memberships(u.id)?;
            let last = store.last_active(u.id)?;
            let mut v = serde_json::to_value(&u).unwrap_or_default();
            v["memberships"] = json!(memberships);
            v["last_active"] = json!(last);
            Ok(v)
        })
        .collect::<R<_>>()?;
    Ok(json!({"users": list}))
}

/// What `PATCH admin/users/ID` and `user_update` change.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserChange {
    #[serde(default)]
    pub disabled: Option<bool>,
    #[serde(default)]
    pub platform_admin: Option<bool>,
}

/// Disable or enable a user, or make or unmake a platform admin. Nobody
/// does either to themselves, and the platform keeps an enabled admin.
pub fn update_user(store: &AuthStore, p: &Principal, id: i64, b: &UserChange) -> R<Value> {
    platform_admin(p)?;
    may_change_accounts(p)?;
    let u = store.user(id)?;
    let demoting = b.disabled == Some(true) || b.platform_admin == Some(false);
    if demoting && id == p.user.id {
        return Err(AuthError::Forbidden(
            "you cannot disable yourself or drop your own platform admin role; ask another platform admin".into(),
        ));
    }
    if demoting && u.platform_admin && store.other_platform_admins(id)? == 0 {
        return Err(AuthError::Conflict(format!(
            "{} is the last enabled platform admin; make someone else one first",
            u.email
        )));
    }
    if let Some(a) = b.platform_admin {
        store.set_platform_admin(id, a)?;
    }
    if let Some(d) = b.disabled {
        store.set_disabled(id, d)?;
    }
    Ok(json!({"user": store.user(id)?}))
}

#[cfg(test)]
#[path = "ops_tests.rs"]
mod tests;
