//! Agent identities: tailnet and Cloudflare Access callers an org lets in as
//! its agents, each mapped to a role in that one org.
//!
//! - A **tailnet** mapping names a login (`someone@example.com`) or a node
//!   tag (`tag:agents`). A tagged node matches only by its tags, never by
//!   its owner's login; any other node matches by its user's login.
//! - An **Access** mapping names the email of someone who is not an isb user
//!   (an isb user's email acts as that user, with their real memberships), or
//!   a service token's client id.
//! - The role is `viewer`, `member` or `admin`: never `owner`.
//!
//! A matched caller is a synthetic principal ([`Principal::agent`]) with no
//! account, confined to the orgs that mapped it. Who resolves a request to
//! an identity (tailscaled's whois, a verified Access assertion) is the
//! daemon's gate; this module is the table and the matching.

use rusqlite::{OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use super::{AuthError, AuthResult, AuthStore, Principal, Role, clean_name, org_col, role_col};
use crate::org::OrgId;

/// How many mappings one org may hold.
pub const MAX_PER_ORG: i64 = 100;

/// Which front door a mapping is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Tailnet,
    Access,
}

impl AgentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentKind::Tailnet => "tailnet",
            AgentKind::Access => "access",
        }
    }

    pub fn parse(s: &str) -> AuthResult<AgentKind> {
        match s {
            "tailnet" => Ok(AgentKind::Tailnet),
            "access" => Ok(AgentKind::Access),
            _ => Err(AuthError::Invalid(format!("kind {s:?}: tailnet or access"))),
        }
    }
}

/// Which front doors this server has, so an org can tell whether its
/// mappings could ever be used.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct AgentWays {
    /// The tailnet `--listen` addresses (empty: no tailnet peer can reach
    /// the server).
    pub tailnet_listen: Vec<String>,
    /// Cloudflare Access guards a listener.
    pub access: bool,
}

/// One mapping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentIdentity {
    pub id: i64,
    pub org: OrgId,
    pub kind: AgentKind,
    /// Tailnet: a login (`a@example.com`) or `tag:name`. Access: an email or
    /// a service token's client id.
    pub subject: String,
    pub role: Role,
    pub note: String,
    pub created_at: i64,
    /// Who made it (an audit-style name).
    pub created_by: String,
}

/// The mapping a subject string means, normalized: logins, tags and emails
/// lowercase; a client id as given. `Err` says what is wrong.
pub fn normalize_subject(kind: AgentKind, subject: &str) -> AuthResult<String> {
    let s = subject.trim();
    let bad = |m: &str| {
        Err(AuthError::Invalid(format!(
            "{} identity {s:?}: {m}",
            kind.as_str()
        )))
    };
    if s.is_empty() || s.len() > 200 {
        return bad("1 to 200 characters");
    }
    if s.contains(['*', '?', ',']) || s.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return bad("exact names only: no wildcards, spaces or commas");
    }
    match kind {
        AgentKind::Tailnet => {
            if let Some(t) = s
                .get(..4)
                .filter(|p| p.eq_ignore_ascii_case("tag:"))
                .map(|_| &s[4..])
            {
                if t.is_empty()
                    || !t
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
                {
                    return bad("a tag is tag: and letters, digits, - or _");
                }
                Ok(format!("tag:{}", t.to_ascii_lowercase()))
            } else if s.contains('@') && !s.starts_with('@') && !s.ends_with('@') {
                Ok(s.to_ascii_lowercase())
            } else {
                bad("a login name (someone@example.com) or a tag (tag:name)")
            }
        }
        AgentKind::Access => {
            if s.contains('@') {
                if s.starts_with('@') || s.ends_with('@') {
                    return bad("a whole email address");
                }
                Ok(s.to_ascii_lowercase())
            } else {
                Ok(s.to_string())
            }
        }
    }
}

fn row(r: &Row) -> rusqlite::Result<AgentIdentity> {
    let kind: String = r.get(2)?;
    Ok(AgentIdentity {
        id: r.get(0)?,
        org: org_col(r, 1)?,
        kind: AgentKind::parse(&kind).unwrap_or(AgentKind::Access),
        subject: r.get(3)?,
        role: role_col(r, 4)?,
        note: r.get(5)?,
        created_at: r.get(6)?,
        created_by: r.get(7)?,
    })
}

const COLS: &str = "id, org, kind, subject, role, note, created_at, created_by";

impl AuthStore {
    /// An org's mappings, oldest first.
    pub fn list_agent_identities(&self, org: &OrgId) -> AuthResult<Vec<AgentIdentity>> {
        let db = self.db();
        let mut st = db.prepare(&format!(
            "SELECT {COLS} FROM org_agent_identities WHERE org = ?1 ORDER BY id"
        ))?;
        let rows = st.query_map([org.as_str()], row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Map `subject` to `role` in `org`, or change the role of an existing
    /// mapping. Never `owner`; an Access email that is an isb user's is
    /// refused (it acts as that user: make them a member).
    pub fn set_agent_identity(
        &self,
        org: &OrgId,
        kind: AgentKind,
        subject: &str,
        role: Role,
        note: &str,
        created_by: &str,
    ) -> AuthResult<AgentIdentity> {
        if role == Role::Owner {
            return Err(AuthError::Invalid(
                "an agent identity is a viewer, member or admin: never an owner".into(),
            ));
        }
        let subject = normalize_subject(kind, subject)?;
        let note = if note.trim().is_empty() {
            String::new()
        } else {
            clean_name(note)?
        };
        if kind == AgentKind::Access
            && subject.contains('@')
            && self.user_by_email(&subject)?.is_some()
        {
            return Err(AuthError::Conflict(format!(
                "{subject} is an isb user, and a Cloudflare Access caller with that email already acts as that user; add them as a member of the org instead"
            )));
        }
        let db = self.db();
        let n: i64 = db.query_row(
            "SELECT COUNT(*) FROM org_agent_identities WHERE org = ?1",
            [org.as_str()],
            |r| r.get(0),
        )?;
        let exists: Option<i64> = db
            .query_row(
                "SELECT id FROM org_agent_identities WHERE org = ?1 AND kind = ?2 AND subject = ?3",
                params![org.as_str(), kind.as_str(), subject],
                |r| r.get(0),
            )
            .optional()?;
        let id = match exists {
            Some(id) => {
                db.execute(
                    "UPDATE org_agent_identities SET role = ?2, note = ?3 WHERE id = ?1",
                    params![id, role.as_str(), note],
                )?;
                id
            }
            None => {
                if n >= MAX_PER_ORG {
                    return Err(AuthError::Invalid(format!(
                        "an org holds at most {MAX_PER_ORG} agent identities; remove one first"
                    )));
                }
                let r = db.execute(
                    "INSERT INTO org_agent_identities (org, kind, subject, role, note, created_at, created_by)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        org.as_str(),
                        kind.as_str(),
                        subject,
                        role.as_str(),
                        note,
                        self.now(),
                        created_by
                    ],
                );
                match r {
                    Ok(_) => db.last_insert_rowid(),
                    Err(rusqlite::Error::SqliteFailure(e, _))
                        if e.code == rusqlite::ErrorCode::ConstraintViolation =>
                    {
                        return Err(AuthError::NotFound(format!("org {org}")));
                    }
                    Err(e) => return Err(e.into()),
                }
            }
        };
        Ok(db.query_row(
            &format!("SELECT {COLS} FROM org_agent_identities WHERE id = ?1"),
            [id],
            row,
        )?)
    }

    /// Remove one of `org`'s mappings. True if it existed.
    pub fn remove_agent_identity(&self, org: &OrgId, id: i64) -> AuthResult<bool> {
        let n = self.db().execute(
            "DELETE FROM org_agent_identities WHERE id = ?1 AND org = ?2",
            params![id, org.as_str()],
        )?;
        Ok(n > 0)
    }

    /// The orgs that map any of `subjects` (already normalized) for `kind`,
    /// with the highest role each gives.
    pub fn agent_orgs(
        &self,
        kind: AgentKind,
        subjects: &[String],
    ) -> AuthResult<Vec<(OrgId, Role)>> {
        let mut out: Vec<(OrgId, Role)> = Vec::new();
        let db = self.db();
        let mut st = db.prepare(
            "SELECT org, role FROM org_agent_identities WHERE kind = ?1 AND subject = ?2",
        )?;
        for s in subjects {
            let rows = st.query_map(params![kind.as_str(), s], |r| {
                Ok((org_col(r, 0)?, role_col(r, 1)?))
            })?;
            for r in rows {
                let (org, role) = r?;
                // The table never holds an owner; a hand-edited row is capped.
                let role = role.min(Role::Admin);
                match out.iter_mut().find(|(o, _)| *o == org) {
                    Some((_, r0)) => *r0 = (*r0).max(role),
                    None => out.push((org, role)),
                }
            }
        }
        out.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        Ok(out)
    }

    /// The principal of a tailnet node: a tagged node by its tags only, any
    /// other by its user's login. `None` when no org maps it.
    pub fn principal_for_tailnet(
        &self,
        login: &str,
        node: &str,
        tags: &[String],
    ) -> AuthResult<Option<Principal>> {
        let subjects: Vec<String> = if tags.is_empty() {
            vec![login.to_ascii_lowercase()]
        } else {
            tags.iter().map(|t| t.to_ascii_lowercase()).collect()
        };
        let orgs = self.agent_orgs(AgentKind::Tailnet, &subjects)?;
        if orgs.is_empty() {
            return Ok(None);
        }
        let name = if tags.is_empty() { login } else { node };
        Ok(Some(Principal::agent(&format!("tailnet:{name}"), orgs)))
    }

    /// The principal of a verified Access identity that is not an isb user:
    /// a person's email, else a service token's client id. `None` when no
    /// org maps it, and for an email that is an isb user's (such a caller
    /// acts as that user, [`AuthStore::principal_for_email`]).
    pub fn principal_for_access_agent(
        &self,
        email: Option<&str>,
        client_id: Option<&str>,
    ) -> AuthResult<Option<Principal>> {
        let name = match (email, client_id) {
            (Some(e), _) => {
                if self.user_by_email(e)?.is_some() {
                    return Ok(None);
                }
                e.to_ascii_lowercase()
            }
            (None, Some(c)) => c.to_string(),
            (None, None) => return Ok(None),
        };
        let orgs = self.agent_orgs(AgentKind::Access, std::slice::from_ref(&name))?;
        if orgs.is_empty() {
            return Ok(None);
        }
        Ok(Some(Principal::agent(&format!("access:{name}"), orgs)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthConfig;

    fn store() -> AuthStore {
        AuthStore::in_memory(AuthConfig {
            password_cost: crate::auth::secret::PasswordCost::insecure_fast(),
            ..Default::default()
        })
        .unwrap()
    }

    fn org(s: &AuthStore, name: &str) -> OrgId {
        let o = OrgId::new(name).unwrap();
        s.ensure_org(&o).unwrap();
        o
    }

    #[test]
    fn subjects_are_exact_and_normalized() {
        use AgentKind::*;
        assert_eq!(
            normalize_subject(Tailnet, " Me@Example.com ").unwrap(),
            "me@example.com"
        );
        assert_eq!(
            normalize_subject(Tailnet, "tag:Agents").unwrap(),
            "tag:agents"
        );
        for bad in [
            "",
            "tag:",
            "tag:a b",
            "*@example.com",
            "@example.com",
            "nobody",
            "tag:a*",
        ] {
            assert!(normalize_subject(Tailnet, bad).is_err(), "{bad}");
        }
        assert_eq!(
            normalize_subject(Access, "Bob@Example.com").unwrap(),
            "bob@example.com"
        );
        assert_eq!(
            normalize_subject(Access, "AbC123.access").unwrap(),
            "AbC123.access"
        );
        assert!(normalize_subject(Access, "a@").is_err());
        assert!(normalize_subject(Access, "a,b").is_err());
    }

    #[test]
    fn mappings_upsert_cap_and_never_own() {
        let s = store();
        let acme = org(&s, "acme");
        let set = |o: &OrgId, k, subj: &str, r| s.set_agent_identity(o, k, subj, r, "", "me");
        let a = set(&acme, AgentKind::Tailnet, "tag:agents", Role::Member).unwrap();
        assert_eq!(a.role, Role::Member);
        let again = set(&acme, AgentKind::Tailnet, "TAG:agents", Role::Viewer).unwrap();
        assert_eq!(again.id, a.id);
        assert_eq!(again.role, Role::Viewer);
        assert_eq!(s.list_agent_identities(&acme).unwrap().len(), 1);
        assert!(set(&acme, AgentKind::Tailnet, "tag:x", Role::Owner).is_err());
        assert!(s.remove_agent_identity(&acme, a.id).unwrap());
        assert!(!s.remove_agent_identity(&acme, a.id).unwrap());
        // An org's mapping cannot be removed through another org.
        let b = set(&acme, AgentKind::Access, "svc.access", Role::Admin).unwrap();
        let other = org(&s, "other");
        assert!(!s.remove_agent_identity(&other, b.id).unwrap());
        // An isb user's email is not an Access mapping.
        s.create_user("alice@example.com", "Alice", None, false)
            .unwrap();
        assert!(matches!(
            set(&acme, AgentKind::Access, "Alice@example.com", Role::Member),
            Err(AuthError::Conflict(_))
        ));
        // Unknown org.
        let ghost = OrgId::new("ghost").unwrap();
        assert!(set(&ghost, AgentKind::Access, "x.access", Role::Member).is_err());
    }

    #[test]
    fn tailnet_tags_and_logins_never_mix() {
        let s = store();
        let acme = org(&s, "acme");
        let set = |subj: &str, r| s.set_agent_identity(&acme, AgentKind::Tailnet, subj, r, "", "x");
        set("me@example.com", Role::Admin).unwrap();
        set("tag:agents", Role::Viewer).unwrap();
        // An untagged node: by login.
        let p = s
            .principal_for_tailnet("me@example.com", "laptop.t.ts.net", &[])
            .unwrap()
            .unwrap();
        assert_eq!(p.user.email, "tailnet:me@example.com");
        assert_eq!(p.orgs, vec![(acme.clone(), Role::Admin)]);
        assert!(!p.platform_admin && p.is_agent() && p.user.id == 0);
        // A tagged node whose owner's login is mapped: only its tags count.
        let p = s
            .principal_for_tailnet("me@example.com", "bot.t.ts.net", &["tag:agents".into()])
            .unwrap()
            .unwrap();
        assert_eq!(p.user.email, "tailnet:bot.t.ts.net");
        assert_eq!(p.orgs, vec![(acme.clone(), Role::Viewer)]);
        // A tagged node with an unmapped tag is nobody, whatever its owner.
        assert!(
            s.principal_for_tailnet("me@example.com", "bot.t.ts.net", &["tag:other".into()])
                .unwrap()
                .is_none()
        );
        // An untagged node cannot use a tag mapping.
        assert!(
            s.principal_for_tailnet("agents@example.com", "x.t.ts.net", &[])
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn identities_pin_to_the_orgs_that_map_them() {
        let s = store();
        let acme = org(&s, "acme");
        let beta = org(&s, "beta");
        let set = |o: &OrgId, k, subj: &str, r| s.set_agent_identity(o, k, subj, r, "", "x");
        set(&acme, AgentKind::Access, "svc.access", Role::Admin).unwrap();
        set(&beta, AgentKind::Access, "svc.access", Role::Viewer).unwrap();
        set(
            &beta,
            AgentKind::Tailnet,
            "svc.access@example.com",
            Role::Admin,
        )
        .unwrap();
        let p = s
            .principal_for_access_agent(None, Some("svc.access"))
            .unwrap()
            .unwrap();
        assert_eq!(p.role_in(&acme), Some(Role::Admin));
        assert_eq!(p.role_in(&beta), Some(Role::Viewer));
        assert_eq!(p.role_in(&OrgId::new("gamma").unwrap()), None);
        assert!(!p.can_admin_org(&beta) && p.can_read_org(&beta));
        // A tailnet mapping is not an Access mapping.
        assert!(
            s.principal_for_access_agent(Some("svc.access@example.com"), None)
                .unwrap()
                .is_none()
        );
        // A person who is an isb user is not an agent identity, even if a
        // row was made before the account existed.
        set(&acme, AgentKind::Access, "eve@example.com", Role::Member).unwrap();
        assert!(
            s.principal_for_access_agent(Some("EVE@example.com"), None)
                .unwrap()
                .is_some()
        );
        s.create_user("eve@example.com", "Eve", None, false)
            .unwrap();
        assert!(
            s.principal_for_access_agent(Some("eve@example.com"), None)
                .unwrap()
                .is_none()
        );
    }
}
