//! Superadmins: the unix socket's reach (every tool, no remote-spec policy,
//! any instance) for an HTTP caller. Three sources grant it, and nothing else:
//!
//! - a **superadmin token** (`isb_sa_...`), minted only on the host with
//!   `isb token create NAME --superadmin`, never over HTTP, so a stolen HTTP
//!   credential cannot mint a durable one;
//! - a **tailnet identity** on `isb serve --superadmin-tailnet` (the daemon's
//!   [`crate::server::tailnet`] check), judged from the real socket peer;
//! - a **Cloudflare Access identity** on `isb serve --superadmin-access`: a
//!   verified `Cf-Access-Jwt-Assertion` whose email (or service token
//!   client id) is on the list;
//! - in a debug build, `ISB_DEV_SUPERADMIN` ([`super::dev`]): any loopback
//!   request with no credential, for developing isb.
//!
//! A superadmin acts as an isb user when its tailnet login, Access email or
//! dev email is one, else as a synthetic principal (user id 0) named after the source.

use std::time::Duration;

use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use super::secret::{self, TokenKind};
use super::{AuthError, AuthResult, AuthStore, Principal, PrincipalKind, TOUCH_EVERY, User};

/// Where a superadmin's power comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SuperadminSource {
    Token {
        id: i64,
        name: String,
    },
    Tailnet {
        /// The tailnet login (`tagged-devices` for a tagged node).
        login: String,
        /// The node's MagicDNS name.
        node: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tags: Vec<String>,
    },
    Access {
        /// The email, or a service token's client id.
        name: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        service_token: bool,
    },
    /// `ISB_DEV_SUPERADMIN` ([`super::dev`]; debug builds): any loopback
    /// request with no credential.
    Dev {
        email: String,
    },
}

impl SuperadminSource {
    /// `token:<name>`, `tailnet:<login>` (a tagged node: `tailnet:<node>`),
    /// `access:<name>` or `dev:<email>`, as audit rows and `isb.owner` labels name it.
    pub fn label(&self) -> String {
        match self {
            SuperadminSource::Token { name, .. } => format!("token:{name}"),
            SuperadminSource::Tailnet { login, node, tags } => {
                if tags.is_empty() {
                    format!("tailnet:{login}")
                } else {
                    format!("tailnet:{node}")
                }
            }
            SuperadminSource::Access { name, .. } => format!("access:{name}"),
            SuperadminSource::Dev { email } => format!("dev:{email}"),
        }
    }

    /// Sent by the browser on its own (a tailnet connection; Access's
    /// `CF_Authorization` cookie; no credential at all, for dev): writes
    /// need the CSRF defences.
    pub fn is_ambient(&self) -> bool {
        matches!(
            self,
            SuperadminSource::Tailnet { .. }
                | SuperadminSource::Access { .. }
                | SuperadminSource::Dev { .. }
        )
    }
}

/// A caller with the unix socket's reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Superadmin {
    pub source: SuperadminSource,
    /// Who it acts as: the isb user its tailnet login names (with every org,
    /// as a platform admin), or a synthetic principal (`user.id` 0, email the
    /// source's label). Its kind is [`PrincipalKind::Superadmin`].
    pub principal: Principal,
}

impl Superadmin {
    /// A superadmin with no isb account of its own.
    pub fn synthetic(source: SuperadminSource) -> Superadmin {
        let label = source.label();
        Superadmin {
            principal: Principal {
                user: User {
                    id: 0,
                    email: label.clone(),
                    name: label,
                    platform_admin: true,
                    created_at: 0,
                    disabled: false,
                    has_password: false,
                },
                kind: PrincipalKind::Superadmin {
                    source: source.clone(),
                },
                orgs: Vec::new(),
                platform_admin: true,
                downscoped: None,
            },
            source,
        }
    }

    /// Acting as `user` (enabled), with its memberships.
    pub fn as_user(source: SuperadminSource, user: User, store: &AuthStore) -> AuthResult<Self> {
        let orgs = store
            .memberships(user.id)?
            .into_iter()
            .map(|m| (m.org, m.role))
            .collect();
        Ok(Superadmin {
            principal: Principal {
                user,
                kind: PrincipalKind::Superadmin {
                    source: source.clone(),
                },
                orgs,
                platform_admin: true,
                downscoped: None,
            },
            source,
        })
    }

    /// Has an isb account (sessions, passkeys, tokens of its own).
    pub fn has_account(&self) -> bool {
        self.principal.user.id > 0
    }

    pub fn label(&self) -> String {
        self.source.label()
    }
}

/// A superadmin token's metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SuperadminToken {
    pub id: i64,
    pub name: String,
    pub created_at: i64,
    pub last_used: Option<i64>,
    pub expires_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct NewSuperadminToken {
    pub token: String,
    pub info: SuperadminToken,
}

const COLS: &str = "id, name, created_at, last_used, expires_at";

fn row(r: &rusqlite::Row) -> rusqlite::Result<SuperadminToken> {
    Ok(SuperadminToken {
        id: r.get(0)?,
        name: r.get(1)?,
        created_at: r.get(2)?,
        last_used: r.get(3)?,
        expires_at: r.get(4)?,
    })
}

impl AuthStore {
    /// Mint a superadmin token. Only the host CLI calls this (it opens
    /// `isb.db` as the daemon's own user); no HTTP endpoint or tool does.
    pub fn create_superadmin_token(
        &self,
        name: &str,
        expires: Option<Duration>,
    ) -> AuthResult<NewSuperadminToken> {
        let name = name.trim();
        if name.is_empty()
            || name.chars().count() > 64
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        {
            return Err(AuthError::Invalid(
                "superadmin token name: 1 to 64 of [A-Za-z0-9._-]".into(),
            ));
        }
        let (token, hash) = secret::new_token(TokenKind::Superadmin)?;
        let now = self.now();
        let expires_at = expires.map(|d| now + d.as_secs() as i64);
        let db = self.db();
        let r = db.execute(
            "INSERT INTO superadmin_tokens (token_hash, name, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![hash, name, now, expires_at],
        );
        match r {
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                return Err(AuthError::Conflict(format!(
                    "a superadmin token named {name} exists; revoke it or pick another name"
                )));
            }
            Err(e) => return Err(e.into()),
        }
        Ok(NewSuperadminToken {
            token,
            info: SuperadminToken {
                id: db.last_insert_rowid(),
                name: name.to_string(),
                created_at: now,
                last_used: None,
                expires_at,
            },
        })
    }

    /// The token behind `isb_sa_...`, if it is valid and unexpired.
    pub fn authenticate_superadmin_token(
        &self,
        token: &str,
    ) -> AuthResult<Option<SuperadminToken>> {
        if !secret::well_formed(token, TokenKind::Superadmin) {
            return Ok(None);
        }
        let hash = secret::hash_token(token);
        let now = self.now();
        let found = self
            .db()
            .query_row(
                &format!("SELECT token_hash, {COLS} FROM superadmin_tokens WHERE token_hash = ?1"),
                [&hash],
                |r| Ok((r.get::<_, Vec<u8>>(0)?, row_at(r)?)),
            )
            .optional()?;
        let Some((stored, t)) = found else {
            return Ok(None);
        };
        if !secret::ct_eq(&stored, &hash) || t.expires_at.is_some_and(|e| now >= e) {
            return Ok(None);
        }
        if t.last_used.is_none_or(|l| now - l >= TOUCH_EVERY) {
            self.db().execute(
                "UPDATE superadmin_tokens SET last_used = ?2 WHERE id = ?1",
                params![t.id, now],
            )?;
        }
        Ok(Some(t))
    }

    pub fn list_superadmin_tokens(&self) -> AuthResult<Vec<SuperadminToken>> {
        let db = self.db();
        let mut st = db.prepare(&format!("SELECT {COLS} FROM superadmin_tokens ORDER BY id"))?;
        let rows = st.query_map([], row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn superadmin_token(&self, id: i64) -> AuthResult<SuperadminToken> {
        self.db()
            .query_row(
                &format!("SELECT {COLS} FROM superadmin_tokens WHERE id = ?1"),
                [id],
                row,
            )
            .optional()?
            .ok_or_else(|| AuthError::NotFound(format!("superadmin token {id}")))
    }

    /// Delete one. True if it existed.
    pub fn revoke_superadmin_token(&self, id: i64) -> AuthResult<bool> {
        let n = self
            .db()
            .execute("DELETE FROM superadmin_tokens WHERE id = ?1", [id])?;
        Ok(n > 0)
    }
}

/// [`row`] past the hash column.
fn row_at(r: &rusqlite::Row) -> rusqlite::Result<SuperadminToken> {
    Ok(SuperadminToken {
        id: r.get(1)?,
        name: r.get(2)?,
        created_at: r.get(3)?,
        last_used: r.get(4)?,
        expires_at: r.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthConfig;

    #[test]
    fn tokens_mint_authenticate_expire_and_revoke() {
        let s = AuthStore::in_memory(AuthConfig::default()).unwrap();
        let t = s.create_superadmin_token("agent", None).unwrap();
        assert!(t.token.starts_with("isb_sa_"));
        let got = s.authenticate_superadmin_token(&t.token).unwrap().unwrap();
        assert_eq!(got.name, "agent");
        assert!(got.last_used.is_some() || s.superadmin_token(got.id).unwrap().last_used.is_some());
        // Names are unique; bad ones refused.
        assert!(matches!(
            s.create_superadmin_token("agent", None),
            Err(AuthError::Conflict(_))
        ));
        assert!(s.create_superadmin_token("has space", None).is_err());
        assert!(s.create_superadmin_token("", None).is_err());
        // An API token string is not a superadmin token, nor a tampered one.
        assert!(
            s.authenticate_superadmin_token(&t.token.replace("isb_sa_", "isb_tok_"))
                .unwrap()
                .is_none()
        );
        let mut bad = t.token.clone();
        bad.pop();
        bad.push(if t.token.ends_with('A') { 'B' } else { 'A' });
        assert!(s.authenticate_superadmin_token(&bad).unwrap().is_none());
        // Expired.
        let e = s
            .create_superadmin_token("short", Some(Duration::from_secs(0)))
            .unwrap();
        assert!(s.authenticate_superadmin_token(&e.token).unwrap().is_none());
        assert_eq!(s.list_superadmin_tokens().unwrap().len(), 2);
        assert!(s.revoke_superadmin_token(got.id).unwrap());
        assert!(!s.revoke_superadmin_token(got.id).unwrap());
        assert!(s.authenticate_superadmin_token(&t.token).unwrap().is_none());
    }

    #[test]
    fn labels_and_synthetic_principals() {
        let tok = SuperadminSource::Token {
            id: 1,
            name: "ci".into(),
        };
        assert_eq!(tok.label(), "token:ci");
        assert!(!tok.is_ambient());
        let person = SuperadminSource::Tailnet {
            login: "a@example.com".into(),
            node: "laptop.tail1.ts.net".into(),
            tags: vec![],
        };
        assert_eq!(person.label(), "tailnet:a@example.com");
        assert!(person.is_ambient());
        let tagged = SuperadminSource::Tailnet {
            login: "tagged-devices".into(),
            node: "agent-1.tail1.ts.net".into(),
            tags: vec!["tag:agents".into()],
        };
        assert_eq!(tagged.label(), "tailnet:agent-1.tail1.ts.net");
        let access = SuperadminSource::Access {
            name: "a@example.com".into(),
            service_token: false,
        };
        assert_eq!(access.label(), "access:a@example.com");
        assert!(access.is_ambient());
        let dev = SuperadminSource::Dev {
            email: "dev@dev.com".into(),
        };
        assert_eq!(dev.label(), "dev:dev@dev.com");
        assert!(dev.is_ambient());
        let s = Superadmin::synthetic(tagged);
        assert!(!s.has_account());
        assert!(s.principal.platform_admin);
        assert_eq!(s.principal.user.email, "tailnet:agent-1.tail1.ts.net");
    }
}
