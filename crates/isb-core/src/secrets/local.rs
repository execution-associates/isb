//! The `local` driver: age ciphertext on the daemon's disk.
//!
//! `<state>/orgs/<org>/secrets/<name>.age` holds the value encrypted to the
//! [`Keyring`]'s recipients; `<name>.json` beside it holds the
//! [`SecretMeta`]. Only the current value is kept. Files are 0600 in 0700
//! directories, and every write is a temp file, fsync, rename.

use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use super::{Driver, Keyring, SecretMeta, not_found, validate_name};
use crate::error::{Error, Result};
use crate::org::OrgId;
use crate::stack::now_secs;

pub const DRIVER: &str = "local";

pub struct LocalDriver {
    state: PathBuf,
    keyring: Arc<Keyring>,
    /// Writers exclusive, readers shared, so a reader never pairs one
    /// version's metadata with another's ciphertext.
    lock: RwLock<()>,
}

impl LocalDriver {
    pub fn new(state_dir: impl Into<PathBuf>, keyring: Arc<Keyring>) -> LocalDriver {
        LocalDriver {
            state: state_dir.into(),
            keyring,
            lock: RwLock::new(()),
        }
    }

    pub fn keyring(&self) -> &Arc<Keyring> {
        &self.keyring
    }

    /// `<state>/orgs/<org>/secrets`.
    pub fn dir(&self, org: &OrgId) -> PathBuf {
        org.dir(&self.state).join("secrets")
    }

    fn paths(&self, org: &OrgId, name: &str) -> Result<(PathBuf, PathBuf)> {
        validate_name(name)?;
        let d = self.dir(org);
        Ok((
            d.join(format!("{name}.age")),
            d.join(format!("{name}.json")),
        ))
    }

    fn ensure_dir(&self, org: &OrgId) -> Result<PathBuf> {
        let d = self.dir(org);
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&d)?;
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700))?;
        Ok(d)
    }

    fn read_meta(&self, org: &OrgId, name: &str) -> Result<SecretMeta> {
        let (_, meta) = self.paths(org, name)?;
        match std::fs::read(&meta) {
            Ok(b) => serde_json::from_slice(&b)
                .map_err(|e| Error::invalid(format!("{}: {e}", meta.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(not_found(org, name)),
            Err(e) => Err(e.into()),
        }
    }

    /// Ciphertext first, then metadata: a crash in between leaves the new
    /// value under the old version, which the next write corrects.
    fn write(&self, org: &OrgId, meta: &SecretMeta, value: &[u8]) -> Result<()> {
        let dir = self.ensure_dir(org)?;
        let (age, json) = self.paths(org, &meta.name)?;
        write_atomic(&age, &self.keyring.encrypt(value)?)?;
        write_atomic(&json, &serde_json::to_vec_pretty(meta)?)?;
        fsync_dir(&dir);
        Ok(())
    }

    fn decrypt(&self, org: &OrgId, name: &str) -> Result<Vec<u8>> {
        let (age, _) = self.paths(org, name)?;
        let ct = match std::fs::read(&age) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::invalid(format!(
                    "secret {name} in org {org}: metadata without a value ({} is missing); set it again",
                    age.display()
                )));
            }
            Err(e) => return Err(e.into()),
        };
        self.keyring.decrypt(&ct).map_err(|e| {
            Error::invalid(format!(
                "secret {name} in org {org}: cannot decrypt with this daemon's key ({e}); was it encrypted to another key?"
            ))
        })
    }

    fn orgs(&self) -> Result<Vec<OrgId>> {
        let dir = self.state.join("orgs");
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut out = Vec::new();
        for e in rd {
            let e = e?;
            if let Some(o) = e.file_name().to_str().and_then(|n| OrgId::new(n).ok()) {
                if e.path().join("secrets").is_dir() {
                    out.push(o);
                }
            }
        }
        out.sort();
        Ok(out)
    }

    fn read_guard(&self) -> std::sync::RwLockReadGuard<'_, ()> {
        self.lock.read().unwrap_or_else(|p| p.into_inner())
    }

    fn write_guard(&self) -> std::sync::RwLockWriteGuard<'_, ()> {
        self.lock.write().unwrap_or_else(|p| p.into_inner())
    }
}

impl Driver for LocalDriver {
    fn name(&self) -> &str {
        DRIVER
    }

    fn get(&self, org: &OrgId, name: &str) -> Result<(Vec<u8>, u64)> {
        let _g = self.read_guard();
        let meta = self.read_meta(org, name)?;
        Ok((self.decrypt(org, name)?, meta.version))
    }

    fn version(&self, org: &OrgId, name: &str) -> Result<u64> {
        let _g = self.read_guard();
        Ok(self.read_meta(org, name)?.version)
    }

    fn inspect(&self, org: &OrgId, name: &str) -> Result<SecretMeta> {
        let _g = self.read_guard();
        self.read_meta(org, name)
    }

    fn list(&self, org: &OrgId) -> Result<Vec<SecretMeta>> {
        let _g = self.read_guard();
        let rd = match std::fs::read_dir(self.dir(org)) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut out = Vec::new();
        for e in rd {
            let p = e?.path();
            if p.extension().is_none_or(|x| x != "json") {
                continue;
            }
            match std::fs::read(&p)
                .map_err(Error::from)
                .and_then(|b| serde_json::from_slice::<SecretMeta>(&b).map_err(Error::from))
            {
                Ok(m) => out.push(m),
                Err(e) => eprintln!("isb secrets: skipping {}: {e}", p.display()),
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    fn create(
        &self,
        org: &OrgId,
        name: &str,
        value: &[u8],
        labels: &BTreeMap<String, String>,
    ) -> Result<SecretMeta> {
        let _g = self.write_guard();
        match self.read_meta(org, name) {
            Ok(_) => return Err(super::exists(org, name)),
            Err(Error::NotFound(_)) => {}
            Err(e) => return Err(e),
        }
        let now = now_secs();
        let meta = SecretMeta {
            org: org.clone(),
            name: name.to_string(),
            driver: DRIVER.into(),
            version: 1,
            created_at: now,
            updated_at: now,
            labels: labels.clone(),
        };
        self.write(org, &meta, value)?;
        Ok(meta)
    }

    fn set(&self, org: &OrgId, name: &str, value: &[u8]) -> Result<u64> {
        let _g = self.write_guard();
        let now = now_secs();
        let meta = match self.read_meta(org, name) {
            Ok(mut m) => {
                m.version += 1;
                m.updated_at = now;
                m
            }
            Err(Error::NotFound(_)) => SecretMeta {
                org: org.clone(),
                name: name.to_string(),
                driver: DRIVER.into(),
                version: 1,
                created_at: now,
                updated_at: now,
                labels: BTreeMap::new(),
            },
            Err(e) => return Err(e),
        };
        self.write(org, &meta, value)?;
        Ok(meta.version)
    }

    fn delete(&self, org: &OrgId, name: &str) -> Result<()> {
        let _g = self.write_guard();
        self.read_meta(org, name)?;
        let (age, json) = self.paths(org, name)?;
        // Metadata first: a crash in between leaves an orphan .age that
        // nothing lists, not a listed secret without a value.
        std::fs::remove_file(&json)?;
        match std::fs::remove_file(&age) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
        fsync_dir(&self.dir(org));
        Ok(())
    }

    fn reencrypt(&self, org: Option<&OrgId>) -> Result<usize> {
        let _g = self.write_guard();
        let orgs = match org {
            Some(o) => vec![o.clone()],
            None => self.orgs()?,
        };
        let mut n = 0;
        for o in &orgs {
            let rd = match std::fs::read_dir(self.dir(o)) {
                Ok(rd) => rd,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            let mut names = Vec::new();
            for e in rd {
                let p = e?.path();
                if p.extension().is_some_and(|x| x == "json") {
                    let b = std::fs::read(&p)?;
                    if let Ok(m) = serde_json::from_slice::<SecretMeta>(&b) {
                        names.push(m.name);
                    }
                }
            }
            names.sort();
            for name in names {
                let value = self.decrypt(o, &name)?;
                let (age, _) = self.paths(o, &name)?;
                write_atomic(&age, &self.keyring.encrypt(&value)?)?;
                n += 1;
            }
            fsync_dir(&self.dir(o));
        }
        Ok(n)
    }
}

/// Write `bytes` to `path` via `<path>.tmp`: 0600, fsync, rename.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let r = (|| -> Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        // A pre-existing temp file keeps its old mode through O_TRUNC.
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r
}

/// Make a rename durable. Best effort: not every filesystem supports it.
pub fn fsync_dir(dir: &Path) {
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::Recipient;

    fn driver(dir: &Path) -> (LocalDriver, OrgId) {
        let k = Keyring::new(age::x25519::Identity::generate(), vec![]);
        (
            LocalDriver::new(dir, Arc::new(k)),
            OrgId::new("ocai").unwrap(),
        )
    }

    fn mode(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn round_trip_and_versions() {
        let dir = tempfile::tempdir().unwrap();
        let (d, org) = driver(dir.path());
        let labels = BTreeMap::from([("team".to_string(), "web".to_string())]);
        let m = d.create(&org, "db_password", b"one", &labels).unwrap();
        assert_eq!((m.version, m.driver.as_str()), (1, "local"));
        assert!(d.create(&org, "db_password", b"again", &labels).is_err());
        assert_eq!(d.get(&org, "db_password").unwrap(), (b"one".to_vec(), 1));
        assert_eq!(d.set(&org, "db_password", b"two").unwrap(), 2);
        assert_eq!(d.set(&org, "db_password", b"three").unwrap(), 3);
        assert_eq!(d.version(&org, "db_password").unwrap(), 3);
        let (v, ver) = d.get(&org, "db_password").unwrap();
        assert_eq!((v.as_slice(), ver), (&b"three"[..], 3));
        let m = d.inspect(&org, "db_password").unwrap();
        assert_eq!(m.labels, labels, "set keeps labels");
        assert!(m.created_at <= m.updated_at);
        // set creates a missing one.
        assert_eq!(d.set(&org, "api_key", b"k").unwrap(), 1);
        // Orgs do not see each other's.
        let other = OrgId::new("norm").unwrap();
        assert!(matches!(
            d.get(&other, "db_password"),
            Err(Error::NotFound(_))
        ));
        assert!(d.list(&other).unwrap().is_empty());
        d.delete(&org, "db_password").unwrap();
        assert!(matches!(
            d.get(&org, "db_password"),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            d.delete(&org, "db_password"),
            Err(Error::NotFound(_))
        ));
        let sec = d.dir(&org);
        assert!(!sec.join("db_password.age").exists());
        assert!(!sec.join("db_password.json").exists());
    }

    #[test]
    fn list_has_metadata_never_values() {
        let dir = tempfile::tempdir().unwrap();
        let (d, org) = driver(dir.path());
        d.create(&org, "b", b"value-b-SENTINEL", &BTreeMap::new())
            .unwrap();
        d.create(&org, "a", b"value-a-SENTINEL", &BTreeMap::new())
            .unwrap();
        let l = d.list(&org).unwrap();
        assert_eq!(
            l.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
        let json = serde_json::to_string(&l).unwrap();
        assert!(!json.contains("SENTINEL"));
        // Nor is the value anywhere on disk in the clear.
        for e in std::fs::read_dir(d.dir(&org)).unwrap() {
            let b = std::fs::read(e.unwrap().path()).unwrap();
            assert!(!String::from_utf8_lossy(&b).contains("SENTINEL"));
        }
    }

    #[test]
    fn files_and_modes() {
        let dir = tempfile::tempdir().unwrap();
        let (d, org) = driver(dir.path());
        d.create(&org, "k", b"v", &BTreeMap::new()).unwrap();
        d.set(&org, "k", b"v2").unwrap();
        let sec = dir.path().join("orgs/ocai/secrets");
        assert_eq!(sec, d.dir(&org));
        assert_eq!(mode(&sec), 0o700);
        assert_eq!(mode(&sec.join("k.age")), 0o600);
        assert_eq!(mode(&sec.join("k.json")), 0o600);
        // No temp files left behind.
        let mut names: Vec<String> = std::fs::read_dir(&sec)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, ["k.age", "k.json"]);
        // The ciphertext is a plain age file.
        let ct = std::fs::read(sec.join("k.age")).unwrap();
        assert!(ct.starts_with(b"age-encryption.org/v1\n"));
        // A stale temp file with loose permissions is tightened on reuse.
        let tmp = sec.join("k.age.tmp");
        std::fs::write(&tmp, b"junk").unwrap();
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644)).unwrap();
        d.set(&org, "k", b"v3").unwrap();
        assert!(!tmp.exists());
        assert_eq!(mode(&sec.join("k.age")), 0o600);
        // A directory created looser is tightened.
        std::fs::set_permissions(&sec, std::fs::Permissions::from_mode(0o755)).unwrap();
        d.set(&org, "k", b"v4").unwrap();
        assert_eq!(mode(&sec), 0o700);
    }

    #[test]
    fn reencrypt_to_an_added_recipient() {
        let dir = tempfile::tempdir().unwrap();
        let daemon = age::x25519::Identity::generate();
        let daemon_text = crate::secrets::keys::identity_file_text(&daemon);
        let k1 = Keyring::new(daemon, vec![]);
        let d1 = LocalDriver::new(dir.path(), Arc::new(k1));
        let org = OrgId::default_org();
        let org2 = OrgId::new("norm").unwrap();
        d1.create(&org, "a", b"alpha", &BTreeMap::new()).unwrap();
        d1.create(&org2, "b", b"beta", &BTreeMap::new()).unwrap();
        let glass = age::x25519::Identity::generate();
        let read_with = |id: &dyn age::Identity, org: &OrgId, n: &str| {
            let ct = std::fs::read(d1.dir(org).join(format!("{n}.age"))).unwrap();
            crate::secrets::inline::decrypt(&ct, &[id])
        };
        assert!(read_with(&glass, &org, "a").is_err());

        // Same daemon key, a break-glass recipient added in config.
        let daemon = crate::secrets::keys::parse_identity(&daemon_text).unwrap();
        let k2 = Keyring::new(daemon, vec![Recipient::X25519(glass.to_public())]);
        let d2 = LocalDriver::new(dir.path(), Arc::new(k2));
        assert_eq!(d2.reencrypt(Some(&org)).unwrap(), 1);
        assert_eq!(read_with(&glass, &org, "a").unwrap(), b"alpha");
        assert!(read_with(&glass, &org2, "b").is_err());
        assert_eq!(d2.reencrypt(None).unwrap(), 2);
        assert_eq!(read_with(&glass, &org2, "b").unwrap(), b"beta");
        // The daemon still reads its own, and versions did not move.
        assert_eq!(d2.get(&org, "a").unwrap(), (b"alpha".to_vec(), 1));
        assert_eq!(d2.get(&org2, "b").unwrap(), (b"beta".to_vec(), 1));
    }

    #[test]
    fn wrong_key_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let (d, org) = driver(dir.path());
        d.create(&org, "k", b"v", &BTreeMap::new()).unwrap();
        let (d2, _) = driver(dir.path());
        let e = d2.get(&org, "k").unwrap_err().to_string();
        assert!(e.contains("cannot decrypt"), "{e}");
        // Listing still works without the key.
        assert_eq!(d2.list(&org).unwrap().len(), 1);
    }
}
