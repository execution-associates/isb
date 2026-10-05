//! Superadmin identities kept in `isb.db`: tailnet logins and tags, Access
//! emails and service token client ids, alongside the `--superadmin-tailnet`
//! and `--superadmin-access` flags (the bootstrap). The daemon reads the
//! table on each request that could match, so `isb superadmin add` and `rm`
//! take effect without a restart.
//!
//! Only the host CLI writes it (it opens `isb.db` as the daemon's own user,
//! as `isb token create --superadmin` does); no HTTP endpoint or tool does,
//! so an HTTP credential cannot make itself, or anyone, a durable superadmin.
//! Values are normalized as the flags parse them
//! ([`super::super::agent_identities::normalize_subject`]): logins, tags and
//! emails lowercase, a service token's client id exact.

use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use super::super::agent_identities::{AgentKind, normalize_subject};
use super::super::{AuthError, AuthResult, AuthStore};

/// How many the table may hold: it is read on every request that could
/// match one.
pub const MAX_SUPERADMIN_IDENTITIES: i64 = 200;

/// One identity in the table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SuperadminIdentity {
    pub id: i64,
    /// `tailnet` (a login or `tag:name`) or `access` (an email, or a
    /// service token's client id).
    pub kind: AgentKind,
    pub value: String,
    pub added_at: i64,
    /// Who added it (the CLI's audit name, `local(uid N)`).
    pub added_by: String,
}

impl SuperadminIdentity {
    /// A tailnet identity: whether it admits this login (an untagged node)
    /// or one of these tags (a tagged node, which never matches by login).
    pub fn admits_tailnet(&self, login: &str, tags: &[String]) -> bool {
        if self.kind != AgentKind::Tailnet {
            return false;
        }
        if tags.is_empty() {
            !self.value.starts_with("tag:") && self.value.eq_ignore_ascii_case(login)
        } else {
            tags.iter().any(|t| t.eq_ignore_ascii_case(&self.value))
        }
    }

    /// An Access identity: a user by email (case-insensitively), a service
    /// token by client id (exactly). An id never matches as an email.
    pub fn admits_access(&self, email: Option<&str>, client_id: Option<&str>) -> bool {
        if self.kind != AgentKind::Access {
            return false;
        }
        match (email, client_id) {
            (Some(e), _) => self.value.contains('@') && self.value.eq_ignore_ascii_case(e),
            (None, Some(cn)) => !self.value.contains('@') && self.value == cn,
            (None, None) => false,
        }
    }
}

fn row(r: &rusqlite::Row) -> rusqlite::Result<SuperadminIdentity> {
    let kind: String = r.get(1)?;
    Ok(SuperadminIdentity {
        id: r.get(0)?,
        kind: AgentKind::parse(&kind).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
        })?,
        value: r.get(2)?,
        added_at: r.get(3)?,
        added_by: r.get(4)?,
    })
}

const COLS: &str = "id, kind, value, added_at, added_by";

impl AuthStore {
    /// Add one. Only the host CLI calls this. A duplicate is refused.
    pub fn add_superadmin_identity(
        &self,
        kind: AgentKind,
        value: &str,
        added_by: &str,
    ) -> AuthResult<SuperadminIdentity> {
        let value = normalize_subject(kind, value)?;
        let db = self.db();
        let n: i64 = db.query_row("SELECT COUNT(*) FROM superadmin_identities", [], |r| {
            r.get(0)
        })?;
        if n >= MAX_SUPERADMIN_IDENTITIES {
            return Err(AuthError::Invalid(format!(
                "{n} superadmin identities, the most isb keeps: remove one first"
            )));
        }
        let now = self.now();
        let r = db.execute(
            "INSERT INTO superadmin_identities (kind, value, added_at, added_by)
             VALUES (?1, ?2, ?3, ?4)",
            params![kind.as_str(), value, now, added_by],
        );
        match r {
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                return Err(AuthError::Conflict(format!(
                    "{} {value} is a superadmin already",
                    kind.as_str()
                )));
            }
            Err(e) => return Err(e.into()),
        }
        Ok(SuperadminIdentity {
            id: db.last_insert_rowid(),
            kind,
            value,
            added_at: now,
            added_by: added_by.to_string(),
        })
    }

    /// Remove one; what was removed. A flag's entry is not in the table.
    pub fn remove_superadmin_identity(
        &self,
        kind: AgentKind,
        value: &str,
    ) -> AuthResult<SuperadminIdentity> {
        let value = normalize_subject(kind, value)?;
        let db = self.db();
        let found = db
            .query_row(
                &format!("SELECT {COLS} FROM superadmin_identities WHERE kind = ?1 AND value = ?2"),
                params![kind.as_str(), value],
                row,
            )
            .optional()?;
        let Some(found) = found else {
            return Err(AuthError::NotFound(format!(
                "{} {value} is not a superadmin in isb.db (one from --superadmin-{} is removed from that flag)",
                kind.as_str(),
                kind.as_str()
            )));
        };
        db.execute(
            "DELETE FROM superadmin_identities WHERE id = ?1",
            [found.id],
        )?;
        Ok(found)
    }

    pub fn list_superadmin_identities(&self) -> AuthResult<Vec<SuperadminIdentity>> {
        let db = self.db();
        let mut st = db.prepare(&format!(
            "SELECT {COLS} FROM superadmin_identities ORDER BY kind, value"
        ))?;
        let rows = st.query_map([], row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthConfig;

    #[test]
    fn add_list_remove_and_validate() {
        let s = AuthStore::in_memory(AuthConfig::default()).unwrap();
        let a = s
            .add_superadmin_identity(AgentKind::Access, " Alice@Example.com ", "local(uid 1)")
            .unwrap();
        assert_eq!(
            (a.value.as_str(), a.added_by.as_str()),
            ("alice@example.com", "local(uid 1)")
        );
        s.add_superadmin_identity(AgentKind::Access, "Svc.Access", "t")
            .unwrap();
        s.add_superadmin_identity(AgentKind::Tailnet, "TAG:Agents", "t")
            .unwrap();
        s.add_superadmin_identity(AgentKind::Tailnet, "me@example.com", "t")
            .unwrap();
        // Duplicates (after normalizing) are refused.
        assert!(matches!(
            s.add_superadmin_identity(AgentKind::Access, "alice@EXAMPLE.com", "t"),
            Err(AuthError::Conflict(_))
        ));
        // Exact names only: empty, wildcards, spaces, half an email, a bad tag.
        for (k, v) in [
            (AgentKind::Access, ""),
            (AgentKind::Access, "  "),
            (AgentKind::Access, "*@example.com"),
            (AgentKind::Access, "@example.com"),
            (AgentKind::Access, "a b@example.com"),
            (AgentKind::Tailnet, "nobody"),
            (AgentKind::Tailnet, "tag:"),
            (AgentKind::Tailnet, "tag:a.b"),
            (AgentKind::Tailnet, "a@x.io,b@x.io"),
        ] {
            assert!(
                matches!(
                    s.add_superadmin_identity(k, v, "t"),
                    Err(AuthError::Invalid(_))
                ),
                "{k:?} {v:?}"
            );
        }
        let l = s.list_superadmin_identities().unwrap();
        let got: Vec<_> = l.iter().map(|i| (i.kind, i.value.as_str())).collect();
        assert_eq!(
            got,
            vec![
                (AgentKind::Access, "Svc.Access"),
                (AgentKind::Access, "alice@example.com"),
                (AgentKind::Tailnet, "me@example.com"),
                (AgentKind::Tailnet, "tag:agents"),
            ]
        );
        // Matching: emails any case, client ids exactly, tags only for
        // tagged nodes, logins only for untagged ones.
        let find = |v: &str| l.iter().find(|i| i.value == v).unwrap();
        assert!(find("alice@example.com").admits_access(Some("ALICE@example.com"), None));
        assert!(!find("alice@example.com").admits_access(None, Some("alice@example.com")));
        assert!(find("Svc.Access").admits_access(None, Some("Svc.Access")));
        assert!(!find("Svc.Access").admits_access(None, Some("svc.access")));
        assert!(!find("Svc.Access").admits_tailnet("Svc.Access", &[]));
        assert!(find("tag:agents").admits_tailnet("tagged-devices", &["tag:agents".into()]));
        assert!(!find("tag:agents").admits_tailnet("tag:agents", &[]));
        assert!(find("me@example.com").admits_tailnet("Me@example.com", &[]));
        assert!(!find("me@example.com").admits_tailnet("me@example.com", &["tag:x".into()]));
        assert!(!find("me@example.com").admits_access(Some("me@example.com"), None));
        // Remove by the same spelling rules; twice is not found.
        let r = s
            .remove_superadmin_identity(AgentKind::Access, "ALICE@example.com")
            .unwrap();
        assert_eq!(r.id, a.id);
        assert!(matches!(
            s.remove_superadmin_identity(AgentKind::Access, "alice@example.com"),
            Err(AuthError::NotFound(_))
        ));
        assert_eq!(s.list_superadmin_identities().unwrap().len(), 3);
    }
}
