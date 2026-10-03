//! Orgs: the trust boundary. An org is an incus project; its people and
//! agents fully administer what is in it, and nothing crosses orgs.
//!
//! The `default` org is the incus `default` project, so everything that
//! predates orgs keeps working where it is. Any other org `x` is the incus
//! project `isb-x`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// A validated org name: `[a-z][a-z0-9-]{0,30}`, not ending in `-`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct OrgId(String);

pub const DEFAULT_ORG: &str = "default";

impl OrgId {
    pub fn new(s: impl Into<String>) -> Result<OrgId> {
        let s = s.into();
        let ok = !s.is_empty()
            && s.len() <= 31
            && s.starts_with(|c: char| c.is_ascii_lowercase())
            && !s.ends_with('-')
            && s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if ok {
            Ok(OrgId(s))
        } else {
            Err(Error::invalid(format!(
                "org name {s:?}: up to 31 characters of [a-z0-9-], starting with a letter"
            )))
        }
    }

    pub fn default_org() -> OrgId {
        OrgId(DEFAULT_ORG.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_default(&self) -> bool {
        self.0 == DEFAULT_ORG
    }

    /// The incus project holding this org.
    pub fn incus_project(&self) -> String {
        if self.is_default() {
            "default".into()
        } else {
            format!("isb-{}", self.0)
        }
    }

    /// The org a project belongs to, if it is one of isb's.
    pub fn from_incus_project(project: &str) -> Option<OrgId> {
        if project == "default" {
            return Some(OrgId::default_org());
        }
        project.strip_prefix("isb-").and_then(|o| OrgId::new(o).ok())
    }

    /// This org's directory under a daemon state directory.
    pub fn dir(&self, state: &Path) -> PathBuf {
        state.join("orgs").join(&self.0)
    }
}

impl std::fmt::Display for OrgId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for OrgId {
    type Error = Error;
    fn try_from(s: String) -> Result<OrgId> {
        OrgId::new(s)
    }
}

impl From<OrgId> for String {
    fn from(o: OrgId) -> String {
        o.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_projects() {
        assert!(OrgId::new("ocai").is_ok());
        assert!(OrgId::new("Ocai").is_err());
        assert!(OrgId::new("a-").is_err());
        assert!(OrgId::new("x".repeat(32)).is_err());
        let o = OrgId::new("ocai").unwrap();
        assert_eq!(o.incus_project(), "isb-ocai");
        assert_eq!(OrgId::default_org().incus_project(), "default");
        assert_eq!(OrgId::from_incus_project("isb-ocai"), Some(o));
        assert_eq!(OrgId::from_incus_project("titan-ocai-ct"), None);
        let j: OrgId = serde_json::from_str("\"norm\"").unwrap();
        assert_eq!(j.as_str(), "norm");
        assert!(serde_json::from_str::<OrgId>("\"Bad Name\"").is_err());
    }
}
