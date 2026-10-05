//! API token scopes: what a token may do on top of its role.

use super::{AuthError, AuthResult};

/// What an API token may do on top of its role. A token with no scopes has
/// the role's whole reach (agents administer their org by default); scopes
/// only ever narrow it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// Read-only tools, never secret values.
    Read,
    /// `read`, plus deploys, redeploys, rollbacks, scaling and builds.
    Deploy,
    /// Everything the role allows, including tokens and members.
    Admin,
    /// Tools whose name matches a glob: `tool:app_*`.
    Tools(String),
}

impl Scope {
    pub fn parse(s: &str) -> AuthResult<Scope> {
        let s = s.trim();
        match s {
            "read" => Ok(Scope::Read),
            "deploy" => Ok(Scope::Deploy),
            "admin" => Ok(Scope::Admin),
            _ => match s.strip_prefix("tool:") {
                Some(g)
                    if !g.is_empty()
                        && g.len() <= 128
                        && g.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"_.-*?[]!^".contains(&b)) =>
                {
                    Ok(Scope::Tools(g.to_string()))
                }
                _ => Err(AuthError::Invalid(format!(
                    "scope {s:?}: read, deploy, admin or tool:GLOB"
                ))),
            },
        }
    }

    pub fn as_string(&self) -> String {
        match self {
            Scope::Read => "read".into(),
            Scope::Deploy => "deploy".into(),
            Scope::Admin => "admin".into(),
            Scope::Tools(g) => format!("tool:{g}"),
        }
    }

    /// Check a list, normalized and without duplicates.
    pub fn normalize(v: &[String]) -> AuthResult<Vec<String>> {
        if v.len() > 32 {
            return Err(AuthError::Invalid("at most 32 scopes".into()));
        }
        let mut out: Vec<String> = Vec::new();
        for s in v {
            let s = Scope::parse(s)?.as_string();
            if !out.contains(&s) {
                out.push(s);
            }
        }
        Ok(out)
    }
}
