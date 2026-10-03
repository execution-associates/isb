//! What the control plane keeps about its servers and where orgs live:
//! `<state>/servers/servers.json` and `<state>/servers/placement.json`
//! (0600). Routing metadata only: no workload state, no secrets.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::org::OrgId;

/// One server the control plane places orgs on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerRecord {
    pub name: String,
    /// What the control plane dials (and the agent certificate's SAN).
    pub address: String,
    /// The agent's mTLS port.
    pub port: u16,
    /// `user@host` it was bootstrapped through (the key is never kept).
    pub ssh: String,
    pub ssh_port: u16,
    /// Unix seconds.
    pub added_at: u64,
    /// SHA-256 of the agent's current certificate.
    pub fingerprint: String,
    /// When that certificate expires (unix seconds), if known.
    #[serde(default)]
    pub cert_not_after: Option<u64>,
    /// The isb version installed at bootstrap.
    pub isb_version: String,
    /// Who the agent's firewall lets reach its port (empty: not managed).
    #[serde(default)]
    pub allow_from: Vec<String>,
}

fn read<T: DeserializeOwned + Default>(p: &Path) -> Result<T> {
    match std::fs::read(p) {
        Ok(b) => {
            serde_json::from_slice(&b).map_err(|e| Error::invalid(format!("{}: {e}", p.display())))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e.into()),
    }
}

fn write<T: Serialize>(p: &Path, v: &T) -> Result<()> {
    super::pki::write_private(p, &serde_json::to_string_pretty(v)?)
}

/// The servers and the placement, write-through.
#[derive(Debug)]
pub struct Store {
    dir: PathBuf,
    pub servers: BTreeMap<String, ServerRecord>,
    pub placement: BTreeMap<OrgId, String>,
}

impl Store {
    pub fn open(dir: &Path) -> Result<Store> {
        std::fs::create_dir_all(dir)?;
        Ok(Store {
            dir: dir.to_path_buf(),
            servers: read(&dir.join("servers.json"))?,
            placement: read(&dir.join("placement.json"))?,
        })
    }

    pub fn save(&self) -> Result<()> {
        write(&self.dir.join("servers.json"), &self.servers)?;
        write(&self.dir.join("placement.json"), &self.placement)
    }

    /// The orgs placed on `server`.
    pub fn orgs_on(&self, server: &str) -> Vec<OrgId> {
        self.placement
            .iter()
            .filter(|(_, s)| s.as_str() == server)
            .map(|(o, _)| o.clone())
            .collect()
    }
}

/// An agent's own list of the orgs its control plane placed on it:
/// `<state>/agent/orgs.json`. Calls for any other org are refused.
#[derive(Debug)]
pub struct AgentOrgs {
    path: PathBuf,
    orgs: std::sync::Mutex<Vec<OrgId>>,
}

impl AgentOrgs {
    pub fn open(state_dir: &Path) -> Result<AgentOrgs> {
        let dir = state_dir.join("agent");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("orgs.json");
        let orgs: Vec<OrgId> = read(&path)?;
        Ok(AgentOrgs {
            path,
            orgs: std::sync::Mutex::new(orgs),
        })
    }

    pub fn contains(&self, o: &OrgId) -> bool {
        self.orgs.lock().unwrap().contains(o)
    }

    pub fn list(&self) -> Vec<OrgId> {
        self.orgs.lock().unwrap().clone()
    }

    pub fn set(&self, o: &OrgId, placed: bool) -> Result<()> {
        let mut v = self.orgs.lock().unwrap();
        v.retain(|x| x != o);
        if placed {
            v.push(o.clone());
            v.sort();
        }
        write(&self.path, &*v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_and_agent_orgs_persist() {
        let d = tempfile::tempdir().unwrap();
        let mut s = Store::open(d.path()).unwrap();
        s.placement
            .insert(OrgId::new("acme").unwrap(), "box".into());
        s.save().unwrap();
        let s = Store::open(d.path()).unwrap();
        assert_eq!(s.orgs_on("box"), vec![OrgId::new("acme").unwrap()]);
        assert!(s.orgs_on("other").is_empty());

        let a = AgentOrgs::open(d.path()).unwrap();
        let acme = OrgId::new("acme").unwrap();
        a.set(&acme, true).unwrap();
        assert!(AgentOrgs::open(d.path()).unwrap().contains(&acme));
        a.set(&acme, false).unwrap();
        assert!(!AgentOrgs::open(d.path()).unwrap().contains(&acme));
    }
}
