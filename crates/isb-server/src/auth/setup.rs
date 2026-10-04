//! First-run setup in the store: the first user is a platform admin and
//! the owner of the `default` org, made once, on the host, with the setup
//! token, or from an edge identity ([`super::edge`]).

use rusqlite::{TransactionBehavior, params};

use super::{
    AuthError, AuthResult, AuthStore, User, clean_name, ensure_org_tx, external, normalize_email,
};
use crate::org::OrgId;

impl AuthStore {
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
        self.first_admin(email, name, Some(password), None)
    }

    /// The first admin from an edge identity ([`super::edge`]), linked to it so it
    /// signs them in from then on. The password is optional: the edge is a
    /// way in, and `isb user passwd` on the host is the way back.
    pub fn claim_first_admin(
        &self,
        email: &str,
        name: &str,
        password: Option<&str>,
        link: &external::ExternalIdentity,
    ) -> AuthResult<User> {
        self.first_admin(email, name, password, Some(link))
    }

    fn first_admin(
        &self,
        email: &str,
        name: &str,
        password: Option<&str>,
        link: Option<&external::ExternalIdentity>,
    ) -> AuthResult<User> {
        let email = normalize_email(email)?;
        let name = clean_name(name)?;
        let hash = password.map(|p| self.hash_pw(p)).transpose()?;
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
        if let Some(ext) = link {
            let verified = ext.email_verified
                && ext
                    .email
                    .as_deref()
                    .is_some_and(|e| e.eq_ignore_ascii_case(&email));
            let shown = ext.email.as_deref().and_then(|e| normalize_email(e).ok());
            external::insert_identity(&tx, id, ext, shown.as_deref(), verified, now)?;
        }
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
}
