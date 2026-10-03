//! Identity for `isb serve`: users, org memberships and roles, browser
//! sessions, invitations, API tokens and password resets, in SQLite at
//! `<state>/isb.db`.
//!
//! - Orgs are the trust boundary. A user is a member of an org with a
//!   [`Role`]; what a role may do is the table in [`Role::permissions`], so
//!   roles can be refined without touching the callers. A platform admin
//!   spans orgs.
//! - Every bearer secret (session, API token, invitation, reset) is 32 random
//!   bytes shown once; only its SHA-256 is stored ([`secret`]).
//! - Passwords are argon2id. Login failures never say which half was wrong,
//!   cost the same either way, and are rate-limited per email and per IP.
//! - The HTTP endpoints are in [`http`]; the CLI uses this API directly on the
//!   same file (SQLite in WAL mode handles the daemon and the CLI at once).
//!
//! External sign-in (OAuth/OIDC, [`oauth`]) attaches rows to
//! `user_identities`; passkeys ([`webauthn`]) live in `passkeys`. Both are
//! managed in [`external`].

pub mod cbor;
pub mod db;
pub mod external;
pub mod http;
pub mod limit;
pub mod oauth;
pub mod oidc;
pub mod secret;
pub mod webauthn;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::org::OrgId;
use limit::{Rate, RateLimiter};
use secret::{PasswordCost, TokenKind};

/// What the identity store can fail with. [`AuthError::InvalidCredentials`]
/// is deliberately vague: it is the answer to every failed login.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("invalid email or password")]
    InvalidCredentials,
    /// A presented invitation or reset token that is unknown, used or expired.
    #[error("{0} is invalid or has expired")]
    InvalidToken(&'static str),
    #[error("too many attempts; try again in {retry_after}s")]
    RateLimited { retry_after: u64 },
    #[error("{0}")]
    Forbidden(String),
    #[error("{0} not found")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Internal(String),
    /// An external sign-in that proved who the user is, but may not go on
    /// (no verified email, no invitation, a disabled account). `code` is
    /// stable, for the login page.
    #[error("{message}")]
    Refused { code: &'static str, message: String },
    /// A passkey assertion or registration that did not verify.
    #[error("passkey rejected: {0}")]
    PasskeyRejected(String),
    #[error("identity database: {0}")]
    Db(#[from] rusqlite::Error),
}

impl AuthError {
    pub(crate) fn io(step: String, e: std::io::Error) -> Self {
        AuthError::Internal(format!("{step}: {e}"))
    }
}

impl From<AuthError> for crate::Error {
    fn from(e: AuthError) -> Self {
        match e {
            AuthError::NotFound(s) => crate::Error::NotFound(s),
            e => crate::Error::Invalid(e.to_string()),
        }
    }
}

pub type AuthResult<T> = std::result::Result<T, AuthError>;

/// A role in an org. Ordered by reach: an actor may grant roles up to its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Member,
    Admin,
    Owner,
}

/// Something a role allows within its org.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Permission {
    /// Apps, stacks, sandboxes, secrets (read included), deploys.
    AdminOrg,
    /// Add, change and remove members; invitations; every token in the org.
    ManageMembers,
    /// Delete the org.
    DeleteOrg,
}

impl Role {
    pub const ALL: [Role; 3] = [Role::Member, Role::Admin, Role::Owner];

    /// What each role may do. The org is the boundary, so for now every
    /// member administers what is in it.
    pub fn permissions(self) -> &'static [Permission] {
        use Permission::*;
        match self {
            Role::Member => &[AdminOrg],
            Role::Admin => &[AdminOrg, ManageMembers],
            Role::Owner => &[AdminOrg, ManageMembers, DeleteOrg],
        }
    }

    pub fn can(self, p: Permission) -> bool {
        self.permissions().contains(&p)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Role::Member => "member",
            Role::Admin => "admin",
            Role::Owner => "owner",
        }
    }

    pub fn parse(s: &str) -> AuthResult<Role> {
        Role::ALL
            .into_iter()
            .find(|r| r.as_str() == s)
            .ok_or_else(|| AuthError::Invalid(format!("role {s:?}: owner, admin or member")))
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A user, without the password hash (which never leaves the store).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct User {
    pub id: i64,
    pub email: String,
    pub name: String,
    pub platform_admin: bool,
    pub created_at: i64,
    pub disabled: bool,
    /// False for a user who signs in only through an external identity.
    pub has_password: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Membership {
    pub org: OrgId,
    pub role: Role,
}

/// A browser session. `expires_at` is the absolute limit; it also ends when
/// unused for the configured idle time ([`Session::idle_expires_at`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Session {
    pub id: i64,
    pub user_id: i64,
    pub created_at: i64,
    pub last_seen: i64,
    pub expires_at: i64,
    pub idle_expires_at: i64,
    pub user_agent: Option<String>,
    pub ip: Option<String>,
}

/// A session just created: the token is in hand only now.
#[derive(Debug, Clone)]
pub struct NewSession {
    pub token: String,
    pub session: Session,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Invitation {
    pub id: i64,
    pub org: OrgId,
    pub email: String,
    pub role: Role,
    /// `None` when made by the local CLI (or the inviter was deleted).
    pub invited_by: Option<i64>,
    pub created_at: i64,
    pub expires_at: i64,
    pub accepted_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct NewInvitation {
    pub token: String,
    pub invitation: Invitation,
}

/// What accepting an invitation did.
#[derive(Debug, Clone)]
pub struct Accepted {
    pub user: User,
    pub membership: Membership,
    /// True when the invitation created the account.
    pub created: bool,
}

/// An API token's metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApiToken {
    pub id: i64,
    pub name: String,
    pub user_id: i64,
    /// `None`: a platform token, acting with all of its (platform admin)
    /// owner's access. `Some`: confined to that org.
    pub org: Option<OrgId>,
    pub created_at: i64,
    pub last_used: Option<i64>,
    pub expires_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct NewApiToken {
    pub token: String,
    pub info: ApiToken,
}

/// How a principal authenticated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PrincipalKind {
    Session { id: i64 },
    ApiToken { id: i64, org: Option<OrgId> },
}

/// An authenticated caller: who, how, and what they may reach. For an
/// org-scoped token, `orgs` is that one org and `platform_admin` is false
/// whatever the user is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub user: User,
    pub kind: PrincipalKind,
    pub orgs: Vec<(OrgId, Role)>,
    pub platform_admin: bool,
}

impl Principal {
    pub fn is_platform_admin(&self) -> bool {
        self.platform_admin
    }

    pub fn role_in(&self, org: &OrgId) -> Option<Role> {
        self.orgs.iter().find(|(o, _)| o == org).map(|(_, r)| *r)
    }

    fn can(&self, org: &OrgId, p: Permission) -> bool {
        self.platform_admin || self.role_in(org).is_some_and(|r| r.can(p))
    }

    /// Apps, stacks, sandboxes and secrets in `org`.
    pub fn can_admin_org(&self, org: &OrgId) -> bool {
        self.can(org, Permission::AdminOrg)
    }

    /// Members, invitations and every token in `org`.
    pub fn can_manage_members(&self, org: &OrgId) -> bool {
        self.can(org, Permission::ManageMembers)
    }

    pub fn can_delete_org(&self, org: &OrgId) -> bool {
        self.can(org, Permission::DeleteOrg)
    }

    /// The highest role this principal may hand out in `org`.
    pub fn max_grant(&self, org: &OrgId) -> Option<Role> {
        if self.platform_admin {
            return Some(Role::Owner);
        }
        self.role_in(org)
            .filter(|r| r.can(Permission::ManageMembers))
    }

    pub fn session_id(&self) -> Option<i64> {
        match self.kind {
            PrincipalKind::Session { id } => Some(id),
            PrincipalKind::ApiToken { .. } => None,
        }
    }
}

/// Where a login came from, kept on the session for the user to review.
#[derive(Debug, Clone, Default)]
pub struct LoginMeta {
    pub user_agent: Option<String>,
    pub ip: Option<String>,
}

/// Lifetimes, cost and rate limits.
#[derive(Debug, Clone)]
pub struct AuthConfig {
    /// A session ends this long after login, used or not.
    pub session_max_age: Duration,
    /// A session ends after this long unused.
    pub session_idle: Duration,
    pub invitation_ttl: Duration,
    pub reset_ttl: Duration,
    pub password_cost: PasswordCost,
    /// Login attempts per email.
    pub login_per_email: Rate,
    /// Unauthenticated attempts (login, setup, invitations, resets) per IP.
    pub attempts_per_ip: Rate,
    /// Password reset requests per email.
    pub resets_per_email: Rate,
}

impl Default for AuthConfig {
    fn default() -> Self {
        AuthConfig {
            session_max_age: Duration::from_secs(30 * 86400),
            session_idle: Duration::from_secs(7 * 86400),
            invitation_ttl: Duration::from_secs(7 * 86400),
            reset_ttl: Duration::from_secs(3600),
            password_cost: PasswordCost::default(),
            login_per_email: Rate::new(5, 60),
            attempts_per_ip: Rate::new(20, 6),
            resets_per_email: Rate::new(3, 900),
        }
    }
}

/// `last_seen`/`last_used` are written at most this often per session or token.
const TOUCH_EVERY: i64 = 60;

pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// The identity store. Cheap to share behind an `Arc`; one connection behind
/// a mutex (requests are short, and argon2 runs outside the lock).
pub struct AuthStore {
    conn: Mutex<Connection>,
    path: Option<PathBuf>,
    cfg: AuthConfig,
    clock: Clock,
    per_email: RateLimiter,
    per_ip: RateLimiter,
    resets: RateLimiter,
    dummy: OnceLock<String>,
}

impl std::fmt::Debug for AuthStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthStore")
            .field("path", &self.path)
            .finish()
    }
}

/// The identity database under a daemon state directory.
pub fn db_path(state_dir: &Path) -> PathBuf {
    state_dir.join("isb.db")
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn secs(d: Duration) -> i64 {
    d.as_secs().min(i64::MAX as u64 / 2) as i64
}

/// Trimmed, lowercased, and plausibly an address. Not RFC 5322: the address
/// is a login name and an invitation target, not something isb parses.
pub fn normalize_email(email: &str) -> AuthResult<String> {
    let e = email.trim().to_lowercase();
    let ok = e.len() <= 254
        && e.split_once('@').is_some_and(|(l, d)| {
            !l.is_empty() && !d.is_empty() && !d.contains('@') && !d.starts_with('.')
        })
        && !e.chars().any(|c| c.is_whitespace() || c.is_control());
    if ok {
        Ok(e)
    } else {
        Err(AuthError::Invalid(format!(
            "{email:?} is not an email address"
        )))
    }
}

pub(crate) fn clean_name(name: &str) -> AuthResult<String> {
    let n = name.trim();
    if n.chars().count() > 100 || n.chars().any(char::is_control) {
        return Err(AuthError::Invalid(
            "name: at most 100 characters, no control characters".into(),
        ));
    }
    Ok(n.to_string())
}

fn clip(s: Option<String>, n: usize) -> Option<String> {
    s.map(|s| s.chars().filter(|c| !c.is_control()).take(n).collect())
}

pub(crate) const USER_COLS: &str = "u.id, u.email, u.name, u.platform_admin, u.created_at, u.disabled, u.password_hash IS NOT NULL";

pub(crate) fn user_row(r: &Row, at: usize) -> rusqlite::Result<User> {
    Ok(User {
        id: r.get(at)?,
        email: r.get(at + 1)?,
        name: r.get(at + 2)?,
        platform_admin: r.get(at + 3)?,
        created_at: r.get(at + 4)?,
        disabled: r.get(at + 5)?,
        has_password: r.get(at + 6)?,
    })
}

pub(crate) fn org_col(r: &Row, i: usize) -> rusqlite::Result<OrgId> {
    let s: String = r.get(i)?;
    OrgId::new(s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn opt_org_col(r: &Row, i: usize) -> rusqlite::Result<Option<OrgId>> {
    match r.get::<_, Option<String>>(i)? {
        None => Ok(None),
        Some(_) => org_col(r, i).map(Some),
    }
}

pub(crate) fn role_col(r: &Row, i: usize) -> rusqlite::Result<Role> {
    let s: String = r.get(i)?;
    Role::parse(&s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, Box::new(e))
    })
}

const INVITATION_COLS: &str =
    "id, org, email, role, invited_by, created_at, expires_at, accepted_at";

fn invitation_row(r: &Row) -> rusqlite::Result<Invitation> {
    Ok(Invitation {
        id: r.get(0)?,
        org: org_col(r, 1)?,
        email: r.get(2)?,
        role: role_col(r, 3)?,
        invited_by: r.get(4)?,
        created_at: r.get(5)?,
        expires_at: r.get(6)?,
        accepted_at: r.get(7)?,
    })
}

const TOKEN_COLS: &str = "id, name, user_id, org, created_at, last_used, expires_at";

fn token_row(r: &Row) -> rusqlite::Result<ApiToken> {
    Ok(ApiToken {
        id: r.get(0)?,
        name: r.get(1)?,
        user_id: r.get(2)?,
        org: opt_org_col(r, 3)?,
        created_at: r.get(4)?,
        last_used: r.get(5)?,
        expires_at: r.get(6)?,
    })
}

impl AuthStore {
    /// Open (creating and migrating) the database at `path`, default config.
    pub fn open(path: impl AsRef<Path>) -> AuthResult<AuthStore> {
        Self::open_with(path, AuthConfig::default())
    }

    pub fn open_with(path: impl AsRef<Path>, cfg: AuthConfig) -> AuthResult<AuthStore> {
        let path = path.as_ref();
        let conn = db::open(path)?;
        Ok(Self::build(conn, Some(path.to_path_buf()), cfg))
    }

    /// A throwaway in-memory store (tests, previews).
    pub fn in_memory(cfg: AuthConfig) -> AuthResult<AuthStore> {
        Ok(Self::build(db::open_in_memory()?, None, cfg))
    }

    fn build(conn: Connection, path: Option<PathBuf>, cfg: AuthConfig) -> AuthStore {
        AuthStore {
            conn: Mutex::new(conn),
            path,
            per_email: RateLimiter::new(cfg.login_per_email),
            per_ip: RateLimiter::new(cfg.attempts_per_ip),
            resets: RateLimiter::new(cfg.resets_per_email),
            cfg,
            clock: Arc::new(unix_now),
            dummy: OnceLock::new(),
        }
    }

    /// Replace the clock (unix seconds), for tests of expiry.
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    pub fn config(&self) -> &AuthConfig {
        &self.cfg
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn now(&self) -> i64 {
        (self.clock)()
    }

    pub(crate) fn db(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn hash_pw(&self, pw: &str) -> AuthResult<String> {
        secret::check_password_policy(pw)?;
        secret::hash_password(pw, self.cfg.password_cost)
    }

    fn rate(&self, l: &RateLimiter, key: &str) -> AuthResult<()> {
        l.take(key, self.now() as f64)
            .map_err(|retry_after| AuthError::RateLimited { retry_after })
    }

    /// Count one unauthenticated attempt from `ip` (login, setup, invitation
    /// acceptance, password resets).
    pub fn limit_ip(&self, ip: Option<&str>) -> AuthResult<()> {
        match ip {
            Some(ip) => self.rate(&self.per_ip, ip),
            None => Ok(()),
        }
    }

    // ---- users ----

    /// True until the first user exists.
    pub fn setup_needed(&self) -> AuthResult<bool> {
        let n: i64 = self
            .db()
            .query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?;
        Ok(n == 0)
    }

    /// Create the first user: a platform admin and owner of the `default`
    /// org. Refused once any user exists.
    pub fn create_first_admin(&self, email: &str, name: &str, password: &str) -> AuthResult<User> {
        let email = normalize_email(email)?;
        let name = clean_name(name)?;
        let hash = self.hash_pw(password)?;
        let now = self.now();
        let mut db = self.db();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let n: i64 = tx.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?;
        if n > 0 {
            return Err(AuthError::Conflict("setup is already done".into()));
        }
        tx.execute(
            "INSERT INTO users (email, name, password_hash, platform_admin, created_at)
             VALUES (?1, ?2, ?3, 1, ?4)",
            params![email, name, hash, now],
        )?;
        let id = tx.last_insert_rowid();
        let org = OrgId::default_org();
        ensure_org_tx(&tx, &org, now)?;
        tx.execute(
            "INSERT INTO memberships (user_id, org, role, created_at) VALUES (?1, ?2, 'owner', ?3)",
            params![id, org.as_str(), now],
        )?;
        tx.commit()?;
        drop(db);
        self.user(id)
    }

    /// Create a user. `password` may be `None` for an account that will sign
    /// in through an external identity or reset its password.
    pub fn create_user(
        &self,
        email: &str,
        name: &str,
        password: Option<&str>,
        platform_admin: bool,
    ) -> AuthResult<User> {
        let email = normalize_email(email)?;
        let name = clean_name(name)?;
        let hash = password.map(|p| self.hash_pw(p)).transpose()?;
        let now = self.now();
        let db = self.db();
        let r = db.execute(
            "INSERT INTO users (email, name, password_hash, platform_admin, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![email, name, hash, platform_admin, now],
        );
        match r {
            Ok(_) => {
                let id = db.last_insert_rowid();
                drop(db);
                self.user(id)
            }
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Err(AuthError::Conflict(format!(
                    "a user with email {email} exists"
                )))
            }
            Err(e) => Err(e.into()),
        }
    }

    pub fn user(&self, id: i64) -> AuthResult<User> {
        self.db()
            .query_row(
                &format!("SELECT {USER_COLS} FROM users u WHERE u.id = ?1"),
                [id],
                |r| user_row(r, 0),
            )
            .optional()?
            .ok_or_else(|| AuthError::NotFound(format!("user {id}")))
    }

    pub fn user_by_email(&self, email: &str) -> AuthResult<Option<User>> {
        let email = normalize_email(email)?;
        Ok(self
            .db()
            .query_row(
                &format!("SELECT {USER_COLS} FROM users u WHERE u.email = ?1"),
                [email],
                |r| user_row(r, 0),
            )
            .optional()?)
    }

    pub fn list_users(&self) -> AuthResult<Vec<User>> {
        let db = self.db();
        let mut st = db.prepare(&format!("SELECT {USER_COLS} FROM users u ORDER BY u.id"))?;
        let rows = st.query_map([], |r| user_row(r, 0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Disable (or re-enable) a user. Disabling ends their sessions; their
    /// tokens stop working while disabled.
    pub fn set_disabled(&self, user_id: i64, disabled: bool) -> AuthResult<()> {
        let db = self.db();
        let n = db.execute(
            "UPDATE users SET disabled = ?2 WHERE id = ?1",
            params![user_id, disabled],
        )?;
        if n == 0 {
            return Err(AuthError::NotFound(format!("user {user_id}")));
        }
        if disabled {
            db.execute("DELETE FROM sessions WHERE user_id = ?1", [user_id])?;
        }
        Ok(())
    }

    pub fn set_platform_admin(&self, user_id: i64, admin: bool) -> AuthResult<()> {
        let n = self.db().execute(
            "UPDATE users SET platform_admin = ?2 WHERE id = ?1",
            params![user_id, admin],
        )?;
        if n == 0 {
            return Err(AuthError::NotFound(format!("user {user_id}")));
        }
        Ok(())
    }

    /// Set a password without the old one (the local CLI, an admin). Ends
    /// every session of the user.
    pub fn set_password(&self, user_id: i64, password: &str) -> AuthResult<()> {
        let hash = self.hash_pw(password)?;
        let db = self.db();
        let n = db.execute(
            "UPDATE users SET password_hash = ?2 WHERE id = ?1",
            params![user_id, hash],
        )?;
        if n == 0 {
            return Err(AuthError::NotFound(format!("user {user_id}")));
        }
        db.execute("DELETE FROM sessions WHERE user_id = ?1", [user_id])?;
        Ok(())
    }

    /// Change a password, proving the current one. Ends every other session
    /// (`keep` is the caller's own).
    pub fn change_password(
        &self,
        user_id: i64,
        current: &str,
        new: &str,
        keep: Option<i64>,
    ) -> AuthResult<()> {
        let stored: Option<String> = self
            .db()
            .query_row(
                "SELECT password_hash FROM users WHERE id = ?1",
                [user_id],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        let ok = match &stored {
            Some(h) => secret::verify_password(current, h),
            None => {
                secret::verify_password(current, self.dummy());
                false
            }
        };
        if !ok {
            return Err(AuthError::Forbidden("current password is wrong".into()));
        }
        let hash = self.hash_pw(new)?;
        let db = self.db();
        db.execute(
            "UPDATE users SET password_hash = ?2 WHERE id = ?1",
            params![user_id, hash],
        )?;
        db.execute(
            "DELETE FROM sessions WHERE user_id = ?1 AND id IS NOT ?2",
            params![user_id, keep],
        )?;
        Ok(())
    }

    fn dummy(&self) -> &str {
        secret::dummy_hash(&self.dummy, self.cfg.password_cost)
    }

    // ---- sessions ----

    /// Check an email and password and start a session. Every failure (no
    /// such user, wrong password, disabled, no password set) is
    /// [`AuthError::InvalidCredentials`] and costs one argon2 verification.
    pub fn login(&self, email: &str, password: &str, meta: LoginMeta) -> AuthResult<NewSession> {
        let key = email.trim().to_lowercase();
        self.limit_ip(meta.ip.as_deref())?;
        self.rate(&self.per_email, &key)?;
        let found: Option<(i64, Option<String>, bool)> = self
            .db()
            .query_row(
                "SELECT id, password_hash, disabled FROM users WHERE email = ?1",
                [&key],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let user_id = match found {
            Some((id, Some(h), disabled)) => {
                let ok = secret::verify_password(password, &h);
                (ok && !disabled).then_some(id)
            }
            _ => {
                secret::verify_password(password, self.dummy());
                None
            }
        };
        let user_id = user_id.ok_or(AuthError::InvalidCredentials)?;
        self.prune()?;
        self.start_session(user_id, meta)
    }

    /// Start a session for a user already proven by other means (an
    /// accepted invitation, a password reset, an external identity).
    pub fn start_session(&self, user_id: i64, meta: LoginMeta) -> AuthResult<NewSession> {
        let (token, hash) = secret::new_token(TokenKind::Session)?;
        let now = self.now();
        let expires = now + secs(self.cfg.session_max_age);
        let (ua, ip) = (clip(meta.user_agent, 256), clip(meta.ip, 64));
        let db = self.db();
        db.execute(
            "INSERT INTO sessions (token_hash, user_id, created_at, last_seen, expires_at, user_agent, ip)
             VALUES (?1, ?2, ?3, ?3, ?4, ?5, ?6)",
            params![hash, user_id, now, expires, ua, ip],
        )?;
        let id = db.last_insert_rowid();
        Ok(NewSession {
            token,
            session: Session {
                id,
                user_id,
                created_at: now,
                last_seen: now,
                expires_at: expires,
                idle_expires_at: self.idle_end(now, expires),
                user_agent: ua,
                ip,
            },
        })
    }

    fn idle_end(&self, last_seen: i64, expires: i64) -> i64 {
        (last_seen + secs(self.cfg.session_idle)).min(expires)
    }

    fn session_row(&self, r: &Row, at: usize) -> rusqlite::Result<Session> {
        let last_seen: i64 = r.get(at + 3)?;
        let expires: i64 = r.get(at + 4)?;
        Ok(Session {
            id: r.get(at)?,
            user_id: r.get(at + 1)?,
            created_at: r.get(at + 2)?,
            last_seen,
            expires_at: expires,
            idle_expires_at: self.idle_end(last_seen, expires),
            user_agent: r.get(at + 5)?,
            ip: r.get(at + 6)?,
        })
    }

    /// The live session for `token`, sliding its idle expiry. An expired one
    /// is deleted; a disabled user has none.
    pub fn session(&self, token: &str) -> AuthResult<Option<(User, Session)>> {
        if !secret::well_formed(token, TokenKind::Session) {
            return Ok(None);
        }
        let hash = secret::hash_token(token);
        let now = self.now();
        let db = self.db();
        let found = db
            .query_row(
                &format!(
                    "SELECT s.token_hash, s.id, s.user_id, s.created_at, s.last_seen, s.expires_at,
                            s.user_agent, s.ip, {USER_COLS}
                     FROM sessions s JOIN users u ON u.id = s.user_id WHERE s.token_hash = ?1"
                ),
                [&hash],
                |r| {
                    Ok((
                        r.get::<_, Vec<u8>>(0)?,
                        self.session_row(r, 1)?,
                        user_row(r, 8)?,
                    ))
                },
            )
            .optional()?;
        let Some((stored, mut s, user)) = found else {
            return Ok(None);
        };
        if !secret::ct_eq(&stored, &hash) {
            return Ok(None);
        }
        if now >= s.expires_at || now >= s.idle_expires_at {
            db.execute("DELETE FROM sessions WHERE id = ?1", [s.id])?;
            return Ok(None);
        }
        if user.disabled {
            return Ok(None);
        }
        if now - s.last_seen >= TOUCH_EVERY {
            db.execute(
                "UPDATE sessions SET last_seen = ?2 WHERE id = ?1",
                params![s.id, now],
            )?;
            s.last_seen = now;
            s.idle_expires_at = self.idle_end(now, s.expires_at);
        }
        Ok(Some((user, s)))
    }

    /// The principal behind a session token.
    pub fn authenticate_session(&self, token: &str) -> AuthResult<Option<Principal>> {
        let Some((user, s)) = self.session(token)? else {
            return Ok(None);
        };
        let orgs = self
            .memberships(user.id)?
            .into_iter()
            .map(|m| (m.org, m.role))
            .collect();
        Ok(Some(Principal {
            platform_admin: user.platform_admin,
            user,
            kind: PrincipalKind::Session { id: s.id },
            orgs,
        }))
    }

    /// End the session holding `token`. True if there was one.
    pub fn logout(&self, token: &str) -> AuthResult<bool> {
        if !secret::well_formed(token, TokenKind::Session) {
            return Ok(false);
        }
        let n = self.db().execute(
            "DELETE FROM sessions WHERE token_hash = ?1",
            [secret::hash_token(token)],
        )?;
        Ok(n > 0)
    }

    /// A user's live sessions, newest first.
    pub fn list_sessions(&self, user_id: i64) -> AuthResult<Vec<Session>> {
        let now = self.now();
        let db = self.db();
        let mut st = db.prepare(
            "SELECT id, user_id, created_at, last_seen, expires_at, user_agent, ip
             FROM sessions WHERE user_id = ?1 ORDER BY id DESC",
        )?;
        let rows = st.query_map([user_id], |r| self.session_row(r, 0))?;
        let all: Vec<Session> = rows.collect::<rusqlite::Result<_>>()?;
        Ok(all
            .into_iter()
            .filter(|s| now < s.expires_at && now < s.idle_expires_at)
            .collect())
    }

    /// End one of a user's sessions. True if it existed.
    pub fn revoke_session(&self, user_id: i64, session_id: i64) -> AuthResult<bool> {
        let n = self.db().execute(
            "DELETE FROM sessions WHERE id = ?1 AND user_id = ?2",
            params![session_id, user_id],
        )?;
        Ok(n > 0)
    }

    /// End all of a user's sessions but `keep`. Returns how many ended.
    pub fn revoke_sessions(&self, user_id: i64, keep: Option<i64>) -> AuthResult<usize> {
        Ok(self.db().execute(
            "DELETE FROM sessions WHERE user_id = ?1 AND id IS NOT ?2",
            params![user_id, keep],
        )?)
    }

    /// Delete expired sessions, invitations and resets.
    pub fn prune(&self) -> AuthResult<()> {
        let now = self.now();
        let idle = secs(self.cfg.session_idle);
        self.db().execute_batch(&format!(
            "DELETE FROM sessions WHERE expires_at <= {now} OR last_seen + {idle} <= {now};
             DELETE FROM invitations WHERE accepted_at IS NULL AND expires_at <= {now};
             DELETE FROM password_resets WHERE expires_at <= {now} OR used_at IS NOT NULL;"
        ))?;
        Ok(())
    }

    // ---- orgs and memberships ----

    /// Record an org (idempotent). The org's runtime is not this module's;
    /// this row anchors memberships, invitations and tokens.
    pub fn ensure_org(&self, org: &OrgId) -> AuthResult<()> {
        ensure_org_tx(&self.db(), org, self.now())
    }

    /// Forget an org: its memberships, invitations and tokens go with it.
    pub fn delete_org(&self, org: &OrgId) -> AuthResult<bool> {
        let n = self
            .db()
            .execute("DELETE FROM orgs WHERE name = ?1", [org.as_str()])?;
        Ok(n > 0)
    }

    pub fn list_orgs(&self) -> AuthResult<Vec<OrgId>> {
        let db = self.db();
        let mut st = db.prepare("SELECT name FROM orgs ORDER BY name")?;
        let rows = st.query_map([], |r| org_col(r, 0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn memberships(&self, user_id: i64) -> AuthResult<Vec<Membership>> {
        let db = self.db();
        let mut st =
            db.prepare("SELECT org, role FROM memberships WHERE user_id = ?1 ORDER BY org")?;
        let rows = st.query_map([user_id], |r| {
            Ok(Membership {
                org: org_col(r, 0)?,
                role: role_col(r, 1)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn list_members(&self, org: &OrgId) -> AuthResult<Vec<(User, Role)>> {
        let db = self.db();
        let mut st = db.prepare(&format!(
            "SELECT {USER_COLS}, m.role FROM memberships m JOIN users u ON u.id = m.user_id
             WHERE m.org = ?1 ORDER BY u.email"
        ))?;
        let rows = st.query_map([org.as_str()], |r| Ok((user_row(r, 0)?, role_col(r, 7)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Add `user` to `org` with `role`, or change their role. Refuses to
    /// demote the org's last owner.
    pub fn set_member(&self, org: &OrgId, user_id: i64, role: Role) -> AuthResult<()> {
        let now = self.now();
        let mut db = self.db();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_org_tx(&tx, org, now)?;
        if role != Role::Owner {
            refuse_last_owner(&tx, org, user_id, "demote")?;
        }
        tx.execute(
            "INSERT INTO memberships (user_id, org, role, created_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (user_id, org) DO UPDATE SET role = excluded.role",
            params![user_id, org.as_str(), role.as_str(), now],
        )
        .map_err(|e| match e {
            rusqlite::Error::SqliteFailure(f, _)
                if f.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                AuthError::NotFound(format!("user {user_id}"))
            }
            e => e.into(),
        })?;
        tx.commit()?;
        Ok(())
    }

    /// Remove `user` from `org`, and their tokens confined to it. Refuses to
    /// remove the last owner. True if they were a member.
    pub fn remove_member(&self, org: &OrgId, user_id: i64) -> AuthResult<bool> {
        let mut db = self.db();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        refuse_last_owner(&tx, org, user_id, "remove")?;
        let n = tx.execute(
            "DELETE FROM memberships WHERE org = ?1 AND user_id = ?2",
            params![org.as_str(), user_id],
        )?;
        tx.execute(
            "DELETE FROM api_tokens WHERE org = ?1 AND user_id = ?2",
            params![org.as_str(), user_id],
        )?;
        tx.commit()?;
        Ok(n > 0)
    }

    // ---- invitations ----

    /// Invite `email` to `org` as `role`. Replaces any pending invitation for
    /// the same address and org. Authorization is the caller's
    /// ([`Principal::max_grant`]); `invited_by` is `None` for the local CLI.
    pub fn create_invitation(
        &self,
        invited_by: Option<i64>,
        org: &OrgId,
        email: &str,
        role: Role,
    ) -> AuthResult<NewInvitation> {
        let email = normalize_email(email)?;
        let (token, hash) = secret::new_token(TokenKind::Invitation)?;
        let now = self.now();
        let expires = now + secs(self.cfg.invitation_ttl);
        let mut db = self.db();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_org_tx(&tx, org, now)?;
        tx.execute(
            "DELETE FROM invitations WHERE org = ?1 AND email = ?2 AND accepted_at IS NULL",
            params![org.as_str(), email],
        )?;
        tx.execute(
            "INSERT INTO invitations (token_hash, org, email, role, invited_by, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![hash, org.as_str(), email, role.as_str(), invited_by, now, expires],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(NewInvitation {
            token,
            invitation: Invitation {
                id,
                org: org.clone(),
                email,
                role,
                invited_by,
                created_at: now,
                expires_at: expires,
                accepted_at: None,
            },
        })
    }

    /// Pending (unaccepted, unexpired) invitations to `org`.
    pub fn list_invitations(&self, org: &OrgId) -> AuthResult<Vec<Invitation>> {
        let db = self.db();
        let mut st = db.prepare(&format!(
            "SELECT {INVITATION_COLS} FROM invitations
             WHERE org = ?1 AND accepted_at IS NULL AND expires_at > ?2 ORDER BY id"
        ))?;
        let rows = st.query_map(params![org.as_str(), self.now()], invitation_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Withdraw a pending invitation. True if there was one.
    pub fn revoke_invitation(&self, org: &OrgId, id: i64) -> AuthResult<bool> {
        let n = self.db().execute(
            "DELETE FROM invitations WHERE id = ?1 AND org = ?2 AND accepted_at IS NULL",
            params![id, org.as_str()],
        )?;
        Ok(n > 0)
    }

    /// The pending invitation for `token`, if it is valid.
    pub fn invitation(&self, token: &str) -> AuthResult<Option<Invitation>> {
        if !secret::well_formed(token, TokenKind::Invitation) {
            return Ok(None);
        }
        let hash = secret::hash_token(token);
        let found = self
            .db()
            .query_row(
                &format!(
                    "SELECT token_hash, {INVITATION_COLS} FROM invitations WHERE token_hash = ?1"
                ),
                [&hash],
                |r| {
                    let stored: Vec<u8> = r.get(0)?;
                    let inv = Invitation {
                        id: r.get(1)?,
                        org: org_col(r, 2)?,
                        email: r.get(3)?,
                        role: role_col(r, 4)?,
                        invited_by: r.get(5)?,
                        created_at: r.get(6)?,
                        expires_at: r.get(7)?,
                        accepted_at: r.get(8)?,
                    };
                    Ok((stored, inv))
                },
            )
            .optional()?;
        Ok(found.and_then(|(stored, inv)| {
            (secret::ct_eq(&stored, &hash)
                && inv.accepted_at.is_none()
                && self.now() < inv.expires_at)
                .then_some(inv)
        }))
    }

    /// Accept an invitation without a session. A new address gets an account
    /// with `name` and `password`; an existing account must prove `password`
    /// (the invitation alone does not let anyone in as someone else).
    pub fn accept_invitation(
        &self,
        token: &str,
        name: &str,
        password: &str,
    ) -> AuthResult<Accepted> {
        let inv = self
            .invitation(token)?
            .ok_or(AuthError::InvalidToken("invitation"))?;
        let existing: Option<(i64, Option<String>, bool)> = self
            .db()
            .query_row(
                "SELECT id, password_hash, disabled FROM users WHERE email = ?1",
                [&inv.email],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let new_user = match &existing {
            Some((_, Some(h), disabled)) => {
                if !secret::verify_password(password, h) || *disabled {
                    return Err(AuthError::InvalidCredentials);
                }
                None
            }
            Some((_, None, _)) => {
                secret::verify_password(password, self.dummy());
                return Err(AuthError::InvalidCredentials);
            }
            None => Some((clean_name(name)?, self.hash_pw(password)?)),
        };
        self.finish_accept(&inv, existing.map(|e| e.0), new_user)
    }

    /// Accept an invitation as the signed-in user, whose email must match.
    pub fn accept_invitation_as(&self, token: &str, user_id: i64) -> AuthResult<Accepted> {
        let inv = self
            .invitation(token)?
            .ok_or(AuthError::InvalidToken("invitation"))?;
        let user = self.user(user_id)?;
        if user.email != inv.email {
            return Err(AuthError::Forbidden(format!(
                "this invitation is for {}, and you are signed in as {}",
                inv.email, user.email
            )));
        }
        self.finish_accept(&inv, Some(user_id), None)
    }

    fn finish_accept(
        &self,
        inv: &Invitation,
        user_id: Option<i64>,
        new_user: Option<(String, String)>,
    ) -> AuthResult<Accepted> {
        let now = self.now();
        let mut db = self.db();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Single use: whoever marks it first wins.
        let n = tx.execute(
            "UPDATE invitations SET accepted_at = ?2 WHERE id = ?1 AND accepted_at IS NULL",
            params![inv.id, now],
        )?;
        if n == 0 {
            return Err(AuthError::InvalidToken("invitation"));
        }
        let (uid, created) = match (user_id, new_user) {
            (Some(id), _) => (id, false),
            (None, Some((name, hash))) => {
                tx.execute(
                    "INSERT INTO users (email, name, password_hash, created_at) VALUES (?1, ?2, ?3, ?4)",
                    params![inv.email, name, hash, now],
                )
                .map_err(|e| match e {
                    rusqlite::Error::SqliteFailure(f, _)
                        if f.code == rusqlite::ErrorCode::ConstraintViolation =>
                    {
                        AuthError::Conflict(format!("a user with email {} exists", inv.email))
                    }
                    e => e.into(),
                })?;
                (tx.last_insert_rowid(), true)
            }
            (None, None) => return Err(AuthError::Internal("accept: no user".into())),
        };
        tx.execute(
            "UPDATE invitations SET accepted_by = ?2 WHERE id = ?1",
            params![inv.id, uid],
        )?;
        // Never lower an existing role by accepting a lesser invitation.
        let current: Option<Role> = tx
            .query_row(
                "SELECT role FROM memberships WHERE user_id = ?1 AND org = ?2",
                params![uid, inv.org.as_str()],
                |r| role_col(r, 0),
            )
            .optional()?;
        let role = current.map_or(inv.role, |c| c.max(inv.role));
        tx.execute(
            "INSERT INTO memberships (user_id, org, role, created_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (user_id, org) DO UPDATE SET role = excluded.role",
            params![uid, inv.org.as_str(), role.as_str(), now],
        )?;
        tx.commit()?;
        drop(db);
        Ok(Accepted {
            user: self.user(uid)?,
            membership: Membership {
                org: inv.org.clone(),
                role,
            },
            created,
        })
    }

    // ---- API tokens ----

    /// Make an API token for `user_id`. Confined to `org` when given (the
    /// user must belong to it, or be a platform admin); a platform token
    /// (`org: None`) is for platform admins only. `expires: None` never expires.
    pub fn create_api_token(
        &self,
        user_id: i64,
        org: Option<&OrgId>,
        name: &str,
        expires: Option<Duration>,
    ) -> AuthResult<NewApiToken> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 100 || name.chars().any(char::is_control) {
            return Err(AuthError::Invalid(
                "token name: 1 to 100 characters, no control characters".into(),
            ));
        }
        let user = self.user(user_id)?;
        if user.disabled {
            return Err(AuthError::Forbidden(format!(
                "user {} is disabled",
                user.email
            )));
        }
        match org {
            None if !user.platform_admin => {
                return Err(AuthError::Forbidden(
                    "only a platform admin can make a token without an org".into(),
                ));
            }
            Some(o) if !user.platform_admin && self.role_of(user_id, o)?.is_none() => {
                return Err(AuthError::Forbidden(format!(
                    "{} is not a member of org {o}",
                    user.email
                )));
            }
            _ => {}
        }
        let (token, hash) = secret::new_token(TokenKind::Api)?;
        let now = self.now();
        let expires_at = expires.map(|d| now + secs(d));
        let db = self.db();
        if let Some(o) = org {
            ensure_org_tx(&db, o, now)?;
        }
        db.execute(
            "INSERT INTO api_tokens (token_hash, name, user_id, org, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![hash, name, user_id, org.map(OrgId::as_str), now, expires_at],
        )?;
        Ok(NewApiToken {
            token,
            info: ApiToken {
                id: db.last_insert_rowid(),
                name: name.to_string(),
                user_id,
                org: org.cloned(),
                created_at: now,
                last_used: None,
                expires_at,
            },
        })
    }

    fn role_of(&self, user_id: i64, org: &OrgId) -> AuthResult<Option<Role>> {
        Ok(self
            .db()
            .query_row(
                "SELECT role FROM memberships WHERE user_id = ?1 AND org = ?2",
                params![user_id, org.as_str()],
                |r| role_col(r, 0),
            )
            .optional()?)
    }

    /// The principal behind an API token. Expired tokens, disabled users, and
    /// org tokens whose user has left the org authenticate nobody.
    pub fn authenticate_token(&self, token: &str) -> AuthResult<Option<Principal>> {
        if !secret::well_formed(token, TokenKind::Api) {
            return Ok(None);
        }
        let hash = secret::hash_token(token);
        let now = self.now();
        let found = self
            .db()
            .query_row(
                &format!(
                    "SELECT t.token_hash, t.id, t.org, t.expires_at, t.last_used, {USER_COLS}
                     FROM api_tokens t JOIN users u ON u.id = t.user_id WHERE t.token_hash = ?1"
                ),
                [&hash],
                |r| {
                    Ok((
                        r.get::<_, Vec<u8>>(0)?,
                        r.get::<_, i64>(1)?,
                        opt_org_col(r, 2)?,
                        r.get::<_, Option<i64>>(3)?,
                        r.get::<_, Option<i64>>(4)?,
                        user_row(r, 5)?,
                    ))
                },
            )
            .optional()?;
        let Some((stored, id, org, expires, last_used, user)) = found else {
            return Ok(None);
        };
        if !secret::ct_eq(&stored, &hash) || expires.is_some_and(|e| now >= e) || user.disabled {
            return Ok(None);
        }
        let (orgs, platform_admin) = match &org {
            Some(o) => {
                let role = match self.role_of(user.id, o)? {
                    Some(r) => r,
                    // A platform admin reaches every org; confined here.
                    None if user.platform_admin => Role::Owner,
                    None => return Ok(None),
                };
                (vec![(o.clone(), role)], false)
            }
            None if user.platform_admin => (
                self.memberships(user.id)?
                    .into_iter()
                    .map(|m| (m.org, m.role))
                    .collect(),
                true,
            ),
            // A platform token whose user is no longer a platform admin.
            None => return Ok(None),
        };
        if last_used.is_none_or(|l| now - l >= TOUCH_EVERY) {
            self.db().execute(
                "UPDATE api_tokens SET last_used = ?2 WHERE id = ?1",
                params![id, now],
            )?;
        }
        Ok(Some(Principal {
            user,
            kind: PrincipalKind::ApiToken { id, org },
            orgs,
            platform_admin,
        }))
    }

    pub fn api_token(&self, id: i64) -> AuthResult<ApiToken> {
        self.db()
            .query_row(
                &format!("SELECT {TOKEN_COLS} FROM api_tokens WHERE id = ?1"),
                [id],
                token_row,
            )
            .optional()?
            .ok_or_else(|| AuthError::NotFound(format!("token {id}")))
    }

    /// A user's tokens.
    pub fn list_api_tokens(&self, user_id: i64) -> AuthResult<Vec<ApiToken>> {
        let db = self.db();
        let mut st = db.prepare(&format!(
            "SELECT {TOKEN_COLS} FROM api_tokens WHERE user_id = ?1 ORDER BY id"
        ))?;
        let rows = st.query_map([user_id], token_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every token confined to `org`, whoever made it.
    pub fn list_org_api_tokens(&self, org: &OrgId) -> AuthResult<Vec<ApiToken>> {
        let db = self.db();
        let mut st = db.prepare(&format!(
            "SELECT {TOKEN_COLS} FROM api_tokens WHERE org = ?1 ORDER BY id"
        ))?;
        let rows = st.query_map([org.as_str()], token_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every token (the local CLI).
    pub fn list_all_api_tokens(&self) -> AuthResult<Vec<ApiToken>> {
        let db = self.db();
        let mut st = db.prepare(&format!("SELECT {TOKEN_COLS} FROM api_tokens ORDER BY id"))?;
        let rows = st.query_map([], token_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Delete a token. True if it existed. Authorization is the caller's.
    pub fn revoke_api_token(&self, id: i64) -> AuthResult<bool> {
        let n = self
            .db()
            .execute("DELETE FROM api_tokens WHERE id = ?1", [id])?;
        Ok(n > 0)
    }

    // ---- password resets ----

    /// A one-hour reset token for `email`, or `None` when there is no such
    /// (enabled) user. The caller delivers it, and must answer the same way
    /// either way so the endpoint does not reveal who has an account.
    pub fn request_password_reset(&self, email: &str) -> AuthResult<Option<String>> {
        let key = email.trim().to_lowercase();
        self.rate(&self.resets, &key)?;
        let Some(user) = self.user_by_email(&key).ok().flatten() else {
            return Ok(None);
        };
        if user.disabled {
            return Ok(None);
        }
        let (token, hash) = secret::new_token(TokenKind::PasswordReset)?;
        let now = self.now();
        let db = self.db();
        // Only the newest link works.
        db.execute("DELETE FROM password_resets WHERE user_id = ?1", [user.id])?;
        db.execute(
            "INSERT INTO password_resets (token_hash, user_id, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![hash, user.id, now, now + secs(self.cfg.reset_ttl)],
        )?;
        Ok(Some(token))
    }

    /// Set a new password with a reset token. Single use; ends every session.
    pub fn reset_password(&self, token: &str, password: &str) -> AuthResult<User> {
        if !secret::well_formed(token, TokenKind::PasswordReset) {
            return Err(AuthError::InvalidToken("reset link"));
        }
        let hash_pw = self.hash_pw(password)?;
        let hash = secret::hash_token(token);
        let now = self.now();
        let mut db = self.db();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // (hash, id, user, expires, used)
        type ResetRow = (Vec<u8>, i64, i64, i64, Option<i64>);
        let found: Option<ResetRow> = tx
            .query_row(
                "SELECT token_hash, id, user_id, expires_at, used_at FROM password_resets
                 WHERE token_hash = ?1",
                [&hash],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        let Some((stored, id, user_id, expires, used)) = found else {
            return Err(AuthError::InvalidToken("reset link"));
        };
        if !secret::ct_eq(&stored, &hash) || used.is_some() || now >= expires {
            return Err(AuthError::InvalidToken("reset link"));
        }
        tx.execute(
            "UPDATE password_resets SET used_at = ?2 WHERE id = ?1",
            params![id, now],
        )?;
        tx.execute(
            "UPDATE users SET password_hash = ?2 WHERE id = ?1",
            params![user_id, hash_pw],
        )?;
        tx.execute("DELETE FROM sessions WHERE user_id = ?1", [user_id])?;
        tx.commit()?;
        drop(db);
        self.user(user_id)
    }
}

pub(crate) fn ensure_org_tx(conn: &Connection, org: &OrgId, now: i64) -> AuthResult<()> {
    conn.execute(
        "INSERT INTO orgs (name, created_at) VALUES (?1, ?2) ON CONFLICT (name) DO NOTHING",
        params![org.as_str(), now],
    )?;
    Ok(())
}

/// An org keeps at least one owner while it has any.
fn refuse_last_owner(conn: &Connection, org: &OrgId, user_id: i64, verb: &str) -> AuthResult<()> {
    let is_owner: bool = conn
        .query_row(
            "SELECT role = 'owner' FROM memberships WHERE org = ?1 AND user_id = ?2",
            params![org.as_str(), user_id],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(false);
    if !is_owner {
        return Ok(());
    }
    let owners: i64 = conn.query_row(
        "SELECT COUNT(*) FROM memberships WHERE org = ?1 AND role = 'owner'",
        [org.as_str()],
        |r| r.get(0),
    )?;
    if owners <= 1 {
        return Err(AuthError::Conflict(format!(
            "cannot {verb} the last owner of org {org}; make someone else owner first"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
