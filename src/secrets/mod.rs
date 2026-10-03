//! Secrets: named values per org, behind pluggable drivers.
//!
//! A secret is `(org, name)` with a value and a [`SecretMeta`] (driver,
//! version, timestamps, labels). A [`Driver`] stores values; [`Secrets`] is
//! the facade the daemon uses, and finds the driver holding each secret. The
//! default driver is [`local`]: age ciphertext under the daemon's state
//! directory, encrypted to the daemon's own key and any break-glass
//! recipients ([`keys`]). Values are never listed: `list` and `inspect`
//! return metadata only.
//!
//! Compose files can also carry a value inline, age-encrypted ([`inline`]).

pub mod inline;
pub mod keys;
pub mod local;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::org::OrgId;

pub use inline::{decrypt_inline, encrypt_inline};
pub use keys::{KeyOrigin, KeySources, Keyring, Recipient, SecretsConfig};
pub use local::LocalDriver;

/// The largest value a secret may hold.
pub const MAX_VALUE_BYTES: usize = 1024 * 1024;

/// What is known about a secret besides its value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretMeta {
    pub org: OrgId,
    pub name: String,
    /// The driver holding the value (`local`, ...).
    pub driver: String,
    /// Starts at 1; every new value bumps it. Stacks roll on a change.
    pub version: u64,
    /// Unix seconds.
    pub created_at: u64,
    pub updated_at: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
}

/// A secret store. Drivers that cannot write (an external vault read through
/// a read-only token) keep the default `create`/`set`/`delete`, which refuse.
pub trait Driver: Send + Sync {
    /// The name compose files and `--driver` use.
    fn name(&self) -> &str;

    /// The value and its version.
    fn get(&self, org: &OrgId, name: &str) -> Result<(Vec<u8>, u64)>;

    /// The current version only: cheap, for polling.
    fn version(&self, org: &OrgId, name: &str) -> Result<u64>;

    fn inspect(&self, org: &OrgId, name: &str) -> Result<SecretMeta>;

    /// Every secret in the org this driver holds: metadata, never values.
    fn list(&self, org: &OrgId) -> Result<Vec<SecretMeta>>;

    /// A new secret at version 1; fails if it exists.
    fn create(
        &self,
        org: &OrgId,
        name: &str,
        _value: &[u8],
        _labels: &BTreeMap<String, String>,
    ) -> Result<SecretMeta> {
        Err(read_only(self.name(), org, name))
    }

    /// A new value; returns the new version. Creates the secret if missing.
    fn set(&self, org: &OrgId, name: &str, _value: &[u8]) -> Result<u64> {
        Err(read_only(self.name(), org, name))
    }

    fn delete(&self, org: &OrgId, name: &str) -> Result<()> {
        Err(read_only(self.name(), org, name))
    }

    /// Re-read the value from its source now (external drivers cache and
    /// poll). A no-op for drivers that are their own source.
    fn refresh(&self, org: &OrgId, name: &str) -> Result<SecretMeta> {
        self.inspect(org, name)
    }

    /// Re-encrypt every value (in one org, or all) to the current
    /// recipients. Returns how many. A no-op for drivers that do not
    /// encrypt.
    fn reencrypt(&self, _org: Option<&OrgId>) -> Result<usize> {
        Ok(0)
    }
}

/// A valid secret name: 1-128 of `[A-Za-z0-9_.-]`, not starting with `.`
/// (so it can never be `.`, `..` or a hidden file).
pub fn validate_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "secret name {name:?}: 1-128 characters of [A-Za-z0-9_.-], not starting with '.'"
        )))
    }
}

pub(crate) fn not_found(org: &OrgId, name: &str) -> Error {
    Error::NotFound(format!("secret {name} in org {org}"))
}

pub(crate) fn exists(org: &OrgId, name: &str) -> Error {
    Error::invalid(format!(
        "secret {name} already exists in org {org}; use `isb secret set` for a new value"
    ))
}

fn read_only(driver: &str, org: &OrgId, name: &str) -> Error {
    Error::invalid(format!(
        "secret {name} in org {org}: driver {driver} is read-only"
    ))
}

fn check_value(value: &[u8]) -> Result<()> {
    if value.len() > MAX_VALUE_BYTES {
        return Err(Error::invalid(format!(
            "secret value is {} bytes; the limit is {MAX_VALUE_BYTES}",
            value.len()
        )));
    }
    Ok(())
}

/// The daemon's secrets: every driver, `local` first and the default.
pub struct Secrets {
    drivers: Vec<Arc<dyn Driver>>,
    keyring: Arc<Keyring>,
}

/// What [`Secrets::open`] found, for the daemon to log.
pub struct Opened {
    pub secrets: Secrets,
    pub origin: KeyOrigin,
    /// One line each: where the key came from, a generated key, warnings.
    pub notes: Vec<String>,
}

impl Secrets {
    /// Just the `local` driver.
    pub fn new(local: LocalDriver) -> Secrets {
        let keyring = local.keyring().clone();
        Secrets {
            drivers: vec![Arc::new(local)],
            keyring,
        }
    }

    /// Find (or generate) the daemon's key, read the break-glass recipients,
    /// and open the local store under `state_dir`.
    pub fn open(state_dir: &Path, keys: &KeySources, config: &SecretsConfig) -> Result<Opened> {
        let break_glass = config.parsed_recipients()?;
        let k = keys::load_identity(keys)?;
        let keyring = Arc::new(Keyring::new(k.identity, break_glass));
        let mut notes = vec![format!(
            "secrets key from {} (public key {})",
            k.origin,
            keyring.public_key()
        )];
        notes.extend(k.notes);
        if keyring.break_glass().is_empty() {
            notes.push(format!(
                "WARNING: no break-glass recipients: losing the secrets key loses every secret. Add recipients = [\"age1...\" or \"ssh-ed25519 ...\"] to {}, then run `isb secret reencrypt --all`",
                SecretsConfig::default_path().display()
            ));
        }
        Ok(Opened {
            secrets: Secrets::new(LocalDriver::new(state_dir, keyring)),
            origin: k.origin,
            notes,
        })
    }

    /// Add a driver. Names are unique.
    pub fn with_driver(mut self, d: Arc<dyn Driver>) -> Result<Secrets> {
        if self.drivers.iter().any(|x| x.name() == d.name()) {
            return Err(Error::invalid(format!(
                "secrets driver {} registered twice",
                d.name()
            )));
        }
        self.drivers.push(d);
        Ok(self)
    }

    pub fn driver_names(&self) -> Vec<String> {
        self.drivers.iter().map(|d| d.name().to_string()).collect()
    }

    pub fn keyring(&self) -> &Arc<Keyring> {
        &self.keyring
    }

    /// The recipient set, as strings (`age1…`, `ssh-…`).
    pub fn recipients(&self) -> Vec<String> {
        self.keyring
            .recipients()
            .iter()
            .map(|r| r.to_string())
            .collect()
    }

    fn driver(&self, name: &str) -> Result<&Arc<dyn Driver>> {
        self.drivers
            .iter()
            .find(|d| d.name() == name)
            .ok_or_else(|| {
                Error::invalid(format!(
                    "unknown secrets driver {name:?} (have: {})",
                    self.driver_names().join(", ")
                ))
            })
    }

    /// The driver holding a secret, and its metadata.
    fn holder(&self, org: &OrgId, name: &str) -> Result<(&Arc<dyn Driver>, SecretMeta)> {
        validate_name(name)?;
        for d in &self.drivers {
            match d.inspect(org, name) {
                Ok(m) => return Ok((d, m)),
                Err(Error::NotFound(_)) => {}
                Err(e) => return Err(e),
            }
        }
        Err(not_found(org, name))
    }

    /// A new secret in `driver` (default `local`); fails if any driver has
    /// one by that name.
    pub fn create(
        &self,
        org: &OrgId,
        name: &str,
        driver: Option<&str>,
        value: &[u8],
        labels: &BTreeMap<String, String>,
    ) -> Result<SecretMeta> {
        validate_name(name)?;
        check_value(value)?;
        for k in labels.keys() {
            if k.is_empty() || k.len() > 128 {
                return Err(Error::invalid(format!("label key {k:?}: 1-128 characters")));
            }
        }
        let d = self.driver(driver.unwrap_or(local::DRIVER))?;
        match self.holder(org, name) {
            Ok(_) => return Err(exists(org, name)),
            Err(Error::NotFound(_)) => {}
            Err(e) => return Err(e),
        }
        d.create(org, name, value, labels)
    }

    /// A new value for a secret, in the driver holding it; a missing one is
    /// created in `local`.
    pub fn set(&self, org: &OrgId, name: &str, value: &[u8]) -> Result<SecretMeta> {
        check_value(value)?;
        let d = match self.holder(org, name) {
            Ok((d, _)) => d,
            Err(Error::NotFound(_)) => self.driver(local::DRIVER)?,
            Err(e) => return Err(e),
        };
        d.set(org, name, value)?;
        d.inspect(org, name)
    }

    pub fn get(&self, org: &OrgId, name: &str) -> Result<(Vec<u8>, SecretMeta)> {
        let (d, _) = self.holder(org, name)?;
        let (v, version) = d.get(org, name)?;
        let mut m = d.inspect(org, name)?;
        // The value read is the one to describe, even if a write landed
        // between the two calls.
        m.version = version;
        Ok((v, m))
    }

    pub fn version(&self, org: &OrgId, name: &str) -> Result<u64> {
        let (d, _) = self.holder(org, name)?;
        d.version(org, name)
    }

    pub fn inspect(&self, org: &OrgId, name: &str) -> Result<SecretMeta> {
        self.holder(org, name).map(|(_, m)| m)
    }

    /// Every driver's secrets in the org, by name.
    pub fn list(&self, org: &OrgId) -> Result<Vec<SecretMeta>> {
        let mut out = Vec::new();
        for d in &self.drivers {
            out.extend(d.list(org)?);
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    pub fn delete(&self, org: &OrgId, name: &str) -> Result<()> {
        let (d, _) = self.holder(org, name)?;
        d.delete(org, name)
    }

    pub fn refresh(&self, org: &OrgId, name: &str) -> Result<SecretMeta> {
        let (d, _) = self.holder(org, name)?;
        d.refresh(org, name)
    }

    /// Re-encrypt every value to the current recipients, in one org or all.
    pub fn reencrypt(&self, org: Option<&OrgId>) -> Result<usize> {
        let mut n = 0;
        for d in &self.drivers {
            n += d.reencrypt(org)?;
        }
        Ok(n)
    }

    /// Decrypt a compose file's inline `age:` value with the daemon's key.
    pub fn decrypt_inline(&self, text: &str) -> Result<Vec<u8>> {
        inline::decrypt_inline(text, &[self.keyring.identity()])
    }

    /// Inline ciphertext to the daemon's recipients.
    pub fn encrypt_inline(&self, value: &[u8]) -> Result<String> {
        inline::encrypt_inline(value, self.keyring.recipients())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A read-only driver holding one fixed secret, as an external vault
    /// would.
    struct Fixed;
    impl Driver for Fixed {
        fn name(&self) -> &str {
            "fixed"
        }
        fn get(&self, org: &OrgId, name: &str) -> Result<(Vec<u8>, u64)> {
            self.inspect(org, name)
                .map(|m| (b"external".to_vec(), m.version))
        }
        fn version(&self, org: &OrgId, name: &str) -> Result<u64> {
            self.inspect(org, name).map(|m| m.version)
        }
        fn inspect(&self, org: &OrgId, name: &str) -> Result<SecretMeta> {
            if name != "vault_token" {
                return Err(not_found(org, name));
            }
            Ok(SecretMeta {
                org: org.clone(),
                name: name.into(),
                driver: "fixed".into(),
                version: 7,
                created_at: 0,
                updated_at: 0,
                labels: BTreeMap::new(),
            })
        }
        fn list(&self, org: &OrgId) -> Result<Vec<SecretMeta>> {
            Ok(vec![self.inspect(org, "vault_token")?])
        }
    }

    fn secrets(dir: &Path) -> Secrets {
        let k = Keyring::new(age::x25519::Identity::generate(), vec![]);
        Secrets::new(LocalDriver::new(dir, Arc::new(k)))
            .with_driver(Arc::new(Fixed))
            .unwrap()
    }

    #[test]
    fn names() {
        for ok in ["a", "DB_PASSWORD", "tls.key", "x-1", &"n".repeat(128)] {
            assert!(validate_name(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            ".",
            "..",
            ".hidden",
            "a/b",
            "../x",
            "a b",
            "ü",
            &"n".repeat(129),
        ] {
            assert!(validate_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn facade_dispatches_per_secret() {
        let dir = tempfile::tempdir().unwrap();
        let s = secrets(dir.path());
        let org = OrgId::default_org();
        assert_eq!(s.driver_names(), ["local", "fixed"]);
        s.create(&org, "db", None, b"pw", &BTreeMap::new()).unwrap();
        // Names are unique across drivers.
        assert!(
            s.create(&org, "vault_token", None, b"x", &BTreeMap::new())
                .is_err()
        );
        assert!(
            s.create(&org, "new", Some("nope"), b"x", &BTreeMap::new())
                .is_err()
        );
        // A read-only driver refuses writes.
        let e = s
            .create(&org, "other", Some("fixed"), b"x", &BTreeMap::new())
            .unwrap_err();
        assert!(e.to_string().contains("read-only"), "{e}");
        assert!(s.set(&org, "vault_token", b"x").is_err());
        assert!(s.delete(&org, "vault_token").is_err());
        let (v, m) = s.get(&org, "vault_token").unwrap();
        assert_eq!((v.as_slice(), m.version), (&b"external"[..], 7));
        let (v, m) = s.get(&org, "db").unwrap();
        assert_eq!(
            (v.as_slice(), m.version, m.driver.as_str()),
            (&b"pw"[..], 1, "local")
        );
        assert_eq!(s.set(&org, "db", b"pw2").unwrap().version, 2);
        assert_eq!(s.version(&org, "db").unwrap(), 2);
        assert_eq!(s.refresh(&org, "db").unwrap().version, 2);
        let names: Vec<_> = s.list(&org).unwrap().into_iter().map(|m| m.name).collect();
        assert_eq!(names, ["db", "vault_token"]);
        assert!(s.set(&org, "big", &vec![0; MAX_VALUE_BYTES + 1]).is_err());
        s.delete(&org, "db").unwrap();
        assert!(matches!(s.inspect(&org, "db"), Err(Error::NotFound(_))));
        assert!(s.with_driver(Arc::new(Fixed)).is_err());
    }

    #[test]
    fn open_generates_and_warns() {
        let dir = tempfile::tempdir().unwrap();
        let keys = KeySources {
            default_file: dir.path().join("cfg/age.txt"),
            ..Default::default()
        };
        let o = Secrets::open(&dir.path().join("state"), &keys, &SecretsConfig::default()).unwrap();
        assert!(matches!(o.origin, KeyOrigin::Generated(_)));
        assert!(o.notes.iter().any(|n| n.contains("no break-glass")));
        let org = OrgId::default_org();
        o.secrets
            .create(&org, "k", None, b"v", &BTreeMap::new())
            .unwrap();
        let ssh = include_str!("testdata/break_glass_ed25519.pub")
            .trim()
            .to_string();
        let cfg = SecretsConfig {
            recipients: vec![ssh],
        };
        let o2 = Secrets::open(&dir.path().join("state"), &keys, &cfg).unwrap();
        assert!(matches!(o2.origin, KeyOrigin::DefaultFile(_)));
        assert!(!o2.notes.iter().any(|n| n.contains("no break-glass")));
        assert_eq!(o2.secrets.recipients().len(), 2);
        assert_eq!(o2.secrets.get(&org, "k").unwrap().0, b"v");
        let a = o2.secrets.encrypt_inline(b"inline").unwrap();
        assert_eq!(o2.secrets.decrypt_inline(&a).unwrap(), b"inline");
        let bad = SecretsConfig {
            recipients: vec!["nope".into()],
        };
        assert!(Secrets::open(&dir.path().join("state"), &keys, &bad).is_err());
    }
}
