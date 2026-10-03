//! What the control plane tells an agent about the caller it forwards.
//!
//! The control plane authenticates and authorizes every call, then forwards
//! it with an [`Assertion`] of who the caller is, in `Authorization:
//! IsbAssert <base64url JSON>`. The agent believes it only because the
//! connection is mutual TLS with the control plane's client certificate;
//! it still runs its own authorizer over it (org scope, role, token scopes),
//! so a forwarded call for org X cannot reach org Y.

use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::auth::{Principal, PrincipalKind, Role, User};
use crate::error::{Error, Result};
use crate::org::OrgId;
use crate::server::Caller;

/// The `Authorization` scheme of a forwarded call.
pub const SCHEME: &str = "IsbAssert";

/// A caller, as the control plane vouches for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assertion {
    pub email: String,
    #[serde(default)]
    pub name: String,
    /// `session`, `token`, `access`, `local` (the control plane's socket)
    /// or `control-plane` (the control plane itself: heartbeats, events).
    pub via: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_org: Option<OrgId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub orgs: Vec<(OrgId, Role)>,
    #[serde(default)]
    pub platform_admin: bool,
}

impl Assertion {
    /// The control plane acting for itself.
    pub fn control_plane() -> Self {
        Assertion {
            email: "control-plane".into(),
            name: "isb control plane".into(),
            via: "control-plane".into(),
            token_name: None,
            token_org: None,
            scopes: Vec::new(),
            orgs: Vec::new(),
            platform_admin: true,
        }
    }

    /// What to assert for a caller the control plane admitted. `None` for
    /// callers that never reach a forward (Access identities without an
    /// account; the authorizer refuses them first).
    pub fn for_caller(c: &Caller) -> Option<Self> {
        match c {
            // The control plane's own user: it could run isb against the
            // control plane directly, so it administers every org.
            Caller::Local { uid } => Some(Assertion {
                email: match uid {
                    Some(u) => format!("local(uid {u}) via control plane"),
                    None => "local via control plane".into(),
                },
                via: "local".into(),
                ..Self::control_plane()
            }),
            // Only reaches a forward on a control plane serving with
            // --allow-unauthenticated, which makes every caller an admin.
            Caller::Unauthenticated { addr } => Some(Assertion {
                email: format!("unauthenticated {addr} via control plane"),
                via: "local".into(),
                ..Self::control_plane()
            }),
            // A superadmin of the control plane is the control plane's
            // own user over HTTP: it administers every org, and the agent
            // judges it as it judges the control plane (its policy holds).
            Caller::Superadmin(s) => Some(Assertion {
                email: format!("{} via control plane", s.label()),
                via: "superadmin".into(),
                ..Self::control_plane()
            }),
            Caller::Access(_) => None,
            Caller::User { principal: p } => {
                let (via, token_name, token_org, scopes) = match &p.kind {
                    PrincipalKind::Session { .. } => ("session", None, None, Vec::new()),
                    PrincipalKind::Access => ("access", None, None, Vec::new()),
                    PrincipalKind::Superadmin { .. } => ("superadmin", None, None, Vec::new()),
                    PrincipalKind::ApiToken {
                        org, name, scopes, ..
                    } => ("token", Some(name.clone()), org.clone(), scopes.clone()),
                };
                Some(Assertion {
                    email: p.user.email.clone(),
                    name: p.user.name.clone(),
                    via: via.into(),
                    token_name,
                    token_org,
                    scopes,
                    orgs: p.orgs.clone(),
                    platform_admin: p.platform_admin,
                })
            }
        }
    }

    /// The `Authorization` header value.
    pub fn header(&self) -> String {
        let json = serde_json::to_vec(self).unwrap_or_default();
        format!(
            "{SCHEME} {}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
        )
    }

    /// Parse an `Authorization` header value.
    pub fn from_header(v: &str) -> Result<Self> {
        let b64 = v
            .strip_prefix(SCHEME)
            .and_then(|r| r.strip_prefix(' '))
            .ok_or_else(|| Error::Forbidden("not a control-plane assertion".into()))?;
        let json = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(b64.trim())
            .map_err(|_| Error::Forbidden("a malformed assertion".into()))?;
        serde_json::from_slice(&json)
            .map_err(|e| Error::Forbidden(format!("a malformed assertion: {e}")))
    }

    /// The principal the agent's authorizer judges.
    pub fn principal(&self) -> Principal {
        let kind = match self.via.as_str() {
            "token" => PrincipalKind::ApiToken {
                id: 0,
                org: self.token_org.clone(),
                name: self.token_name.clone().unwrap_or_default(),
                scopes: self.scopes.clone(),
            },
            "access" => PrincipalKind::Access,
            _ => PrincipalKind::Session { id: 0 },
        };
        Principal {
            user: User {
                id: 0,
                email: self.email.clone(),
                name: self.name.clone(),
                platform_admin: self.platform_admin,
                created_at: 0,
                disabled: false,
                has_password: false,
            },
            kind,
            orgs: self.orgs.clone(),
            platform_admin: self.platform_admin,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn a_token_round_trips_with_its_scopes() {
        let p = Principal {
            user: User {
                id: 7,
                email: "a@example.com".into(),
                name: "A".into(),
                platform_admin: false,
                created_at: 1,
                disabled: false,
                has_password: true,
            },
            kind: PrincipalKind::ApiToken {
                id: 3,
                org: Some(OrgId::new("acme").unwrap()),
                name: "agent".into(),
                scopes: vec!["read".into()],
            },
            orgs: vec![(OrgId::new("acme").unwrap(), Role::Member)],
            platform_admin: false,
        };
        let a = Assertion::for_caller(&Caller::User {
            principal: Arc::new(p),
        })
        .unwrap();
        let back = Assertion::from_header(&a.header()).unwrap();
        assert_eq!(a, back);
        let q = back.principal();
        assert_eq!(q.scopes(), ["read".to_string()]);
        assert_eq!(q.role_in(&OrgId::new("acme").unwrap()), Some(Role::Member));
        assert!(!q.platform_admin);
        assert!(Assertion::from_header("Bearer x").is_err());
        assert!(Assertion::from_header("IsbAssert !!!").is_err());
    }

    #[test]
    fn the_local_socket_becomes_a_platform_admin_not_a_local_caller() {
        let a = Assertion::for_caller(&Caller::Local { uid: Some(1000) }).unwrap();
        assert!(a.platform_admin);
        assert!(a.email.contains("uid 1000"));
    }
}
