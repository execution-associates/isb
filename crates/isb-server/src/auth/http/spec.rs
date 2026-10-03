//! The identity endpoints as data: every route [`super::AuthApi`] answers,
//! with who may call it, its body and its answer, and the MCP tool that
//! does the same (or why there is none). The OpenAPI document is built from
//! this table, and a test holds it to the router's own `match` arms, so the
//! two cannot drift apart.

use serde_json::{Value, json};

/// Where the same capability is for agents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agents {
    /// This tool does the same, with the same rules.
    Tool(&'static str),
    /// Deliberately not a tool, and why.
    BrowserOnly(&'static str),
}

/// One identity endpoint.
#[derive(Debug, Clone, Copy)]
pub struct Route {
    pub method: &'static str,
    /// Under `/api/v1/auth/`, with `{param}`s.
    pub path: &'static str,
    pub summary: &'static str,
    /// Who may call it.
    pub who: &'static str,
    /// The JSON body, when it takes one.
    pub body: Option<fn() -> Value>,
    /// The success status.
    pub ok: u16,
    /// The success answer (`None`: no body, or a redirect).
    pub answer: Option<fn() -> Value>,
    pub agents: Agents,
}

const SIGN_IN: &str =
    "a browser sign-in flow: it sets the session cookie a person's browser carries";
const WAYS_IN: &str = "a way into the account: changed from a signed-in browser session only, so a leaked token cannot lock its owner out or let an attacker in";
const NO_CREDENTIAL: &str =
    "for someone who holds no credential yet: the token or address they bring is the credential";

fn r(name: &str) -> Value {
    json!({"$ref": format!("#/components/schemas/{name}")})
}

fn obj(props: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": props, "required": required})
}

fn list(key: &str, item: &str) -> Value {
    obj(json!({key: {"type": "array", "items": r(item)}}), &[key])
}

fn session_answer() -> Value {
    r("SessionAnswer")
}
fn me() -> Value {
    r("Me")
}
fn setup_get() -> Value {
    obj(json!({"needed": {"type": "boolean"}}), &["needed"])
}
fn setup_body() -> Value {
    obj(
        json!({"setup_token": {"type": "string"}, "email": {"type": "string"}, "name": {"type": "string"}, "password": {"type": "string"}}),
        &["setup_token", "email", "password"],
    )
}
fn login_body() -> Value {
    obj(
        json!({"email": {"type": "string"}, "password": {"type": "string"}}),
        &["email", "password"],
    )
}
fn sessions() -> Value {
    list("sessions", "Session")
}
fn invite_body() -> Value {
    obj(
        json!({"org": {"type": "string"}, "email": {"type": "string"}, "role": r("Role")}),
        &["org", "email"],
    )
}
fn invite_answer() -> Value {
    obj(
        json!({"invitation": r("Invitation"), "token": {"type": "string"}, "link": {"type": ["string", "null"]}}),
        &["invitation", "token", "link"],
    )
}
fn token_body() -> Value {
    obj(json!({"token": {"type": "string"}}), &["token"])
}
fn invitation_info() -> Value {
    r("InvitationInfo")
}
fn accept_body() -> Value {
    obj(
        json!({"token": {"type": "string"}, "name": {"type": "string"}, "password": {"type": "string"}}),
        &["token"],
    )
}
fn accept_answer() -> Value {
    obj(
        json!({"user": r("User"), "membership": r("Membership"), "created": {"type": "boolean"}}),
        &["user", "membership", "created"],
    )
}
fn tokens() -> Value {
    list("tokens", "ApiToken")
}
fn new_token_body() -> Value {
    obj(
        json!({
            "name": {"type": "string"},
            "org": {"type": ["string", "null"]},
            "expires": {"type": ["string", "null"], "description": "90d, 12h; absent: never."},
            "scopes": {"type": "array", "items": {"type": "string"}, "description": "read, deploy, admin, tool:GLOB."},
        }),
        &["name"],
    )
}
fn new_token_answer() -> Value {
    obj(
        json!({"token": {"type": "string"}, "info": r("ApiToken")}),
        &["token", "info"],
    )
}
fn ssh_keys() -> Value {
    list("ssh_keys", "SshKey")
}
fn ssh_key_body() -> Value {
    obj(
        json!({"public_key": {"type": "string"}, "name": {"type": "string"}}),
        &["public_key"],
    )
}
fn ssh_key_answer() -> Value {
    obj(json!({"ssh_key": r("SshKey")}), &["ssh_key"])
}
fn password_body() -> Value {
    obj(
        json!({"current_password": {"type": "string"}, "new_password": {"type": "string"}}),
        &["current_password", "new_password"],
    )
}
fn email_body() -> Value {
    obj(json!({"email": {"type": "string"}}), &["email"])
}
fn ok_answer() -> Value {
    obj(json!({"ok": {"type": "boolean"}}), &["ok"])
}
fn reset_body() -> Value {
    obj(
        json!({"token": {"type": "string"}, "password": {"type": "string"}}),
        &["token", "password"],
    )
}
fn members() -> Value {
    list("members", "Member")
}
fn role_body() -> Value {
    obj(json!({"role": r("Role")}), &["role"])
}
fn role_answer() -> Value {
    obj(
        json!({"user_id": {"type": "integer"}, "role": r("Role")}),
        &["user_id", "role"],
    )
}
fn invitations() -> Value {
    list("invitations", "Invitation")
}
fn org_tokens() -> Value {
    list("tokens", "OrgToken")
}
fn users() -> Value {
    list("users", "AdminUser")
}
fn user_change() -> Value {
    json!({"type": "object", "properties": {"disabled": {"type": "boolean"}, "platform_admin": {"type": "boolean"}}, "additionalProperties": false})
}
fn user_answer() -> Value {
    obj(json!({"user": r("User")}), &["user"])
}
fn providers() -> Value {
    r("Providers")
}
fn oauth_body() -> Value {
    obj(
        json!({"next": {"type": "string"}, "invite": {"type": "string"}, "intent": {"type": "string", "enum": ["login", "link"]}}),
        &[],
    )
}
fn url_answer() -> Value {
    obj(json!({"url": {"type": "string"}}), &["url"])
}
fn identities() -> Value {
    list("identities", "Identity")
}
fn passkeys() -> Value {
    list("passkeys", "Passkey")
}
fn public_key_options() -> Value {
    obj(
        json!({"publicKey": {"type": "object", "additionalProperties": true, "description": "WebAuthn options, in the JSON form of PublicKeyCredential.parse*OptionsFromJSON()."}}),
        &["publicKey"],
    )
}
fn passkey_login_body() -> Value {
    obj(json!({"email": {"type": "string"}}), &[])
}
fn credential_body() -> Value {
    obj(
        json!({"name": {"type": "string"}, "credential": {"type": "object", "additionalProperties": true, "description": "The browser's credential.toJSON()."}}),
        &["credential"],
    )
}
fn passkey_answer() -> Value {
    obj(json!({"passkey": r("Passkey")}), &["passkey"])
}

macro_rules! route {
    ($m:literal $p:literal, $summary:literal, $who:literal, $body:expr, $ok:literal, $answer:expr, $agents:expr) => {
        Route {
            method: $m,
            path: $p,
            summary: $summary,
            who: $who,
            body: $body,
            ok: $ok,
            answer: $answer,
            agents: $agents,
        }
    };
}

use Agents::{BrowserOnly as B, Tool as T};

/// Every identity endpoint.
pub const ROUTES: &[Route] = &[
    route!("GET" "setup", "Is first-run setup needed", "anyone", None, 200, Some(setup_get), B("first-run setup happens once, in a browser, with the setup token from the host")),
    route!("POST" "setup", "Create the first platform admin", "anyone, with the setup token", Some(setup_body), 201, Some(session_answer), B("first-run setup happens once, in a browser, with the setup token from the host")),
    route!("POST" "login", "Sign in with a password", "anyone", Some(login_body), 200, Some(session_answer), B(SIGN_IN)),
    route!("POST" "logout", "Sign out", "anyone", None, 204, None, B(SIGN_IN)),
    route!("GET" "me", "Who is calling", "signed in", None, 200, Some(me), T("whoami")),
    route!("GET" "sessions", "Your sessions", "signed in", None, 200, Some(sessions), T("session_list")),
    route!("DELETE" "sessions/{id}", "End a session", "signed in", None, 204, None, T("session_revoke")),
    route!("POST" "invitations", "Invite someone to an org", "org owners and admins", Some(invite_body), 201, Some(invite_answer), T("invitation_create")),
    route!("POST" "invitations/inspect", "What an invitation is for", "anyone with the token", Some(token_body), 200, Some(invitation_info), B(NO_CREDENTIAL)),
    route!("POST" "invitations/accept", "Accept an invitation", "anyone with the token", Some(accept_body), 200, Some(accept_answer), B(NO_CREDENTIAL)),
    route!("GET" "tokens", "Your API tokens", "signed in", None, 200, Some(tokens), T("token_list")),
    route!("POST" "tokens", "Create an API token", "signed in with a session or an Access or tailnet identity; never with a token", Some(new_token_body), 201, Some(new_token_answer), T("token_create")),
    route!("DELETE" "tokens/{id}", "Revoke an API token", "its user, or the org's owners and admins", None, 204, None, T("token_revoke")),
    route!("GET" "ssh-keys", "Your SSH keys", "signed in", None, 200, Some(ssh_keys), T("ssh_key_list")),
    route!("POST" "ssh-keys", "Add an SSH key", "signed in, with an account", Some(ssh_key_body), 201, Some(ssh_key_answer), T("ssh_key_add")),
    route!("DELETE" "ssh-keys/{id}", "Remove an SSH key", "signed in, with an account", None, 204, None, T("ssh_key_remove")),
    route!("POST" "password", "Change your password", "signed in with a session", Some(password_body), 204, None, B(WAYS_IN)),
    route!("POST" "password-reset/request", "Ask for a password reset", "anyone", Some(email_body), 202, Some(ok_answer), B(NO_CREDENTIAL)),
    route!("POST" "password-reset/confirm", "Set a password with a reset token", "anyone with the token", Some(reset_body), 204, None, B(NO_CREDENTIAL)),
    route!("GET" "providers", "How one can sign in here", "anyone", None, 200, Some(providers), B(SIGN_IN)),
    route!("GET" "oauth/{provider}/start", "Start a provider sign-in (redirect)", "anyone", None, 303, None, B(SIGN_IN)),
    route!("POST" "oauth/{provider}/start", "Start a provider sign-in", "anyone", Some(oauth_body), 200, Some(url_answer), B(SIGN_IN)),
    route!("GET" "oauth/{provider}/callback", "The provider's redirect back", "the provider's redirect", None, 303, None, B(SIGN_IN)),
    route!("GET" "identities", "Your linked sign-in identities", "signed in", None, 200, Some(identities), B(WAYS_IN)),
    route!("DELETE" "identities/{id}", "Unlink a sign-in identity", "signed in with a session", None, 204, None, B(WAYS_IN)),
    route!("GET" "passkeys", "Your passkeys", "signed in", None, 200, Some(passkeys), B(WAYS_IN)),
    route!("DELETE" "passkeys/{id}", "Remove a passkey", "signed in with a session", None, 204, None, B(WAYS_IN)),
    route!("POST" "passkeys/register/options", "Begin adding a passkey", "signed in with a session", None, 200, Some(public_key_options), B(WAYS_IN)),
    route!("POST" "passkeys/register/verify", "Finish adding a passkey", "signed in with a session", Some(credential_body), 201, Some(passkey_answer), B(WAYS_IN)),
    route!("POST" "passkeys/login/options", "Begin a passkey sign-in", "anyone", Some(passkey_login_body), 200, Some(public_key_options), B(SIGN_IN)),
    route!("POST" "passkeys/login/verify", "Finish a passkey sign-in", "anyone", Some(credential_body), 200, Some(session_answer), B(SIGN_IN)),
    route!("GET" "admin/users", "Every user", "platform admins", None, 200, Some(users), T("user_list")),
    route!("PATCH" "admin/users/{id}", "Disable a user, or grant platform admin", "platform admins", Some(user_change), 200, Some(user_answer), T("user_update")),
    route!("GET" "orgs/{org}/members", "An org's members", "org members", None, 200, Some(members), T("member_list")),
    route!("PUT" "orgs/{org}/members/{user_id}", "Change a member's role", "org owners and admins", Some(role_body), 200, Some(role_answer), T("member_update")),
    route!("DELETE" "orgs/{org}/members/{user_id}", "Remove a member (or leave)", "org owners and admins, or the member leaving", None, 204, None, T("member_remove")),
    route!("GET" "orgs/{org}/invitations", "An org's pending invitations", "org owners and admins", None, 200, Some(invitations), T("invitation_list")),
    route!("DELETE" "orgs/{org}/invitations/{id}", "Revoke an invitation", "org owners and admins", None, 204, None, T("invitation_revoke")),
    route!("GET" "orgs/{org}/tokens", "Every API token in an org", "org owners and admins", None, 200, Some(org_tokens), T("token_list")),
];

/// The schemas the identity endpoints answer with, by name.
pub fn schemas() -> Value {
    let user = obj(
        json!({
            "id": {"type": "integer"}, "email": {"type": "string"}, "name": {"type": "string"},
            "platform_admin": {"type": "boolean"}, "created_at": {"type": "integer"},
            "disabled": {"type": "boolean"}, "has_password": {"type": "boolean"},
        }),
        &[
            "id",
            "email",
            "name",
            "platform_admin",
            "created_at",
            "disabled",
            "has_password",
        ],
    );
    let token_props = json!({
        "id": {"type": "integer"}, "name": {"type": "string"}, "user_id": {"type": "integer"},
        "org": {"type": ["string", "null"]}, "created_at": {"type": "integer"},
        "last_used": {"type": ["integer", "null"]}, "expires_at": {"type": ["integer", "null"]},
        "scopes": {"type": "array", "items": {"type": "string"}},
    });
    let token_req = [
        "id",
        "name",
        "user_id",
        "org",
        "created_at",
        "last_used",
        "expires_at",
        "scopes",
    ];
    let mut org_token_props = token_props.clone();
    org_token_props["user"] = obj(
        json!({"id": {"type": "integer"}, "email": {"type": "string"}, "name": {"type": "string"}}),
        &["id", "email", "name"],
    );
    let mut org_token_req = token_req.to_vec();
    org_token_req.push("user");
    let mut admin_user = user.clone();
    admin_user["properties"]["memberships"] = json!({"type": "array", "items": r("Membership")});
    admin_user["properties"]["last_active"] = json!({"type": ["integer", "null"]});
    for k in ["memberships", "last_active"] {
        admin_user["required"]
            .as_array_mut()
            .expect("required")
            .push(json!(k));
    }
    let mut out = json!({
        "Role": {"type": "string", "enum": ["owner", "admin", "member", "viewer"]},
        "AuthError": obj(json!({"error": {"type": "string"}, "message": {"type": "string"}}), &["error", "message"]),
        "User": user,
        "AdminUser": admin_user,
        "Membership": obj(json!({"org": {"type": "string"}, "role": r("Role")}), &["org", "role"]),
        "Member": obj(json!({"user": r("User"), "role": r("Role"), "last_active": {"type": ["integer", "null"]}}), &["user", "role", "last_active"]),
        "Session": obj(json!({
            "id": {"type": "integer"}, "user_id": {"type": "integer"}, "created_at": {"type": "integer"},
            "last_seen": {"type": "integer"}, "expires_at": {"type": "integer"}, "idle_expires_at": {"type": "integer"},
            "user_agent": {"type": ["string", "null"]}, "ip": {"type": ["string", "null"]}, "current": {"type": "boolean"},
        }), &["id", "user_id", "created_at", "last_seen", "expires_at", "idle_expires_at", "user_agent", "ip", "current"]),
        "ApiToken": obj(token_props, &token_req),
        "OrgToken": obj(org_token_props, &org_token_req),
        "Invitation": obj(json!({
            "id": {"type": "integer"}, "org": {"type": "string"}, "email": {"type": "string"}, "role": r("Role"),
            "invited_by": {"type": ["integer", "null"]}, "created_at": {"type": "integer"},
            "expires_at": {"type": "integer"}, "accepted_at": {"type": ["integer", "null"]},
        }), &["id", "org", "email", "role", "invited_by", "created_at", "expires_at", "accepted_at"]),
        "InvitationInfo": obj(json!({
            "org": {"type": "string"}, "email": {"type": "string"}, "role": r("Role"),
            "expires_at": {"type": "integer"}, "account_exists": {"type": "boolean"},
        }), &["org", "email", "role", "expires_at", "account_exists"]),
        "SshKey": obj(json!({
            "id": {"type": "integer"}, "user_id": {"type": "integer"}, "name": {"type": "string"},
            "algorithm": {"type": "string"}, "public_key": {"type": "string"}, "fingerprint": {"type": "string"},
            "created_at": {"type": "integer"}, "last_used": {"type": ["integer", "null"]},
        }), &["id", "user_id", "name", "algorithm", "public_key", "fingerprint", "created_at", "last_used"]),
    });
    if let (Some(s), Value::Object(more)) = (out.as_object_mut(), sign_in_schemas()) {
        s.extend(more);
    }
    out
}

/// The schemas of the sign-in answers: sessions, identities, passkeys,
/// providers and `me`.
fn sign_in_schemas() -> Value {
    let superadmin_via = json!({"type": "object", "properties": {"kind": {"type": "string", "enum": ["token", "tailnet", "access"]}}, "required": ["kind"], "additionalProperties": true});
    json!({
        "SessionAnswer": obj(json!({
            "user": r("User"),
            "memberships": {"type": "array", "items": r("Membership")},
            "session": obj(json!({"id": {"type": "integer"}, "expires_at": {"type": "integer"}, "idle_expires_at": {"type": "integer"}}), &["id", "expires_at", "idle_expires_at"]),
        }), &["user", "memberships", "session"]),
        "Identity": obj(json!({
            "id": {"type": "integer"}, "user_id": {"type": "integer"}, "provider": {"type": "string"},
            "provider_id": {"type": ["string", "null"]}, "label": {"type": "string"}, "subject": {"type": "string"},
            "email": {"type": ["string", "null"]}, "email_verified": {"type": "boolean"},
            "created_at": {"type": "integer"}, "last_used": {"type": ["integer", "null"]},
        }), &["id", "user_id", "provider", "provider_id", "label", "subject", "email", "email_verified", "created_at", "last_used"]),
        "Passkey": obj(json!({
            "id": {"type": "integer"}, "user_id": {"type": "integer"}, "credential_id": {"type": "string"},
            "name": {"type": "string"}, "alg": {"type": "integer"}, "sign_count": {"type": "integer"},
            "transports": {"type": "array", "items": {"type": "string"}}, "aaguid": {"type": ["string", "null"]},
            "created_at": {"type": "integer"}, "last_used": {"type": ["integer", "null"]},
        }), &["id", "user_id", "credential_id", "name", "alg", "sign_count", "transports", "aaguid", "created_at", "last_used"]),
        "Provider": obj(json!({
            "id": {"type": "string"}, "label": {"type": "string"},
            "kind": {"type": "string", "enum": ["oauth2", "oidc"]}, "start": {"type": "string"},
        }), &["id", "label", "kind", "start"]),
        "Providers": obj(json!({
            "providers": {"type": "array", "items": r("Provider")}, "password": {"type": "boolean"},
            "passkeys": {"type": "boolean"}, "open_signup": {"type": "boolean"},
        }), &["providers", "password", "passkeys", "open_signup"]),
        "Me": obj(json!({
            "user": r("User"),
            "platform_admin": {"type": "boolean"},
            "memberships": {"type": "array", "items": r("Membership")},
            "orgs": {"type": "array", "items": {"type": "string"}, "description": "Every org this caller can open."},
            "auth": {"type": "object", "properties": {"kind": {"type": "string", "enum": ["session", "api_token", "access", "superadmin", "workspace"]}}, "required": ["kind"], "additionalProperties": true, "description": "How the caller signed in: {kind: session, id}, {kind: api_token, id, org, name, scopes?}, {kind: access}, {kind: superadmin, source}, {kind: workspace, org, name}."},
            "superadmin": {"oneOf": [{"type": "null"}, obj(json!({"source": {"type": "string"}, "via": superadmin_via, "account": {"type": "boolean"}}), &["source", "via", "account"])]},
        }), &["user", "platform_admin", "memberships", "orgs", "auth", "superadmin"]),
    })
}

/// The OpenAPI path items for the identity endpoints, keyed by full path.
pub fn paths() -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    for rt in ROUTES {
        let full = format!("{}{}", super::PREFIX, rt.path);
        let params: Vec<Value> = rt
            .path
            .split('/')
            .filter_map(|s| s.strip_prefix('{').and_then(|s| s.strip_suffix('}')))
            .map(|p| {
                let ty = if matches!(p, "id" | "user_id") {
                    "integer"
                } else {
                    "string"
                };
                json!({"name": p, "in": "path", "required": true, "schema": {"type": ty}})
            })
            .collect();
        let mut op = json!({
            "operationId": operation_id(rt),
            "tags": ["identity"],
            "summary": rt.summary,
            "description": format!("Who: {}.", rt.who),
            "responses": {"default": {"description": "An error", "content": {"application/json": {"schema": r("AuthError")}}}},
        });
        match rt.agents {
            Agents::Tool(t) => op["x-isb-tool"] = json!(t),
            Agents::BrowserOnly(why) => op["x-isb-browser-only"] = json!(why),
        }
        if !params.is_empty() {
            op["parameters"] = json!(params);
        }
        if let Some(b) = rt.body {
            op["requestBody"] =
                json!({"required": true, "content": {"application/json": {"schema": b()}}});
        }
        op["responses"][rt.ok.to_string()] = match rt.answer {
            Some(a) => {
                json!({"description": "OK", "content": {"application/json": {"schema": a()}}})
            }
            None if rt.ok == 303 => json!({"description": "A redirect (Location)"}),
            None => json!({"description": "No content"}),
        };
        let item = out.entry(full).or_insert_with(|| json!({}));
        item[rt.method.to_ascii_lowercase()] = op;
    }
    out
}

/// `auth_get_orgs_org_members` and the like: unique and stable.
fn operation_id(rt: &Route) -> String {
    let path: String = rt
        .path
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let path = path
        .split('_')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    format!("auth_{}_{path}", rt.method.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The router's `("METHOD", ["seg", var, ...])` arms, as `METHOD a/{}/b`.
    fn router_arms() -> Vec<String> {
        let src = include_str!("../http.rs");
        let arm = regex_lite_arms(src);
        let mut out = Vec::new();
        let mut in_org = false;
        for line in src.lines() {
            if line.contains("fn org_route(") {
                in_org = true;
            }
            if line.contains("// ---- platform administration") {
                in_org = false;
            }
            for (m, segs) in arm(line) {
                let p = segs.join("/");
                out.push(if in_org {
                    format!("{m} orgs/{{}}/{p}")
                } else {
                    format!("{m} {p}")
                });
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// A tiny matcher for `("GET", ["a", b, "c"])`, without a regex crate.
    fn regex_lite_arms(_src: &str) -> impl Fn(&str) -> Vec<(String, Vec<String>)> {
        |line: &str| {
            let mut found = Vec::new();
            let mut rest = line;
            while let Some(i) = rest.find("(\"") {
                let after = &rest[i + 2..];
                let Some(q) = after.find('"') else { break };
                let method = &after[..q];
                let tail = &after[q + 1..];
                rest = tail;
                if !matches!(method, "GET" | "POST" | "PUT" | "PATCH" | "DELETE") {
                    continue;
                }
                let Some(tail) = tail.strip_prefix(", [") else {
                    continue;
                };
                let Some(end) = tail.find("])") else { continue };
                let segs: Vec<String> = tail[..end]
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(
                        |s| match s.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                            Some(lit) => lit.to_string(),
                            None => "{}".to_string(),
                        },
                    )
                    .collect();
                found.push((method.to_string(), segs));
            }
            found
        }
    }

    #[test]
    fn the_table_is_the_router() {
        let mut table: Vec<String> = ROUTES
            .iter()
            .map(|r| {
                let p: Vec<&str> = r
                    .path
                    .split('/')
                    .map(|s| if s.starts_with('{') { "{}" } else { s })
                    .collect();
                format!("{} {}", r.method, p.join("/"))
            })
            .collect();
        table.sort();
        let before = table.len();
        table.dedup();
        assert_eq!(before, table.len(), "a route is listed twice");
        assert_eq!(table, router_arms());
    }

    #[test]
    fn paths_and_operations_are_unique() {
        let p = paths();
        let ops: Vec<String> = ROUTES.iter().map(operation_id).collect();
        let mut sorted = ops.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), ops.len());
        assert!(p["/api/v1/auth/orgs/{org}/members/{user_id}"]["put"].is_object());
        assert_eq!(
            p["/api/v1/auth/tokens"]["post"]["x-isb-tool"],
            "token_create"
        );
    }

    /// The hand-written schemas name exactly the fields the types serialize.
    #[test]
    fn schemas_match_the_types() {
        use crate::auth::{ApiToken, Invitation, Membership, Role, Session, User};
        use crate::org::OrgId;
        let s = schemas();
        let keys = |v: Value| -> Vec<String> {
            let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
            k.sort();
            k
        };
        let props = |name: &str| -> Vec<String> {
            let mut k: Vec<String> = s[name]["properties"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect();
            k.sort();
            k
        };
        let user = User {
            id: 1,
            email: "a@x.io".into(),
            name: "A".into(),
            platform_admin: false,
            created_at: 0,
            disabled: false,
            has_password: true,
        };
        assert_eq!(keys(serde_json::to_value(&user).unwrap()), props("User"));
        let org = OrgId::new("acme").unwrap();
        let m = Membership {
            org: org.clone(),
            role: Role::Admin,
        };
        assert_eq!(keys(serde_json::to_value(&m).unwrap()), props("Membership"));
        let t = ApiToken {
            id: 1,
            name: "t".into(),
            user_id: 1,
            org: Some(org.clone()),
            created_at: 0,
            last_used: None,
            expires_at: None,
            scopes: vec![],
        };
        assert_eq!(keys(serde_json::to_value(&t).unwrap()), props("ApiToken"));
        let i = Invitation {
            id: 1,
            org,
            email: "a@x.io".into(),
            role: Role::Member,
            invited_by: None,
            created_at: 0,
            expires_at: 0,
            accepted_at: None,
        };
        assert_eq!(keys(serde_json::to_value(&i).unwrap()), props("Invitation"));
        let sess = Session {
            id: 1,
            user_id: 1,
            created_at: 0,
            last_seen: 0,
            expires_at: 0,
            idle_expires_at: 0,
            user_agent: None,
            ip: None,
        };
        let mut v = serde_json::to_value(&sess).unwrap();
        v["current"] = json!(true);
        assert_eq!(keys(v), props("Session"));
        let k = crate::auth::ssh_keys::SshKey {
            id: 1,
            user_id: 1,
            name: "k".into(),
            algorithm: "ssh-ed25519".into(),
            public_key: "x".into(),
            fingerprint: "SHA256:x".into(),
            created_at: 0,
            last_used: None,
        };
        assert_eq!(keys(serde_json::to_value(&k).unwrap()), props("SshKey"));
        let pk = crate::auth::external::Passkey {
            id: 1,
            user_id: 1,
            credential_id: "c".into(),
            name: "n".into(),
            alg: -7,
            sign_count: 0,
            transports: vec![],
            aaguid: "00".into(),
            created_at: 0,
            last_used: None,
        };
        assert_eq!(keys(serde_json::to_value(&pk).unwrap()), props("Passkey"));
    }
}
