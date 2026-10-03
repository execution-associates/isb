//! Ways in besides a password: external identities (OAuth/OIDC) and
//! passkeys, and the rules that tie them to users.
//!
//! Signing in with a provider ([`AuthStore::external_sign_in`]):
//! 1. a known identity (provider, subject) signs its user in;
//! 2. else a **verified** email that matches a user links the identity to
//!    that user and signs them in;
//! 3. else, with a verified email, an account is created only when the
//!    email has a pending invitation (the invitation token may ride the
//!    flow, and must then be for that email) or open sign-up is on. Every
//!    pending invitation for the email is accepted. Never before first-run
//!    setup.
//!
//! An unverified email never links and never signs up. A user always keeps
//! one way in: the last of password, identities and passkeys cannot be
//! removed.

use rusqlite::{OptionalExtension, Row, TransactionBehavior, params};
use serde::Serialize;

use super::secret::{self, TokenKind};
use super::{
    AuthError, AuthResult, AuthStore, Role, USER_COLS, User, clean_name, ensure_org_tx,
    normalize_email, org_col, role_col, user_row,
};
use crate::org::OrgId;

/// What a provider says about the person signing in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalIdentity {
    /// `github`, `google`, `oidc:<issuer>`.
    pub provider: String,
    pub subject: String,
    pub email: Option<String>,
    pub email_verified: bool,
    pub name: Option<String>,
}

/// A linked identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Identity {
    pub id: i64,
    pub user_id: i64,
    pub provider: String,
    pub subject: String,
    pub email: Option<String>,
    pub email_verified: bool,
    pub created_at: i64,
    pub last_used: Option<i64>,
}

/// How an external sign-in found its user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignIn {
    /// The identity was already linked.
    Existing,
    /// Linked to the user with the same verified email.
    Linked,
    /// A new account (by invitation, or open sign-up).
    Created,
}

/// A registered passkey (the public key stays in the store).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Passkey {
    pub id: i64,
    pub user_id: i64,
    /// base64url, as the browser knows it.
    pub credential_id: String,
    pub name: String,
    pub alg: i64,
    pub sign_count: u32,
    pub transports: Vec<String>,
    /// The authenticator model's AAGUID, hex (all zeros for many).
    pub aaguid: String,
    pub created_at: i64,
    pub last_used: Option<i64>,
}

/// A passkey with what verifying an assertion needs.
#[derive(Debug, Clone)]
pub struct StoredPasskey {
    pub passkey: Passkey,
    pub user_handle: Vec<u8>,
    pub public_key: Vec<u8>,
}

const IDENTITY_COLS: &str =
    "id, user_id, provider, subject, email, email_verified, created_at, last_used";

fn identity_row(r: &Row) -> rusqlite::Result<Identity> {
    Ok(Identity {
        id: r.get(0)?,
        user_id: r.get(1)?,
        provider: r.get(2)?,
        subject: r.get(3)?,
        email: r.get(4)?,
        email_verified: r.get(5)?,
        created_at: r.get(6)?,
        last_used: r.get(7)?,
    })
}

const PASSKEY_COLS: &str = "id, user_id, credential_id, name, alg, sign_count, transports, aaguid, created_at, last_used, user_handle, public_key";

fn passkey_row(r: &Row) -> rusqlite::Result<StoredPasskey> {
    let cred: Vec<u8> = r.get(2)?;
    let transports: String = r.get(6)?;
    let count: i64 = r.get(5)?;
    Ok(StoredPasskey {
        passkey: Passkey {
            id: r.get(0)?,
            user_id: r.get(1)?,
            credential_id: super::webauthn::b64(&cred),
            name: r.get(3)?,
            alg: r.get(4)?,
            sign_count: count.clamp(0, u32::MAX as i64) as u32,
            transports: serde_json::from_str(&transports).unwrap_or_default(),
            aaguid: r.get(7)?,
            created_at: r.get(8)?,
            last_used: r.get(9)?,
        },
        user_handle: r.get(10)?,
        public_key: r.get(11)?,
    })
}

fn refused(code: &'static str, message: impl Into<String>) -> AuthError {
    AuthError::Refused {
        code,
        message: message.into(),
    }
}

/// How many ways in a user has: a password, identities, passkeys.
fn ways_in(conn: &rusqlite::Connection, user_id: i64) -> AuthResult<i64> {
    Ok(conn.query_row(
        "SELECT (SELECT COUNT(*) FROM users WHERE id = ?1 AND password_hash IS NOT NULL)
              + (SELECT COUNT(*) FROM user_identities WHERE user_id = ?1)
              + (SELECT COUNT(*) FROM passkeys WHERE user_id = ?1)",
        [user_id],
        |r| r.get(0),
    )?)
}

const LAST_WAY_IN: &str =
    "this is your last way to sign in; add a password, a passkey or another provider first";

impl AuthStore {
    // ---- external identities ----

    /// Sign in with a provider's identity, by the rules in the module docs.
    /// `invite` is an invitation token carried through the flow.
    pub fn external_sign_in(
        &self,
        ext: &ExternalIdentity,
        invite: Option<&str>,
        open_signup: bool,
    ) -> AuthResult<(User, SignIn)> {
        if ext.provider.is_empty() || ext.subject.is_empty() {
            return Err(AuthError::Internal(
                "external identity without a subject".into(),
            ));
        }
        let email = ext.email.as_deref().and_then(|e| normalize_email(e).ok());
        let verified = ext.email_verified && email.is_some();
        let now = self.now();
        let mut db = self.db();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;

        // 1. A known identity.
        let known: Option<(i64, i64)> = tx
            .query_row(
                "SELECT id, user_id FROM user_identities WHERE provider = ?1 AND subject = ?2",
                params![ext.provider, ext.subject],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((iid, uid)) = known {
            let user = user_in(&tx, uid)?;
            if user.disabled {
                return Err(refused("account_disabled", "this account is disabled"));
            }
            tx.execute(
                "UPDATE user_identities SET email = ?2, email_verified = ?3, last_used = ?4 WHERE id = ?1",
                params![iid, email, verified, now],
            )?;
            tx.commit()?;
            return Ok((user, SignIn::Existing));
        }

        // Nothing below happens on an unverified email.
        let Some(email) = email.filter(|_| verified) else {
            return Err(refused(
                "unverified_email",
                "your account with this provider has no verified email address; verify one there (for GitHub, the primary address) and try again",
            ));
        };

        // 2. A user with this verified email.
        let existing: Option<User> = tx
            .query_row(
                &format!("SELECT {USER_COLS} FROM users u WHERE u.email = ?1"),
                [&email],
                |r| user_row(r, 0),
            )
            .optional()?;
        if let Some(user) = existing {
            if user.disabled {
                return Err(refused("account_disabled", "this account is disabled"));
            }
            insert_identity(&tx, user.id, ext, Some(&email), true, now)?;
            tx.commit()?;
            return Ok((user, SignIn::Linked));
        }

        // 3. A new account, by invitation or open sign-up.
        let users: i64 = tx.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?;
        if users == 0 {
            return Err(refused(
                "setup_required",
                "isb has no admin yet; finish first-run setup before signing in with a provider",
            ));
        }
        if let Some(token) = invite {
            let inv = invitation_by_token(&tx, token, now)?
                .ok_or(AuthError::InvalidToken("invitation"))?;
            if inv.1 != email {
                return Err(refused(
                    "invitation_mismatch",
                    format!(
                        "this invitation is for {}, and your provider account's verified email is {email}",
                        inv.1
                    ),
                ));
            }
        }
        let pending = pending_invitations(&tx, &email, now)?;
        if pending.is_empty() && !open_signup {
            return Err(refused(
                "signup_closed",
                format!("{email} has no account here; ask an org admin for an invitation"),
            ));
        }
        let name = ext
            .name
            .as_deref()
            .map(|n| {
                n.chars()
                    .filter(|c| !c.is_control())
                    .take(100)
                    .collect::<String>()
            })
            .and_then(|n| clean_name(&n).ok())
            .unwrap_or_default();
        tx.execute(
            "INSERT INTO users (email, name, created_at) VALUES (?1, ?2, ?3)",
            params![email, name, now],
        )?;
        let uid = tx.last_insert_rowid();
        insert_identity(&tx, uid, ext, Some(&email), true, now)?;
        for (id, org, role) in pending {
            tx.execute(
                "UPDATE invitations SET accepted_at = ?2, accepted_by = ?3 WHERE id = ?1",
                params![id, now, uid],
            )?;
            ensure_org_tx(&tx, &org, now)?;
            tx.execute(
                "INSERT INTO memberships (user_id, org, role, created_at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (user_id, org) DO NOTHING",
                params![uid, org.as_str(), role.as_str(), now],
            )?;
        }
        let user = user_in(&tx, uid)?;
        tx.commit()?;
        Ok((user, SignIn::Created))
    }

    /// Link an identity to a signed-in user. Refused when it already
    /// belongs to someone else.
    pub fn link_identity(&self, user_id: i64, ext: &ExternalIdentity) -> AuthResult<Identity> {
        let email = ext.email.as_deref().and_then(|e| normalize_email(e).ok());
        let verified = ext.email_verified && email.is_some();
        let now = self.now();
        let mut db = self.db();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let owner: Option<i64> = tx
            .query_row(
                "SELECT user_id FROM user_identities WHERE provider = ?1 AND subject = ?2",
                params![ext.provider, ext.subject],
                |r| r.get(0),
            )
            .optional()?;
        match owner {
            Some(u) if u != user_id => {
                return Err(refused(
                    "identity_taken",
                    "that provider account is already linked to another isb user",
                ));
            }
            Some(_) => {
                tx.execute(
                    "UPDATE user_identities SET email = ?3, email_verified = ?4, last_used = ?5
                     WHERE provider = ?1 AND subject = ?2",
                    params![ext.provider, ext.subject, email, verified, now],
                )?;
            }
            None => insert_identity(&tx, user_id, ext, email.as_deref(), verified, now)?,
        }
        let id = tx.query_row(
            &format!(
                "SELECT {IDENTITY_COLS} FROM user_identities WHERE provider = ?1 AND subject = ?2"
            ),
            params![ext.provider, ext.subject],
            identity_row,
        )?;
        tx.commit()?;
        Ok(id)
    }

    pub fn list_identities(&self, user_id: i64) -> AuthResult<Vec<Identity>> {
        let db = self.db();
        let mut st = db.prepare(&format!(
            "SELECT {IDENTITY_COLS} FROM user_identities WHERE user_id = ?1 ORDER BY id"
        ))?;
        let rows = st.query_map([user_id], identity_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Remove one of a user's identities, unless it is their last way in.
    /// False when there was no such identity.
    pub fn unlink_identity(&self, user_id: i64, id: i64) -> AuthResult<bool> {
        let mut db = self.db();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = tx
            .query_row(
                "SELECT 1 FROM user_identities WHERE id = ?1 AND user_id = ?2",
                params![id, user_id],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if !exists {
            return Ok(false);
        }
        if ways_in(&tx, user_id)? <= 1 {
            return Err(AuthError::Conflict(LAST_WAY_IN.into()));
        }
        tx.execute("DELETE FROM user_identities WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(true)
    }

    // ---- passkeys ----

    /// The WebAuthn user handle of `user_id`'s passkeys, if they have any.
    pub fn passkey_user_handle(&self, user_id: i64) -> AuthResult<Option<Vec<u8>>> {
        Ok(self
            .db()
            .query_row(
                "SELECT user_handle FROM passkeys WHERE user_id = ?1 ORDER BY id LIMIT 1",
                [user_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Store a verified registration.
    pub fn add_passkey(
        &self,
        user_id: i64,
        user_handle: &[u8],
        reg: &super::webauthn::Registration,
        name: &str,
        transports: &[String],
    ) -> AuthResult<Passkey> {
        let name: String = name
            .trim()
            .chars()
            .filter(|c| !c.is_control())
            .take(100)
            .collect();
        let transports: Vec<&String> = transports
            .iter()
            .filter(|t| t.len() <= 32 && t.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
            .take(8)
            .collect();
        let aaguid: String = reg.aaguid.iter().map(|b| format!("{b:02x}")).collect();
        let now = self.now();
        let db = self.db();
        let r = db.execute(
            "INSERT INTO passkeys (credential_id, user_id, user_handle, public_key, alg, sign_count,
                                   transports, aaguid, name, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                reg.credential_id,
                user_id,
                user_handle,
                reg.public_key_cose,
                reg.alg,
                i64::from(reg.sign_count),
                serde_json::to_string(&transports).unwrap_or_else(|_| "[]".into()),
                aaguid,
                name,
                now
            ],
        );
        match r {
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                return Err(AuthError::Conflict(
                    "that passkey is already registered".into(),
                ));
            }
            Err(e) => return Err(e.into()),
        }
        let id = db.last_insert_rowid();
        Ok(db
            .query_row(
                &format!("SELECT {PASSKEY_COLS} FROM passkeys WHERE id = ?1"),
                [id],
                passkey_row,
            )?
            .passkey)
    }

    pub fn passkey_by_credential(&self, credential_id: &[u8]) -> AuthResult<Option<StoredPasskey>> {
        Ok(self
            .db()
            .query_row(
                &format!("SELECT {PASSKEY_COLS} FROM passkeys WHERE credential_id = ?1"),
                [credential_id],
                passkey_row,
            )
            .optional()?)
    }

    pub fn list_passkeys(&self, user_id: i64) -> AuthResult<Vec<Passkey>> {
        let db = self.db();
        let mut st = db.prepare(&format!(
            "SELECT {PASSKEY_COLS} FROM passkeys WHERE user_id = ?1 ORDER BY id"
        ))?;
        let rows = st.query_map([user_id], passkey_row)?;
        Ok(rows
            .map(|r| r.map(|p| p.passkey))
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Record a sign-in with a passkey: the new counter, if nobody else
    /// moved it meanwhile (two racing assertions cannot both pass).
    pub fn use_passkey(&self, id: i64, old_count: u32, new_count: u32) -> AuthResult<()> {
        let n = self.db().execute(
            "UPDATE passkeys SET sign_count = ?3, last_used = ?4 WHERE id = ?1 AND sign_count = ?2",
            params![id, i64::from(old_count), i64::from(new_count), self.now()],
        )?;
        if n == 0 {
            return Err(AuthError::PasskeyRejected(
                "the signature counter moved during sign-in".into(),
            ));
        }
        Ok(())
    }

    /// Remove one of a user's passkeys, unless it is their last way in.
    pub fn delete_passkey(&self, user_id: i64, id: i64) -> AuthResult<bool> {
        let mut db = self.db();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = tx
            .query_row(
                "SELECT 1 FROM passkeys WHERE id = ?1 AND user_id = ?2",
                params![id, user_id],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if !exists {
            return Ok(false);
        }
        if ways_in(&tx, user_id)? <= 1 {
            return Err(AuthError::Conflict(LAST_WAY_IN.into()));
        }
        tx.execute("DELETE FROM passkeys WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(true)
    }
}

fn user_in(conn: &rusqlite::Connection, id: i64) -> AuthResult<User> {
    conn.query_row(
        &format!("SELECT {USER_COLS} FROM users u WHERE u.id = ?1"),
        [id],
        |r| user_row(r, 0),
    )
    .optional()?
    .ok_or_else(|| AuthError::NotFound(format!("user {id}")))
}

fn insert_identity(
    conn: &rusqlite::Connection,
    user_id: i64,
    ext: &ExternalIdentity,
    email: Option<&str>,
    verified: bool,
    now: i64,
) -> AuthResult<()> {
    conn.execute(
        "INSERT INTO user_identities (user_id, provider, subject, email, email_verified, created_at, last_used)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
        params![user_id, ext.provider, ext.subject, email, verified, now],
    )?;
    Ok(())
}

/// A pending invitation by token: `(id, email)`.
fn invitation_by_token(
    conn: &rusqlite::Connection,
    token: &str,
    now: i64,
) -> AuthResult<Option<(i64, String)>> {
    if !secret::well_formed(token, TokenKind::Invitation) {
        return Ok(None);
    }
    let hash = secret::hash_token(token);
    // (hash, id, email, expires, accepted)
    type InvRow = (Vec<u8>, i64, String, i64, Option<i64>);
    let found: Option<InvRow> = conn
        .query_row(
            "SELECT token_hash, id, email, expires_at, accepted_at FROM invitations WHERE token_hash = ?1",
            [&hash],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    Ok(found.and_then(|(stored, id, email, expires, accepted)| {
        (secret::ct_eq(&stored, &hash) && accepted.is_none() && now < expires)
            .then_some((id, email))
    }))
}

/// Every pending invitation for `email`: `(id, org, role)`.
fn pending_invitations(
    conn: &rusqlite::Connection,
    email: &str,
    now: i64,
) -> AuthResult<Vec<(i64, OrgId, Role)>> {
    let mut st = conn.prepare(
        "SELECT id, org, role FROM invitations
         WHERE email = ?1 AND accepted_at IS NULL AND expires_at > ?2 ORDER BY id",
    )?;
    let rows = st.query_map(params![email, now], |r| {
        Ok((r.get(0)?, org_col(r, 1)?, role_col(r, 2)?))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests;
