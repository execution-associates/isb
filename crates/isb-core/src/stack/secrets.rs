//! A stack's secrets: references into its org's store, by name and version.
//!
//! A deployed stack never holds a value. Each top-level secret its services
//! use becomes a [`SecretBinding`]: the name in the org's store (or a
//! driver's reference), the driver, and the version deployed. Values are read
//! from the store when they are delivered, and a new version is a new
//! revision, so the services using it roll.
//!
//! - `external: true` names an existing secret in the org.
//! - `file:` and `environment:` are read by the deploying client, and `age:`
//!   is decrypted with the daemon's key; all three are stored as `local`
//!   secrets named `<stack>_<key>`, as swarm does, and removed with the
//!   stack.
//! - `driver: X, name: REF` is read through driver X.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::org::OrgId;
use crate::secrets::Secrets;
use crate::spec::{ComposeFile, OnChange, SecretDef};

/// One top-level secret a stack uses, as deployed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretBinding {
    /// The name in the org's store, or the driver's reference.
    pub name: String,
    /// The driver holding it (`local`, ...).
    pub driver: String,
    /// The version deployed; a new one rolls the services using it.
    pub version: u64,
    /// The stack made it from a `file:`, `environment:` or `age:` source,
    /// and removes it with the stack.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub owned: bool,
}

impl SecretBinding {
    /// Held outside isb's own store (an external vault): polled for new
    /// versions. A `local` secret changes only through isb, which rolls its
    /// users there and then.
    pub fn is_driver_backed(&self) -> bool {
        self.driver != crate::secrets::local::DRIVER
    }
}

/// `<stack>_<key>`: where a stack keeps a secret it was given a value for.
pub fn owned_name(stack: &str, key: &str) -> Result<String> {
    let n = format!("{stack}_{key}");
    crate::secrets::validate_name(&n)
        .map_err(|e| Error::invalid(format!("secret {key:?} of stack {stack}: {e}")))?;
    Ok(n)
}

/// The top-level secrets a file's services use (as files or variables).
pub fn used_keys(file: &ComposeFile) -> BTreeSet<String> {
    file.services
        .values()
        .flat_map(|s| s.secret_keys())
        .map(String::from)
        .collect()
}

fn declared<'a>(file: &'a ComposeFile, key: &str) -> Result<&'a SecretDef> {
    file.secrets.get(key).ok_or_else(|| {
        Error::invalid(format!(
            "secret {key:?} is not declared under top-level secrets"
        ))
    })
}

/// Bind every secret `file`'s services use, for deploying it as `stack` in
/// `org`. `given` holds the values of `file:`/`environment:` secrets, read by
/// the client. Values the stack owns are stored now, and only when changed,
/// so an unchanged value keeps its version. With `dry_run` nothing is
/// written; the versions are what a deploy would produce.
pub fn bind(
    secrets: &Secrets,
    org: &OrgId,
    stack: &str,
    file: &ComposeFile,
    given: &BTreeMap<String, Vec<u8>>,
    dry_run: bool,
) -> Result<BTreeMap<String, SecretBinding>> {
    let mut out = BTreeMap::new();
    for key in used_keys(file) {
        let def = declared(file, &key)?;
        let b = if let Some(store) = def.store_name(&key) {
            let m = secrets.inspect(org, store).map_err(|e| match e {
                Error::NotFound(_) => Error::invalid(format!(
                    "secret {key:?}: external secret {store} does not exist in org {org}; create it with `isb secret create {store} --org {org}`"
                )),
                e => e,
            })?;
            SecretBinding {
                name: store.to_string(),
                driver: m.driver,
                version: m.version,
                owned: false,
            }
        } else if let Some(driver) = &def.driver {
            let r = def.name.clone().unwrap_or_default();
            let version = secrets
                .version_in(driver, org, &r)
                .map_err(|e| Error::invalid(format!("secret {key:?} ({driver} {r}): {e}")))?;
            SecretBinding {
                name: r,
                driver: driver.clone(),
                version,
                owned: false,
            }
        } else {
            let value = if let Some(text) = &def.age {
                secrets
                    .decrypt_inline(text)
                    .map_err(|e| Error::invalid(format!("secret {key:?}: {e}")))?
            } else {
                given.get(&key).cloned().ok_or_else(|| {
                    Error::invalid(format!("no value for secret {key:?}: pass it in `secrets`"))
                })?
            };
            let name = owned_name(stack, &key)?;
            let m = if dry_run {
                would_put(secrets, org, &name, &value)?
            } else {
                let m = secrets.put(org, &name, &value)?;
                (m.driver, m.version)
            };
            SecretBinding {
                name,
                driver: m.0,
                version: m.1,
                owned: true,
            }
        };
        out.insert(key, b);
    }
    Ok(out)
}

/// The driver and version `put` would leave.
fn would_put(secrets: &Secrets, org: &OrgId, name: &str, value: &[u8]) -> Result<(String, u64)> {
    match secrets.get(org, name) {
        Ok((v, m)) if v == value => Ok((m.driver, m.version)),
        Ok((_, m)) => Ok((m.driver, m.version + 1)),
        Err(Error::NotFound(_)) => Ok((crate::secrets::local::DRIVER.into(), 1)),
        Err(e) => Err(e),
    }
}

impl SecretBinding {
    /// The value now in the store (its version may be newer than this
    /// binding's; the controller rolls to it on its next check).
    pub fn read(&self, secrets: &Secrets, org: &OrgId) -> Result<Vec<u8>> {
        secrets
            .get_in(&self.driver, org, &self.name)
            .map(|(v, _)| v)
            .map_err(|e| Error::invalid(format!("secret {}: {e}", self.name)))
    }
}

/// The values of the given keys, read from the store through the stack's
/// bindings.
pub fn values<'a>(
    secrets: &Secrets,
    org: &OrgId,
    bindings: &BTreeMap<String, SecretBinding>,
    keys: impl IntoIterator<Item = &'a str>,
) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut out = BTreeMap::new();
    for key in keys {
        let b = bindings.get(key).ok_or_else(|| {
            Error::invalid(format!(
                "secret {key:?} is not bound in this deployment; deploy the stack again"
            ))
        })?;
        out.insert(key.to_string(), b.read(secrets, org)?);
    }
    Ok(out)
}

/// The values of a file's store-backed secrets (`external`, `age`,
/// `driver`), for `isb up`, which runs no stack. `file:` and `environment:`
/// secrets are skipped: the client reads those itself.
pub fn resolve(
    secrets: &Secrets,
    org: &OrgId,
    defs: &BTreeMap<String, SecretDef>,
) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut out = BTreeMap::new();
    for (key, def) in defs {
        def.validate()
            .map_err(|e| Error::invalid(format!("secret {key:?}: {e}")))?;
        let v = if let Some(store) = def.store_name(key) {
            secrets.get(org, store).map(|(v, _)| v)
        } else if let Some(text) = &def.age {
            secrets.decrypt_inline(text)
        } else if let Some(driver) = &def.driver {
            secrets
                .get_in(driver, org, def.name.as_deref().unwrap_or_default())
                .map(|(v, _)| v)
        } else {
            continue;
        };
        out.insert(
            key.clone(),
            v.map_err(|e| Error::invalid(format!("secret {key:?}: {e}")))?,
        );
    }
    Ok(out)
}

/// What a new version of a secret did to one service of a stack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cycle {
    /// The stack's name in its org.
    pub stack: String,
    pub service: String,
    /// The top-level secret key in the stack's file.
    pub key: String,
    /// The store name, or the driver's reference.
    pub secret: String,
    pub from: u64,
    pub to: u64,
    /// `roll`: a rolling update replaces the replicas. `restart`: each
    /// replica gets the value and its app is restarted in place. `none`:
    /// the value is delivered where it can be, nothing restarts, and the
    /// replicas are stale until they next start.
    pub action: OnChange,
}

impl Cycle {
    /// The replicas run the new value once this is done.
    pub fn cycles(&self) -> bool {
        self.action != OnChange::None
    }
}

/// The stacks with a service that cycles (rolls or restarts), by name.
pub fn cycled_stacks(cycles: &[Cycle]) -> Vec<String> {
    let mut v: Vec<String> = cycles
        .iter()
        .filter(|c| c.cycles())
        .map(|c| c.stack.clone())
        .collect();
    v.sort();
    v.dedup();
    v
}

/// A forced refresh: the (driver, version) of every binding to the name, and
/// what the new version did, per service.
pub type Refreshed = (Vec<(String, u64)>, Vec<Cycle>);

/// Instance config key (without `user.`) holding the versions of the
/// `restart`/`none` secrets its app last started with, as a JSON object.
/// A replica whose versions are behind the stack's is stale.
pub const LABEL_SECRETS: &str = "isb.secrets";

/// [`LABEL_SECRETS`]'s value.
pub fn versions_label(v: &BTreeMap<String, u64>) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

/// [`LABEL_SECRETS`] read back; `None` when absent or unreadable.
pub fn parse_versions_label(s: Option<&str>) -> Option<BTreeMap<String, u64>> {
    serde_json::from_str(s?).ok()
}

/// A secret a replica runs an older version of (`on_change: none`, or a
/// restart still to come).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StaleSecret {
    pub key: String,
    /// The version its app started with.
    pub running: u64,
    /// The version bound now, delivered to its files.
    pub current: u64,
}

/// Which of `want`'s secrets a replica that started with `have` runs an
/// older version of. A secret missing from `have` is not stale: the replica
/// predates the setting, and is taken to run what is bound.
pub fn stale(have: &BTreeMap<String, u64>, want: &BTreeMap<String, u64>) -> Vec<StaleSecret> {
    want.iter()
        .filter_map(|(k, cur)| {
            let run = *have.get(k)?;
            (run != *cur).then(|| StaleSecret {
                key: k.clone(),
                running: run,
                current: *cur,
            })
        })
        .collect()
}

/// One polling round's answers: the version of each `(org, driver, name)`,
/// or why it could not be read.
pub type Polled = BTreeMap<(OrgId, String, String), std::result::Result<u64, String>>;

/// The current version of every `(org, driver, name)`, one polling round:
/// each driver is asked once per org for all its names, so it can answer
/// names that share a version (fields of one 1Password item) with one
/// lookup.
pub fn poll_versions(
    secrets: &Secrets,
    refs: impl IntoIterator<Item = (OrgId, String, String)>,
) -> Polled {
    let mut groups: BTreeMap<(OrgId, String), BTreeSet<String>> = BTreeMap::new();
    for (org, driver, name) in refs {
        groups.entry((org, driver)).or_default().insert(name);
    }
    let mut out = BTreeMap::new();
    for ((org, driver), names) in groups {
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let got = secrets.versions_in(&driver, &org, &names);
        for (n, v) in names.iter().zip(got) {
            out.insert(
                (org.clone(), driver.clone(), (*n).to_string()),
                v.map_err(|e| e.to_string()),
            );
        }
    }
    out
}

/// When each driver-backed binding is next due for a version check.
#[derive(Debug, Default)]
pub struct RefreshSchedule {
    next: BTreeMap<(String, String), Instant>,
}

impl RefreshSchedule {
    /// The `(stack, key)` pairs due at `now`, given each stack's bindings
    /// and declared refresh intervals; each one returned is rescheduled. A
    /// binding seen for the first time is due one interval after `now`: it
    /// was just read at deploy.
    pub fn due<'a>(
        &mut self,
        stacks: impl IntoIterator<Item = (&'a str, &'a super::StackDef)>,
        now: Instant,
    ) -> Vec<(String, String)> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for (q, def) in stacks {
            for (key, b) in &def.secrets {
                if !b.is_driver_backed() {
                    continue;
                }
                let decl = def.file.secrets.get(key);
                let every = decl
                    .map(SecretDef::refresh_interval)
                    .unwrap_or(crate::spec::DEFAULT_SECRET_REFRESH);
                let id = (q.to_string(), key.clone());
                seen.insert(id.clone());
                let next = self.next.entry(id.clone()).or_insert(now + every);
                if now >= *next {
                    *next = now + every;
                    out.push(id);
                }
            }
        }
        // Forget stacks and keys that went away.
        self.next.retain(|k, _| seen.contains(k));
        out
    }

    /// Check sooner than scheduled (a forced refresh).
    pub fn reset(&mut self, q: &str, key: &str, every: Duration, now: Instant) {
        self.next
            .insert((q.to_string(), key.to_string()), now + every);
    }
}

#[cfg(test)]
pub(crate) mod tests_support {
    use super::*;
    use crate::secrets::{Driver, Keyring, LocalDriver, SecretMeta};
    use std::sync::{Arc, Mutex};

    /// An external vault whose version the test moves.
    pub(crate) struct Vault(pub Mutex<u64>);
    impl Driver for Vault {
        fn name(&self) -> &str {
            "vault"
        }
        fn get(&self, org: &OrgId, name: &str) -> Result<(Vec<u8>, u64)> {
            let v = self.version(org, name)?;
            Ok((format!("{name}@{v}").into_bytes(), v))
        }
        fn version(&self, org: &OrgId, name: &str) -> Result<u64> {
            if name.starts_with("op://") {
                Ok(*self.0.lock().unwrap())
            } else {
                Err(crate::secrets::not_found(org, name))
            }
        }
        fn inspect(&self, org: &OrgId, name: &str) -> Result<SecretMeta> {
            Err(crate::secrets::not_found(org, name))
        }
        fn list(&self, _org: &OrgId) -> Result<Vec<SecretMeta>> {
            Ok(vec![])
        }
    }

    pub(crate) fn store(dir: &std::path::Path) -> (Secrets, Arc<Vault>) {
        let k = Keyring::new(age::x25519::Identity::generate(), vec![]);
        let vault = Arc::new(Vault(Mutex::new(3)));
        let s = Secrets::new(LocalDriver::new(dir, Arc::new(k)))
            .with_driver(vault.clone())
            .unwrap();
        (s, vault)
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::store;
    use super::*;
    use crate::secrets::Keyring;

    fn file(y: &str) -> ComposeFile {
        serde_yaml_ng::from_str(y).unwrap()
    }

    const FILE: &str = concat!(
        "secrets:\n",
        "  db: {external: true, name: db.password}\n",
        "  tok: {environment: TOK}\n",
        "  cert: {file: ./cert.pem}\n",
        "  api: {driver: vault, name: 'op://v/api/key', refresh: 30m}\n",
        "  unused: {external: true}\n",
        "services:\n",
        "  web:\n",
        "    image: docker:busybox\n",
        "    secrets: [db, cert]\n",
        "    environment: {TOKEN: {secret: tok}, API: {secret: api}, PLAIN: x}\n",
    );

    #[test]
    fn binds_every_source_by_name_and_version() {
        let dir = tempfile::tempdir().unwrap();
        let (s, _) = store(dir.path());
        let org = OrgId::default_org();
        let f = file(FILE);
        assert_eq!(
            used_keys(&f).into_iter().collect::<Vec<_>>(),
            ["api", "cert", "db", "tok"]
        );
        let given = BTreeMap::from([
            ("tok".to_string(), b"t0k".to_vec()),
            ("cert".to_string(), b"PEM".to_vec()),
        ]);
        // The external secret must exist.
        let e = bind(&s, &org, "app", &f, &given, false).unwrap_err();
        assert!(
            e.to_string().contains("isb secret create db.password"),
            "{e}"
        );
        s.create(&org, "db.password", None, b"pw", &BTreeMap::new())
            .unwrap();
        // A dry run writes nothing.
        let dry = bind(&s, &org, "app", &f, &given, true).unwrap();
        assert_eq!(dry["tok"].version, 1);
        assert!(s.inspect(&org, "app_tok").is_err());
        let b = bind(&s, &org, "app", &f, &given, false).unwrap();
        assert_eq!(dry, b);
        assert_eq!(
            b["db"],
            SecretBinding {
                name: "db.password".into(),
                driver: "local".into(),
                version: 1,
                owned: false
            }
        );
        assert_eq!(
            (b["tok"].name.as_str(), b["tok"].version, b["tok"].owned),
            ("app_tok", 1, true)
        );
        assert_eq!(b["cert"].name, "app_cert");
        assert_eq!((b["api"].driver.as_str(), b["api"].version), ("vault", 3));
        assert!(!b.contains_key("unused"));
        // Values come from the store, never from the binding.
        let v = values(&s, &org, &b, ["tok", "db", "api"]).unwrap();
        assert_eq!(v["tok"], b"t0k");
        assert_eq!(v["db"], b"pw");
        assert_eq!(v["api"], b"op://v/api/key@3");
        assert!(values(&s, &org, &b, ["nope"]).is_err());
        // The same value again keeps the version; a new one bumps it.
        let again = bind(&s, &org, "app", &f, &given, false).unwrap();
        assert_eq!(again["tok"].version, 1);
        let mut given2 = given.clone();
        given2.insert("tok".into(), b"new".to_vec());
        assert_eq!(
            bind(&s, &org, "app", &f, &given2, true).unwrap()["tok"].version,
            2
        );
        assert_eq!(
            bind(&s, &org, "app", &f, &given2, false).unwrap()["tok"].version,
            2
        );
        // A missing client value is an error naming the secret.
        let e = bind(&s, &org, "app", &f, &BTreeMap::new(), false).unwrap_err();
        assert!(e.to_string().contains("no value for secret"), "{e}");
    }

    #[test]
    fn inline_age_is_decrypted_and_stored() {
        let dir = tempfile::tempdir().unwrap();
        let (s, _) = store(dir.path());
        let org = OrgId::new("alpha").unwrap();
        let armored = s.encrypt_inline(b"inline-value").unwrap();
        let mut f = file("services:\n  web: {image: x, secrets: [k]}\n");
        f.secrets.insert(
            "k".into(),
            SecretDef {
                age: Some(armored.clone()),
                ..Default::default()
            },
        );
        let b = bind(&s, &org, "web", &f, &BTreeMap::new(), false).unwrap();
        assert_eq!((b["k"].name.as_str(), b["k"].version), ("web_k", 1));
        assert_eq!(s.get(&org, "web_k").unwrap().0, b"inline-value");
        // Re-encrypting the same value (new ciphertext) is no new version.
        f.secrets.get_mut("k").unwrap().age = Some(s.encrypt_inline(b"inline-value").unwrap());
        assert_eq!(
            bind(&s, &org, "web", &f, &BTreeMap::new(), false).unwrap()["k"].version,
            1
        );
        // Ciphertext for another key fails, naming the secret.
        let other = Keyring::new(age::x25519::Identity::generate(), vec![]);
        let foreign = crate::secrets::encrypt_inline(b"x", other.recipients()).unwrap();
        f.secrets.get_mut("k").unwrap().age = Some(foreign);
        let e = bind(&s, &org, "web", &f, &BTreeMap::new(), false).unwrap_err();
        assert!(e.to_string().contains("secret \"k\""), "{e}");
        // isb up's resolution reads the same sources.
        f.secrets.get_mut("k").unwrap().age = Some(armored);
        let r = resolve(&s, &org, &f.secrets).unwrap();
        assert_eq!(r["k"], b"inline-value");
    }

    #[test]
    fn resolve_reads_store_backed_sources_only() {
        let dir = tempfile::tempdir().unwrap();
        let (s, _) = store(dir.path());
        let org = OrgId::default_org();
        s.create(&org, "db.password", None, b"pw", &BTreeMap::new())
            .unwrap();
        let f = file(FILE);
        let r = resolve(&s, &org, &f.secrets).unwrap_err();
        assert!(r.to_string().contains("unused"), "{r}");
        let mut defs = f.secrets.clone();
        defs.remove("unused");
        let r = resolve(&s, &org, &defs).unwrap();
        assert_eq!(
            r.keys().map(String::as_str).collect::<Vec<_>>(),
            ["api", "db"]
        );
        assert_eq!(r["db"], b"pw");
    }

    #[test]
    fn refresh_schedule() {
        let mut def = super::super::StackDef {
            name: "app".into(),
            org: OrgId::default_org(),
            file: file(FILE),
            base_dir: "/".into(),
            secrets: BTreeMap::new(),
            force: BTreeMap::new(),
            images: BTreeMap::new(),
            deployed_at: 0,
            deployed_by: String::new(),
            previous: None,
        };
        let bind = |name: &str, driver: &str| SecretBinding {
            name: name.into(),
            driver: driver.into(),
            version: 1,
            owned: false,
        };
        def.secrets
            .insert("db".into(), bind("db.password", "local"));
        def.secrets
            .insert("api".into(), bind("op://v/api/key", "vault"));
        // A driver binding without a declared refresh uses the default.
        let mut f2 = def.file.clone();
        f2.secrets.get_mut("api").unwrap().refresh = None;
        let mut def2 = def.clone();
        def2.name = "two".into();
        def2.file = f2;

        let mut sch = RefreshSchedule::default();
        let t0 = Instant::now();
        let stacks = |a: &super::super::StackDef, b: &super::super::StackDef| {
            vec![
                ("app".to_string(), a.clone()),
                ("two".to_string(), b.clone()),
            ]
        };
        let list = stacks(&def, &def2);
        let it = || list.iter().map(|(q, d)| (q.as_str(), d));
        // Nothing is due right after deploy; local bindings never are.
        assert!(sch.due(it(), t0).is_empty());
        assert!(sch.due(it(), t0 + Duration::from_secs(29 * 60)).is_empty());
        let d = sch.due(it(), t0 + Duration::from_secs(30 * 60));
        assert_eq!(d, [("app".to_string(), "api".to_string())]);
        // Rescheduled: not due again until another 30m.
        assert!(sch.due(it(), t0 + Duration::from_secs(31 * 60)).is_empty());
        let d = sch.due(it(), t0 + Duration::from_secs(60 * 60));
        assert_eq!(
            d,
            [
                ("app".to_string(), "api".to_string()),
                ("two".to_string(), "api".to_string())
            ]
        );
        // A forced check moves the next one.
        sch.reset("app", "api", Duration::from_secs(30 * 60), t0);
        assert_eq!(sch.due(it(), t0 + Duration::from_secs(30 * 60)).len(), 1);
        // A stack that went away is forgotten.
        let only = [("app".to_string(), def.clone())];
        sch.due(only.iter().map(|(q, d)| (q.as_str(), d)), t0);
        assert_eq!(sch.next.len(), 1);
    }

    #[test]
    fn owned_names() {
        assert_eq!(owned_name("app", "db").unwrap(), "app_db");
        assert!(owned_name("app", "a/b").is_err());
    }
}
