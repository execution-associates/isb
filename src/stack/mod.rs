//! Stacks: a compose file deployed to the `isb serve` daemon, which keeps it
//! running the way docker swarm keeps a stack running, on one host.
//!
//! The daemon's desired state is a file per stack under its state directory,
//! and every instance it creates carries `user.isb.stack`, `user.isb.service`,
//! `user.isb.slot` and `user.isb.rev`. Both survive a daemon restart, so a new
//! daemon picks up exactly where the last one stopped. Apps never depend on
//! the daemon being alive: they are supervised inside their guests (see
//! [`crate::supervise`]) and start with the host.
//!
//! - A service has `deploy.replicas` slots. Each slot holds one instance,
//!   named `<stack>-<service>-<slot>-<id>`.
//! - A service's revision is a hash of everything that shapes an instance. An
//!   instance whose revision is not the current one is replaced, in batches,
//!   per `deploy.update_config`.
//! - Published host ports are served by the daemon's load balancer
//!   ([`crate::balance`]), which only sends traffic to healthy replicas, so a
//!   `start-first` rollout has no gap.

pub mod controller;
pub mod migrate;
pub mod secrets;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::org::OrgId;
use crate::spec::{ComposeFile, SandboxSpec};

pub use controller::Controller;
pub use secrets::SecretBinding;

/// Instance config keys (without `user.`) that tie an instance to its stack.
pub const LABEL_STACK: &str = "isb.stack";
pub const LABEL_SERVICE: &str = "isb.service";
pub const LABEL_SLOT: &str = "isb.slot";
pub const LABEL_REV: &str = "isb.rev";

/// A deployed stack, as the daemon stores it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackDef {
    pub name: String,
    /// The org the stack runs in (its incus project).
    #[serde(default = "OrgId::default_org")]
    pub org: OrgId,
    /// The resolved compose file.
    pub file: ComposeFile,
    /// Where relative bind paths resolve.
    pub base_dir: PathBuf,
    /// The secrets the services use, by top-level key: references into the
    /// org's store by name and version, never values ([`secrets`]).
    #[serde(default)]
    pub secrets: BTreeMap<String, SecretBinding>,
    /// Bumped per service by a forced update, to replace instances whose spec
    /// did not change (a moved image tag, say).
    #[serde(default)]
    pub force: BTreeMap<String, u64>,
    /// Unix seconds.
    pub deployed_at: u64,
    /// Who deployed it (an Access identity, or `local`).
    #[serde(default)]
    pub deployed_by: String,
    /// The deployment this one replaced, for rollback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<Box<StackDef>>,
}

/// A stack's name qualified by its org: `web` in the default org,
/// `alpha/web` in org `alpha`. The controller keys everything by it.
pub fn qualified(org: &OrgId, name: &str) -> String {
    if org.is_default() {
        name.to_string()
    } else {
        format!("{org}/{name}")
    }
}

/// The org and name of a qualified stack name.
pub fn split_qualified(q: &str) -> Result<(OrgId, String)> {
    match q.split_once('/') {
        Some((o, n)) => Ok((OrgId::new(o)?, n.to_string())),
        None => Ok((OrgId::default_org(), q.to_string())),
    }
}

impl StackDef {
    pub fn qualified(&self) -> String {
        qualified(&self.org, &self.name)
    }

    /// The service's revision: a hash of what shapes its instances. Replica
    /// count, rollout settings and dependencies are left out, so changing
    /// them never replaces an instance. A secret counts by its binding
    /// (store name, driver, version), so a new version is a new revision.
    pub fn revision(&self, service: &str) -> Result<String> {
        self.revision_with(service, &|key| {
            self.secrets
                .get(key)
                .map(|b| format!("{}\0{}\0{}", b.name, b.driver, b.version).into_bytes())
                .unwrap_or_default()
        })
    }

    /// [`StackDef::revision`] with each secret's contribution given.
    pub(crate) fn revision_with(
        &self,
        service: &str,
        secret: &dyn Fn(&str) -> Vec<u8>,
    ) -> Result<String> {
        let spec = self.service(service)?;
        let mut s = spec.clone();
        s.name = None;
        s.depends_on.clear();
        // Domains are the ingress's: changing them never replaces an instance.
        s.domains.clear();
        // Published ports are the balancer's, not the instance's.
        s.ports.retain(|p| p.bind == crate::spec::PortBind::Guest);
        if let Some(d) = &mut s.deploy {
            d.replicas = None;
            d.update_config = None;
            d.rollback_config = None;
        }
        if s.deploy.as_ref().is_some_and(|d| *d == Default::default()) {
            s.deploy = None;
        }
        let mut h = Fnv64::new();
        h.write(serde_json::to_string(&s)?.as_bytes());
        for r in &spec.secrets {
            h.write(r.source.as_bytes());
            h.write(&secret(&r.source));
        }
        for (var, key) in &spec.env.secrets {
            h.write(b"env");
            h.write(var.as_bytes());
            h.write(&secret(key));
        }
        // Named volumes are part of the instance's devices; their definitions
        // are only used at creation, but a renamed one must move the instance.
        for v in &spec.volumes {
            if let Some(d) = self.file.volumes.get(&v.source) {
                h.write(serde_json::to_string(d)?.as_bytes());
            }
        }
        h.write(&self.force.get(service).copied().unwrap_or(0).to_le_bytes());
        Ok(format!("{:08x}", h.finish() as u32))
    }

    /// Names in the org's store that the stack's services use (external
    /// ones, and the `<stack>_<key>` ones it owns): what `isb secret rm`
    /// must not pull out from under it.
    pub fn store_secrets(&self) -> std::collections::BTreeSet<String> {
        let used = secrets::used_keys(&self.file);
        self.secrets
            .iter()
            .filter(|(k, _)| used.contains(*k))
            .map(|(_, b)| b.name.clone())
            .collect()
    }

    pub fn service(&self, service: &str) -> Result<&SandboxSpec> {
        self.file
            .services
            .get(service)
            .ok_or_else(|| Error::NotFound(format!("service {service} in stack {}", self.name)))
    }
}

/// FNV-1a, 64-bit: stable across builds and platforms, unlike std's hasher.
struct Fnv64(u64);

impl Fnv64 {
    fn new() -> Self {
        Fnv64(0xcbf29ce484222325)
    }
    fn write(&mut self, b: &[u8]) {
        for x in b {
            self.0 ^= *x as u64;
            self.0 = self.0.wrapping_mul(0x100000001b3);
        }
        // A separator, so ("ab", "c") and ("a", "bc") differ.
        self.0 ^= 0xff;
        self.0 = self.0.wrapping_mul(0x100000001b3);
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

/// Valid stack name: what an instance name prefix allows.
pub fn validate_stack_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 30
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && !name.ends_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "stack name {name:?}: up to 30 characters of [a-z0-9-], starting with a letter"
        )))
    }
}

/// `<stack>-<service>-<slot>-<id>`, checked against incus' 63-character limit.
pub fn instance_name(stack: &str, service: &str, slot: u32, id: &str) -> Result<String> {
    let n = format!(
        "{stack}-{}-{slot}-{id}",
        crate::compose::sanitize_name(service)
    );
    crate::plan::validate_instance_name(&n).map_err(|_| {
        Error::invalid(format!(
            "instance name {n:?} is too long; shorten the stack or service name"
        ))
    })?;
    Ok(n)
}

/// A short random id for a new instance.
pub fn new_id() -> String {
    let mut b = [0u8; 2];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(&mut b);
    }
    format!("{:02x}{:02x}", b[0], b[1])
}

/// Where stack definitions live: `$XDG_STATE_HOME/isb` (else
/// `~/.local/state/isb`), one 0600 file per stack under `stacks/`.
#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn default_dir() -> PathBuf {
        std::env::var_os("XDG_STATE_HOME")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
            .unwrap_or_else(|| PathBuf::from("/var/lib"))
            .join("isb")
    }

    pub fn open(dir: impl Into<PathBuf>) -> Result<Store> {
        let dir = dir.into();
        let stacks = dir.join("stacks");
        std::fs::create_dir_all(&stacks)?;
        set_mode(&dir, 0o700)?;
        set_mode(&stacks, 0o700)?;
        Ok(Store { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, org: &OrgId, name: &str) -> PathBuf {
        self.stacks_dir(org).join(format!("{name}.json"))
    }

    /// Default-org stacks stay where they always were, under `stacks/`.
    fn stacks_dir(&self, org: &OrgId) -> PathBuf {
        if org.is_default() {
            self.dir.join("stacks")
        } else {
            org.dir(&self.dir).join("stacks")
        }
    }

    /// Every stored definition's file, in every org.
    pub fn files(&self) -> Result<Vec<PathBuf>> {
        let mut dirs = vec![self.dir.join("stacks")];
        if let Ok(rd) = std::fs::read_dir(self.dir.join("orgs")) {
            for e in rd.flatten() {
                dirs.push(e.path().join("stacks"));
            }
        }
        let mut out = Vec::new();
        for d in dirs {
            let Ok(rd) = std::fs::read_dir(&d) else {
                continue;
            };
            for e in rd {
                let p = e?.path();
                if p.extension().is_some_and(|x| x == "json") {
                    out.push(p);
                }
            }
        }
        out.sort();
        Ok(out)
    }

    pub fn load_all(&self) -> Result<Vec<StackDef>> {
        let mut out = Vec::new();
        for p in self.files()? {
            let text = std::fs::read_to_string(&p)?;
            match serde_json::from_str::<StackDef>(&text) {
                Ok(d) => out.push(d),
                Err(e) => eprintln!("isb serve: skipping {}: {e}", p.display()),
            }
        }
        out.sort_by_key(|d| d.qualified());
        Ok(out)
    }

    /// Write atomically (temp file, fsync, rename), so a crash never leaves
    /// half a stack behind.
    pub fn save(&self, def: &StackDef) -> Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let dir = self.stacks_dir(&def.org);
        std::fs::create_dir_all(&dir)?;
        set_mode(&dir, 0o700)?;
        let path = self.path(&def.org, &def.name);
        let tmp = path.with_extension("json.tmp");
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(serde_json::to_string_pretty(def)?.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn remove(&self, org: &OrgId, name: &str) -> Result<()> {
        match std::fs::remove_file(self.path(org, name)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }
}

fn set_mode(p: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode))?;
    Ok(())
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(y: &str) -> StackDef {
        StackDef {
            name: "app".into(),
            org: OrgId::default_org(),
            file: serde_yaml_ng::from_str(y).unwrap(),
            base_dir: "/".into(),
            secrets: BTreeMap::new(),
            force: BTreeMap::new(),
            deployed_at: 0,
            deployed_by: String::new(),
            previous: None,
        }
    }

    #[test]
    fn revision_ignores_replicas_and_rollout_settings() {
        let a = def("services:\n  web: {image: x, deploy: {replicas: 1}}\n");
        let b = def(
            "services:\n  web: {image: x, deploy: {replicas: 5, update_config: {order: start-first}}}\n",
        );
        assert_eq!(a.revision("web").unwrap(), b.revision("web").unwrap());
        let c = def("services:\n  web: {image: y}\n");
        assert_ne!(a.revision("web").unwrap(), c.revision("web").unwrap());
        let mut d = a.clone();
        d.force.insert("web".into(), 1);
        assert_ne!(a.revision("web").unwrap(), d.revision("web").unwrap());
    }

    fn binding(name: &str, version: u64) -> SecretBinding {
        SecretBinding {
            name: name.into(),
            driver: "local".into(),
            version,
            owned: false,
        }
    }

    #[test]
    fn revision_follows_secret_versions() {
        let y = "secrets: {k: {external: true}, e: {external: true}}\nservices:\n  web: {image: x, secrets: [k]}\n  api: {image: docker:busybox, environment: {TOKEN: {secret: e}}}\n";
        let mut a = def(y);
        a.secrets.insert("k".into(), binding("k", 1));
        a.secrets.insert("e".into(), binding("e", 1));
        let (web, api) = (a.revision("web").unwrap(), a.revision("api").unwrap());
        // A new version of the file secret rolls web, not api.
        let mut b = a.clone();
        b.secrets.get_mut("k").unwrap().version = 2;
        assert_ne!(b.revision("web").unwrap(), web);
        assert_eq!(b.revision("api").unwrap(), api);
        // A new version of the env secret rolls api, not web.
        let mut c = a.clone();
        c.secrets.get_mut("e").unwrap().version = 2;
        assert_eq!(c.revision("web").unwrap(), web);
        assert_ne!(c.revision("api").unwrap(), api);
        // So does pointing it at another store name, at the same version.
        let mut d = a.clone();
        d.secrets.get_mut("e").unwrap().name = "other".into();
        assert_ne!(d.revision("api").unwrap(), api);
        // And delivering it as another variable.
        let mut e = a.clone();
        let env = &mut e.file.services.get_mut("api").unwrap().env.secrets;
        env.clear();
        env.insert("TOKEN2".into(), "e".into());
        assert_ne!(e.revision("api").unwrap(), api);
        // Bookkeeping is not part of it.
        let mut f = a.clone();
        f.secrets.get_mut("k").unwrap().owned = true;
        f.deployed_at = 99;
        assert_eq!(f.revision("web").unwrap(), web);
    }

    #[test]
    fn revision_without_secrets_is_unchanged_by_bindings() {
        // A stack with no secrets hashes exactly as before secrets became
        // references, so upgrading never rolls it.
        let a = def("services:\n  web: {image: x, environment: {A: '1'}}\n");
        assert_eq!(
            a.revision("web").unwrap(),
            a.revision_with("web", &|_| b"ignored".to_vec()).unwrap()
        );
    }

    #[test]
    fn store_secrets_are_the_bound_names() {
        let mut d = def(concat!(
            "secrets:\n",
            "  a: {external: true}\n",
            "  b: {external: true, name: db.password}\n",
            "  c: {environment: C}\n",
            "  e: {external: true}\n",
            "  unused: {external: true}\n",
            "services:\n",
            "  web: {image: x, secrets: [a, c]}\n",
            "  db: {image: x, secrets: [{source: b, target: pw}], command: [x], environment: {E: {secret: e}}}\n",
        ));
        d.secrets.insert("a".into(), binding("a", 1));
        d.secrets.insert("b".into(), binding("db.password", 1));
        d.secrets.insert("c".into(), binding("app_c", 1));
        d.secrets.insert("e".into(), binding("e", 1));
        // A stale binding for a key no service uses does not count.
        d.secrets.insert("unused".into(), binding("unused", 1));
        let s: Vec<String> = d.store_secrets().into_iter().collect();
        assert_eq!(s, ["a", "app_c", "db.password", "e"]);
    }

    #[test]
    fn environment_round_trips_with_secrets() {
        let d = def(
            "services:\n  web: {image: x, environment: {A: 1, T: {secret: tok}}}\nsecrets: {tok: {external: true}}\n",
        );
        let env = &d.file.services["web"].env;
        assert_eq!(env["A"], "1");
        assert_eq!(env.secrets["T"], "tok");
        let json = serde_json::to_string(&d.file).unwrap();
        assert!(
            json.contains(r#""environment":{"A":"1","T":{"secret":"tok"}}"#),
            "{json}"
        );
        let back: crate::spec::ComposeFile = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d.file);
        assert!(
            serde_yaml_ng::from_str::<crate::spec::SandboxSpec>(
                "image: x\nenvironment: {T: {secret: tok, extra: 1}}\n"
            )
            .is_err()
        );
        assert!(
            serde_yaml_ng::from_str::<crate::spec::SandboxSpec>(
                "image: x\nenvironment: {T: {secret: ''}}\n"
            )
            .is_err()
        );
    }

    #[test]
    fn names() {
        assert_eq!(
            instance_name("app", "web", 2, "ab12").unwrap(),
            "app-web-2-ab12"
        );
        assert!(instance_name(&"a".repeat(30), &"b".repeat(40), 1, "ab12").is_err());
        assert!(validate_stack_name("my-app").is_ok());
        assert!(validate_stack_name("My_App").is_err());
        assert!(validate_stack_name("1app").is_err());
    }

    #[test]
    fn store_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let d = def("services:\n  web: {image: x}\n");
        s.save(&d).unwrap();
        let all = s.load_all().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "app");
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.path().join("stacks/app.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let mut other = d.clone();
        other.org = OrgId::new("alpha").unwrap();
        s.save(&other).unwrap();
        assert!(dir.path().join("orgs/alpha/stacks/app.json").is_file());
        let all = s.load_all().unwrap();
        assert_eq!(
            all.iter().map(|d| d.qualified()).collect::<Vec<_>>(),
            vec!["alpha/app", "app"]
        );
        s.remove(&OrgId::default_org(), "app").unwrap();
        s.remove(&other.org, "app").unwrap();
        assert!(s.load_all().unwrap().is_empty());
    }
}
