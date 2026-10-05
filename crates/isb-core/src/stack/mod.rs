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

mod changes;
pub mod controller;
pub mod deployments;
pub mod failure;
pub mod migrate;
mod ports;
pub mod secrets;
pub mod source;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::org::OrgId;
use crate::spec::{ComposeFile, OnChange, SandboxSpec};

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
    /// The digest each `registry:` image resolved to at deploy time, by
    /// service: a moved tag is a new revision, and a rollback runs exactly
    /// what ran before.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub images: BTreeMap<String, String>,
    /// Unix seconds.
    pub deployed_at: u64,
    /// Who deployed it (an Access identity, or `local`).
    #[serde(default)]
    pub deployed_by: String,
    /// The compose file as written (`${VAR}` unresolved), when it was
    /// deployed as text and needed no variables from the caller: what
    /// stack_export hands out. `file` is what it resolved to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The stack's managed domains merged into `file` ([`source`]), per
    /// service, so a rollback puts them back too.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub domains: BTreeMap<String, Vec<crate::spec::DomainSpec>>,
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
    /// (store name, driver, version), so a new version is a new revision,
    /// unless its `on_change` for the service is `restart` or `none`: then
    /// the version is left out, and a new one is delivered to the running
    /// replicas instead ([`StackDef::live_secrets`]).
    pub fn revision(&self, service: &str) -> Result<String> {
        self.revision_with(service, &|key| {
            self.secrets
                .get(key)
                .map(|b| match self.on_change(service, key) {
                    OnChange::Roll => format!("{}\0{}\0{}", b.name, b.driver, b.version),
                    _ => format!("{}\0{}\0live", b.name, b.driver),
                })
                .map(String::into_bytes)
                .unwrap_or_default()
        })
    }

    /// What a new version of the top-level secret `key` does to `service`:
    /// the service's own references first, then the secret's `on_change`,
    /// then `roll`.
    pub fn on_change(&self, service: &str, key: &str) -> OnChange {
        self.file
            .services
            .get(service)
            .and_then(|s| s.secret_on_change(key))
            .or_else(|| self.file.secrets.get(key).and_then(|d| d.on_change))
            .unwrap_or_default()
    }

    /// The secrets `service` uses whose new versions reach its running
    /// replicas in place (`on_change: restart` or `none`), with the setting
    /// and the version bound now.
    pub fn live_secrets(&self, service: &str) -> BTreeMap<String, (OnChange, u64)> {
        let Ok(spec) = self.service(service) else {
            return BTreeMap::new();
        };
        spec.secret_keys()
            .into_iter()
            .filter_map(|k| {
                let mode = self.on_change(service, k);
                let b = self.secrets.get(k)?;
                (mode != OnChange::Roll).then(|| (k.to_string(), (mode, b.version)))
            })
            .collect()
    }

    /// The services using the top-level secret `key`.
    pub fn services_using(&self, key: &str) -> Vec<String> {
        self.file
            .services
            .iter()
            .filter(|(_, s)| s.secret_keys().contains(key))
            .map(|(n, _)| n.clone())
            .collect()
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
        // What a new secret version does is not part of the instance; the
        // version itself counts below, or not.
        for r in &mut s.secrets {
            r.on_change = None;
        }
        s.env.on_change.clear();
        // Domains are the ingress's: changing them never replaces an instance.
        s.domains.clear();
        // Published ports are the balancer's, not the instance's (UDP: below).
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
        // Only when there is one, so stacks without registry images keep
        // their revisions.
        if let Some(d) = self.images.get(service) {
            h.write(b"image");
            h.write(d.as_bytes());
        }
        // A UDP port is a device on the instance (see `ports`), so a changed
        // one replaces it. Only when there is one, as above.
        for p in ports::published(spec).unwrap_or_default() {
            if p.udp {
                h.write(format!("udp {} {}", p.listen, p.target).as_bytes());
            }
        }
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

    /// A service's image as its instances get it: a `registry:` tag pinned
    /// to the digest it named at deploy time ([`StackDef::images`]).
    pub fn instance_image(&self, service: &str, image: &str) -> String {
        let (Some(r), Some(d)) = (image.strip_prefix("registry:"), self.images.get(service)) else {
            return image.to_string();
        };
        match crate::registry::ImageRef::parse(r) {
            Ok(r) if r.digest.is_none() => format!("registry:{}", r.pinned(d).render()),
            _ => image.to_string(),
        }
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

/// incus' limit on an instance name.
pub const INSTANCE_NAME_MAX: usize = 63;

/// `<stack>-<service>-<slot>-<id>`, within incus' 63-character limit. A
/// name that would be longer keeps the start of the service name plus a
/// hash of all of it (`<stack>-<svc-prefix>-<hash6>-<slot>-<id>`), so two
/// long services in one stack still differ. Nothing parses these names
/// back: replicas are found by their `user.isb.*` labels.
pub fn instance_name(stack: &str, service: &str, slot: u32, id: &str) -> Result<String> {
    let svc = crate::compose::sanitize_name(service);
    let n = format!("{stack}-{svc}-{slot}-{id}");
    if n.len() <= INSTANCE_NAME_MAX {
        crate::plan::validate_instance_name(&n)?;
        return Ok(n);
    }
    let mut h = Fnv64::new();
    h.write(service.as_bytes());
    let hash = format!("{:06x}", h.finish() & 0xff_ffff);
    let tail = format!("-{hash}-{slot}-{id}");
    // What the service prefix may use: the stack, its `-`, and the tail.
    let room = INSTANCE_NAME_MAX.saturating_sub(stack.len() + 1 + tail.len());
    if room < 1 {
        return Err(Error::invalid(format!(
            "instance names of service {service:?} in stack {stack:?} (slot {slot}) cannot fit incus' {INSTANCE_NAME_MAX} characters even with the service name shortened; shorten the stack name ({} characters) or use fewer replicas",
            stack.len()
        )));
    }
    let prefix: String = svc.chars().take(room).collect();
    let n = format!("{stack}-{}{tail}", prefix.trim_end_matches('-'));
    crate::plan::validate_instance_name(&n)?;
    Ok(n)
}

/// Whether `<stack>-<service>-<slot>-<id>` fits as is, without the
/// shortening `instance_name` falls back to.
pub fn instance_name_fits(stack: &str, service: &str, slot: u32) -> bool {
    let n = format!(
        "{stack}-{}-{slot}-0000",
        crate::compose::sanitize_name(service)
    );
    n.len() <= INSTANCE_NAME_MAX && crate::plan::validate_instance_name(&n).is_ok()
}

/// The first valid stack name of `candidates` whose instance names of
/// `service` fit unshortened, else the first `instance_name` can shorten.
/// Unshortened first, because that is how an existing stack (a running
/// preview's) was picked, so it keeps its name.
pub fn pick_stack_name(candidates: &[String], service: &str, slot: u32) -> Option<String> {
    let valid = candidates.iter().filter(|s| validate_stack_name(s).is_ok());
    valid
        .clone()
        .find(|s| instance_name_fits(s, service, slot))
        .or_else(|| {
            valid
                .clone()
                .find(|s| instance_name(s, service, slot, "0000").is_ok())
        })
        .cloned()
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

/// Resolve the paths the local CLI sends with a deploy: the project's own
/// directory, so relative binds and `file:` secrets work as with `isb up`.
pub fn local_deploy_args(
    project: &crate::compose::Project,
    name: &str,
    wait: bool,
    timeout: Option<&str>,
) -> crate::Result<serde_json::Value> {
    // Only `file:`/`environment:` values travel; the daemon reads the rest
    // from the org's store.
    let secrets: std::collections::BTreeMap<String, String> =
        crate::supervise::resolve_secret_values(&project.file, &project.base_dir, &|k| {
            project.lookup(k)
        })?
        .into_iter()
        .map(|(k, v)| {
            String::from_utf8(v)
                .map(|s| (k.clone(), s))
                .map_err(|_| crate::Error::invalid(format!("secret {k:?} is not UTF-8 text")))
        })
        .collect::<crate::Result<_>>()?;
    let mut v = serde_json::json!({
        "name": name,
        "file": project.file,
        "base_dir": project.base_dir,
        "secrets": secrets,
        "wait": wait,
    });
    if let Some(t) = timeout {
        v["timeout"] = serde_json::json!(t);
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(y: &str) -> StackDef {
        StackDef {
            source: None,
            domains: Default::default(),
            name: "app".into(),
            org: OrgId::default_org(),
            file: serde_yaml_ng::from_str(y).unwrap(),
            base_dir: "/".into(),
            secrets: BTreeMap::new(),
            force: BTreeMap::new(),
            images: BTreeMap::new(),
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
    fn udp_ports_are_part_of_the_revision_tcp_ports_are_not() {
        let rev = |ports: &str| {
            def(&format!("services:\n  m: {{image: x, ports: {ports}}}\n"))
                .revision("m")
                .unwrap()
        };
        let none = rev("[]");
        assert_eq!(rev("['8080:80']"), none);
        let udp = rev("['203.0.113.7:10000:10000/udp']");
        assert_ne!(udp, none);
        assert_ne!(rev("['203.0.113.7:10001:10000/udp']"), udp);
    }

    #[test]
    fn on_change_decides_what_a_new_version_rolls() {
        let y = concat!(
            "secrets:\n",
            "  k: {external: true, on_change: restart}\n",
            "  e: {external: true}\n",
            "  n: {external: true, on_change: restart}\n",
            "services:\n",
            "  web: {image: x, secrets: [k, {source: n, on_change: none}]}\n",
            "  api: {image: docker:busybox, secrets: [k], environment: {T: {secret: e, on_change: none}, U: {secret: k, on_change: roll}}}\n",
        );
        let mut a = def(y);
        for k in ["k", "e", "n"] {
            a.secrets.insert(k.into(), binding(k, 1));
        }
        // The service's own references win, the strongest of them; then
        // the top-level setting; then roll.
        assert_eq!(a.on_change("web", "k"), OnChange::Restart);
        assert_eq!(a.on_change("web", "n"), OnChange::None);
        assert_eq!(a.on_change("api", "k"), OnChange::Roll);
        assert_eq!(a.on_change("api", "e"), OnChange::None);
        assert_eq!(
            a.live_secrets("web"),
            BTreeMap::from([
                ("k".to_string(), (OnChange::Restart, 1)),
                ("n".to_string(), (OnChange::None, 1)),
            ])
        );
        assert_eq!(a.services_using("k"), ["api", "web"]);
        let (web, api) = (a.revision("web").unwrap(), a.revision("api").unwrap());
        // k: web restarts in place (same revision), api rolls.
        let mut b = a.clone();
        b.secrets.get_mut("k").unwrap().version = 2;
        assert_eq!(b.revision("web").unwrap(), web);
        assert_ne!(b.revision("api").unwrap(), api);
        // e and n roll nothing.
        let mut c = a.clone();
        c.secrets.get_mut("e").unwrap().version = 2;
        c.secrets.get_mut("n").unwrap().version = 2;
        assert_eq!(c.revision("web").unwrap(), web);
        assert_eq!(c.revision("api").unwrap(), api);
        // Moving between restart and none is not an instance change.
        let mut d = a.clone();
        d.file.secrets.get_mut("k").unwrap().on_change = Some(OnChange::None);
        assert_eq!(d.revision("web").unwrap(), web);
        // An explicit roll hashes exactly as no setting at all.
        let plain = def(&y
            .replace(", on_change: restart}", "}")
            .replace(", on_change: none}", "}")
            .replace(", on_change: roll}", "}"));
        let mut p = plain.clone();
        p.secrets = a.secrets.clone();
        let mut r = p.clone();
        for s in r.file.secrets.values_mut() {
            s.on_change = Some(OnChange::Roll);
        }
        assert_eq!(p.revision("api").unwrap(), r.revision("api").unwrap());
        assert_eq!(p.revision("web").unwrap(), r.revision("web").unwrap());
        // The file round-trips its settings.
        let y2 = serde_yaml_ng::to_string(&a.file).unwrap();
        assert!(y2.contains("on_change: none"), "{y2}");
        let back: ComposeFile = serde_yaml_ng::from_str(&y2).unwrap();
        assert_eq!(back, a.file);
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
        assert!(validate_stack_name("my-app").is_ok());
        assert!(validate_stack_name("My_App").is_err());
        assert!(validate_stack_name("1app").is_err());
    }

    #[test]
    fn names_that_fit_are_unchanged() {
        // 63 exactly: as it always was.
        let stack = "project-management-production";
        let svc = "a".repeat(63 - stack.len() - 1 - "-1-ab12".len());
        let n = instance_name(stack, &svc, 1, "ab12").unwrap();
        assert_eq!(n, format!("{stack}-{svc}-1-ab12"));
        assert_eq!(n.len(), 63);
        assert!(instance_name_fits(stack, &svc, 1));
        assert_eq!(
            instance_name("shop-production", "My_Web", 12, "ab12").unwrap(),
            "shop-production-my-web-12-ab12"
        );
    }

    #[test]
    fn long_names_are_shortened_with_a_hash() {
        let stack = "project-management-production";
        let svc = "project-management-postgres";
        assert!(!instance_name_fits(stack, svc, 1));
        let n = instance_name(stack, svc, 1, "ab12").unwrap();
        assert!(n.len() <= 63, "{n}");
        assert!(n.starts_with(&format!("{stack}-project-")), "{n}");
        assert!(n.ends_with("-1-ab12"), "{n}");
        crate::plan::validate_instance_name(&n).unwrap();
        // Deterministic.
        assert_eq!(n, instance_name(stack, svc, 1, "ab12").unwrap());
        // Two long services sharing a prefix still differ.
        let other = instance_name(stack, "project-management-postgres-replica", 1, "ab12").unwrap();
        assert_ne!(n, other);
        // The slot and id stay at the end, at any width.
        let wide = instance_name(&"a".repeat(30), &"b".repeat(40), 4_000_000_000, "ab12").unwrap();
        assert!(
            wide.len() <= 63 && wide.ends_with("-4000000000-ab12"),
            "{wide}"
        );
        // A stack too long to leave room for any service is refused, saying so.
        let e = instance_name(&"s".repeat(50), "web-frontend-x", 4_000_000_000, "ab12")
            .unwrap_err()
            .to_string();
        assert!(e.contains("shorten the stack name"), "{e}");
    }

    #[test]
    fn stack_names_prefer_unshortened_instances() {
        let c = ["aaaa-production-pr-1".to_string(), "aaaa-pr-1".to_string()];
        assert_eq!(pick_stack_name(&c, "web", 100).unwrap(), c[0]);
        // As before shortening existed: the shorter stack, whose names fit.
        assert_eq!(pick_stack_name(&c, &"w".repeat(35), 100).unwrap(), c[1]);
        // Fits nowhere unshortened: the first, shortened.
        assert_eq!(pick_stack_name(&c, &"w".repeat(60), 100).unwrap(), c[0]);
        assert_eq!(pick_stack_name(&["Bad".to_string()], "web", 1), None);
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
