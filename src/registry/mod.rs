//! The local OCI registry: where builds push and incus pulls.
//!
//! One per host, run by isb as an OCI container (`registry:2`, pinned by
//! digest) in the system project [`PROJECT`], never in an org:
//!
//! - It has no network interface. A proxy device listening on the host's
//!   `127.0.0.1:<port>` is its only way in, so incusd (and the daemon) reach
//!   it and no org network can, by construction.
//! - It speaks TLS with a certificate from an isb CA kept in the daemon's
//!   state directory ([`tls`]); `isb host setup` installs that CA where the
//!   skopeo inside incusd looks for it, since incus pulls OCI images only
//!   over https.
//! - Only the daemon pushes. A build exports an OCI layout inside its
//!   sandbox; the daemon copies it out and pushes it ([`oci`]). Org
//!   sandboxes never get credentials for, or a route to, the registry.
//! - Repositories are `<org>/<app>`. A compose file names an image as
//!   `registry:<app>:<tag>` (or `@sha256:...`), always resolved in the org
//!   the stack or sandbox lives in, so one org cannot name another's images.

pub mod oci;
pub mod tls;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::client::{Client, encode_segment};
use crate::error::{Error, Result};
use crate::org::OrgId;

/// The incus project for isb's own services. Not an org: `system` is a
/// reserved org name.
pub const PROJECT: &str = "isb-system";
pub const INSTANCE: &str = "registry";
pub const VOLUME: &str = "registry-data";
/// `registry:2.8.3`, by digest, so what runs is what was reviewed.
pub const IMAGE: &str =
    "docker:registry@sha256:a3d8aaa63ed8681a604f1dea0aa03f100d5895b6a58ace528858a7b332415373";
pub const DEFAULT_PORT: u16 = 5480;
/// Tags kept per repository by [`Registry::gc`], besides deployed ones.
pub const DEFAULT_KEEP: usize = 10;

/// Where the registry listens inside its container: a unix socket, since a
/// container without a NIC has no loopback up either.
const INNER_SOCKET: &str = "/tmp/registry.sock";
const KEY_ADDR: &str = "user.isb.registry.addr";
const KEY_CA: &str = "user.isb.registry.ca";
const CERT_DIR: &str = "/certs";

/// What a compose file's `registry:` image names: an app's repository in
/// the org, and a tag and/or a digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRef {
    pub app: String,
    pub tag: Option<String>,
    pub digest: Option<String>,
}

/// An app name as a repository component: `[a-z0-9][a-z0-9._-]*`, at most
/// 128 characters, no `/` (the org is the only namespace).
pub fn valid_app(a: &str) -> bool {
    !a.is_empty()
        && a.len() <= 128
        && a.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && a.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c))
}

/// An OCI tag: `[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}`.
pub fn valid_tag(t: &str) -> bool {
    !t.is_empty()
        && t.len() <= 128
        && !t.starts_with(['.', '-'])
        && t.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
}

impl ImageRef {
    /// `APP[:TAG][@sha256:...]`, what follows `registry:`.
    pub fn parse(s: &str) -> Result<ImageRef> {
        let bad = |why: &str| {
            Error::invalid(format!(
                "registry image {s:?}: {why} (want registry:APP[:TAG][@sha256:DIGEST], the app's image in this org)"
            ))
        };
        let (rest, digest) = match s.split_once('@') {
            Some((r, d)) => {
                if !oci::valid_digest(d) {
                    return Err(bad("the digest must be sha256: and 64 hex digits"));
                }
                (r, Some(d.to_string()))
            }
            None => (s, None),
        };
        let (app, tag) = match rest.split_once(':') {
            Some((a, t)) => (a, Some(t.to_string())),
            None => (rest, None),
        };
        if app.contains('/') {
            return Err(bad(
                "images are named by app alone; the org is implied and other orgs' images cannot be named",
            ));
        }
        if !valid_app(app) {
            return Err(bad("the app is [a-z0-9][a-z0-9._-]*"));
        }
        if let Some(t) = &tag {
            if !valid_tag(t) {
                return Err(bad("bad tag"));
            }
        }
        Ok(ImageRef {
            app: app.to_string(),
            tag,
            digest,
        })
    }

    /// The tag, `latest` by default.
    pub fn tag_or_latest(&self) -> &str {
        self.tag.as_deref().unwrap_or("latest")
    }

    /// `APP:TAG@DIGEST`, as written after `registry:`.
    pub fn render(&self) -> String {
        let mut s = self.app.clone();
        if let Some(t) = &self.tag {
            s.push(':');
            s.push_str(t);
        }
        if let Some(d) = &self.digest {
            s.push('@');
            s.push_str(d);
        }
        s
    }

    /// This reference pinned to `digest` (the tag is kept for people).
    pub fn pinned(&self, digest: &str) -> ImageRef {
        ImageRef {
            digest: Some(digest.to_string()),
            ..self.clone()
        }
    }

    /// The incus `alias` to pull: `<org>/<app>@digest`, or `:tag`. Skopeo
    /// refuses a tag and a digest together, and the digest is what counts.
    pub fn pull_alias(&self, org: &OrgId) -> String {
        match &self.digest {
            Some(d) => format!("{}@{d}", repo(org, &self.app)),
            None => format!("{}:{}", repo(org, &self.app), self.tag_or_latest()),
        }
    }
}

/// An org's repository for an app.
pub fn repo(org: &OrgId, app: &str) -> String {
    format!("{org}/{app}")
}

/// Where the registry is, as recorded on the system project (so any isb
/// process with incus access finds it, the daemon's state dir or not).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Info {
    /// `127.0.0.1:5480`
    pub addr: String,
    pub ca_pem: String,
}

impl Info {
    pub fn url(&self) -> String {
        format!("https://{}", self.addr)
    }
}

fn host(base: &Client) -> Client {
    base.clone().project("default")
}

fn sys(base: &Client) -> Client {
    base.clone().project(PROJECT)
}

/// The registry's address and CA, if it has been set up.
pub fn info(base: &Client) -> Result<Option<Info>> {
    let Some(p) = host(base).get_opt(&format!("/1.0/projects/{PROJECT}"))? else {
        return Ok(None);
    };
    let addr = p["config"][KEY_ADDR].as_str().unwrap_or_default();
    let ca = p["config"][KEY_CA].as_str().unwrap_or_default();
    if addr.is_empty() || ca.is_empty() {
        return Ok(None);
    }
    Ok(Some(Info {
        addr: addr.to_string(),
        ca_pem: ca.to_string(),
    }))
}

/// Where skopeo (inside incusd) looks for the CA of `addr`.
pub fn host_ca_path(addr: &str) -> PathBuf {
    Path::new("/etc/containers/certs.d")
        .join(addr)
        .join("ca.crt")
}

/// Create the registry, or bring it in line: the system project, its data
/// volume, the TLS material (in `<state>/registry`), the container and its
/// loopback proxy. Safe to run again; `renew` reissues the certificate.
#[allow(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
pub fn setup(
    base: &Client,
    state: &Path,
    port: u16,
    renew: bool,
    report: &mut dyn FnMut(&str),
) -> Result<Info> {
    let ip: std::net::IpAddr = "127.0.0.1".parse().expect("constant");
    let addr = format!("{ip}:{port}");
    let mat = tls::ensure(&state.join("registry"), ip, renew)?;
    let h = host(base);
    let other = h.get_timeouts().other;

    let proj_path = format!("/1.0/projects/{PROJECT}");
    let config = json!({
        "features.images": "false",
        "features.profiles": "true",
        "features.storage.volumes": "true",
        "features.networks": "false",
        KEY_ADDR: addr,
        KEY_CA: mat.ca_cert,
    });
    match h.get_opt(&proj_path)? {
        None => {
            report(&format!("creating project {PROJECT}"));
            h.mutate(
                "POST",
                "/1.0/projects",
                Some(&json!({"name": PROJECT, "description": "isb system services (not an org)", "config": config})),
                &format!("create project {PROJECT}"),
                other,
            )?;
        }
        Some(p) => {
            let mut merged = p["config"].clone();
            for (k, v) in config.as_object().expect("object") {
                merged[k] = v.clone();
            }
            h.mutate(
                "PUT",
                &proj_path,
                Some(&json!({"description": p["description"], "config": merged})),
                &format!("update project {PROJECT}"),
                other,
            )?;
        }
    }
    let s = sys(base);
    let facts = crate::sandbox::host_facts(&h)?;
    let pool = facts.pick_pool(None)?;
    // A root disk and nothing else: no NIC.
    s.mutate(
        "PUT",
        "/1.0/profiles/default",
        Some(&json!({
            "description": "isb system services: no network",
            "config": {},
            "devices": {"root": {"type": "disk", "path": "/", "pool": pool}},
        })),
        "set the system project's default profile",
        other,
    )?;
    let vol_path = format!(
        "/1.0/storage-pools/{}/volumes/custom/{VOLUME}",
        encode_segment(&pool)
    );
    if s.get_opt(&vol_path)?.is_none() {
        report(&format!("creating volume {VOLUME} on {pool}"));
        s.mutate(
            "POST",
            &format!("/1.0/storage-pools/{}/volumes/custom", encode_segment(&pool)),
            Some(&json!({"name": VOLUME, "type": "custom", "content_type": "filesystem", "config": {}})),
            &format!("create volume {VOLUME}"),
            other,
        )?;
    }

    let env = [
        ("REGISTRY_HTTP_NET", "unix".into()),
        ("REGISTRY_HTTP_ADDR", INNER_SOCKET.to_string()),
        (
            "REGISTRY_HTTP_TLS_CERTIFICATE",
            format!("{CERT_DIR}/tls.crt"),
        ),
        ("REGISTRY_HTTP_TLS_KEY", format!("{CERT_DIR}/tls.key")),
        ("REGISTRY_STORAGE_DELETE_ENABLED", "true".into()),
        (
            "REGISTRY_STORAGE_FILESYSTEM_ROOTDIRECTORY",
            "/var/lib/registry".into(),
        ),
        ("REGISTRY_LOG_LEVEL", "warn".into()),
    ];
    let mut cfg = json!({
        "boot.autostart": "true",
        "limits.cpu": "2",
        "limits.memory": "1GiB",
        "user.isb.role": "registry",
    });
    for (k, v) in &env {
        cfg[format!("environment.{k}")] = json!(v);
    }
    let devices = json!({
        "data": {"type": "disk", "pool": pool, "source": VOLUME, "path": "/var/lib/registry"},
        "https": {
            "type": "proxy",
            "listen": format!("tcp:{addr}"),
            "connect": format!("unix:{INNER_SOCKET}"),
            "bind": "host",
        },
    });
    let inst_path = format!("/1.0/instances/{INSTANCE}");
    let mut restart = false;
    match s.get_opt(&inst_path)? {
        None => {
            report(&format!("creating {INSTANCE} from {IMAGE}"));
            let src = crate::plan::ImageSource::parse(IMAGE)?;
            s.mutate(
                "POST",
                "/1.0/instances",
                Some(&json!({
                    "name": INSTANCE,
                    "type": "container",
                    "source": src.to_api(None),
                    "config": cfg,
                    "devices": devices,
                    "profiles": ["default"],
                })),
                &format!("create {INSTANCE}"),
                s.get_timeouts().create,
            )?;
        }
        Some(i) => {
            let mut c = i["config"].clone();
            let mut d = i["devices"].clone();
            let mut changed = false;
            for (k, v) in cfg.as_object().expect("object") {
                if c[k] != *v {
                    c[k] = v.clone();
                    changed = true;
                }
            }
            for (k, v) in devices.as_object().expect("object") {
                if d[k] != *v {
                    d[k] = v.clone();
                    changed = true;
                }
            }
            if changed {
                report(&format!("updating {INSTANCE}"));
                s.mutate(
                    "PATCH",
                    &inst_path,
                    Some(&json!({"config": c, "devices": d})),
                    &format!("update {INSTANCE}"),
                    other,
                )?;
                restart = true;
            }
        }
    }
    s.make_dir(INSTANCE, CERT_DIR, 0, 0, 0o700)?;
    for (name, text, mode) in [("tls.crt", &mat.cert, 0o644), ("tls.key", &mat.key, 0o600)] {
        let path = format!("{CERT_DIR}/{name}");
        if s.read_file(INSTANCE, &path)?.as_deref() != Some(text.as_bytes()) {
            s.push_file(INSTANCE, &path, text.as_bytes(), 0, 0, mode)?;
            restart = true;
        }
    }
    let state_now = s.get(&format!("{inst_path}/state"))?;
    let running = state_now["status"].as_str() == Some("Running");
    let action = match (running, restart) {
        (false, _) => Some("start"),
        (true, true) => Some("restart"),
        (true, false) => None,
    };
    if let Some(a) = action {
        report(&format!("{a}ing {INSTANCE}"));
        s.mutate(
            "PUT",
            &format!("{inst_path}/state"),
            Some(&json!({"action": a, "timeout": 30, "force": true})),
            &format!("{a} {INSTANCE}"),
            other,
        )?;
    }
    let info = Info {
        addr,
        ca_pem: mat.ca_cert,
    };
    wait_up(&info, Duration::from_secs(60))?;
    Ok(info)
}

/// Wait until the registry answers `/v2/`.
fn wait_up(info: &Info, deadline: Duration) -> Result<()> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(5)))
        .http_status_as_error(false)
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::new_with_certs(&[
                    ureq::tls::Certificate::from_pem(info.ca_pem.as_bytes())
                        .map_err(|e| Error::invalid(format!("registry CA: {e}")))?,
                ]))
                .build(),
        )
        .build()
        .into();
    let started = Instant::now();
    let mut last = String::new();
    while started.elapsed() < deadline {
        match agent.get(format!("{}/v2/", info.url())).call() {
            Ok(r) if r.status().as_u16() == 200 => return Ok(()),
            Ok(r) => last = format!("HTTP {}", r.status()),
            Err(e) => last = e.to_string(),
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err(Error::invalid(format!(
        "registry {} not answering after {deadline:?}: {last}",
        info.url()
    )))
}

/// When an app's tag was pushed, as the daemon recorded it: retention keeps
/// the newest.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PushIndex {
    /// repo -> tag -> (digest, unix seconds)
    #[serde(default)]
    repos: BTreeMap<String, BTreeMap<String, (String, u64)>>,
}

/// One tag of a repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TagInfo {
    pub tag: String,
    pub digest: String,
    /// Unix seconds; 0 when the push was not recorded.
    pub pushed_at: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepoInfo {
    pub repo: String,
    pub org: String,
    pub app: String,
    pub tags: Vec<TagInfo>,
}

/// What a garbage collection did (or, dry, would do).
#[derive(Debug, Clone, Default, Serialize)]
pub struct GcReport {
    /// `repo@digest` with the tags that pointed at it.
    pub deleted: Vec<String>,
    pub kept: usize,
    pub dry_run: bool,
    /// The registry's own garbage-collect output, last lines.
    pub collect: String,
}

/// Which manifests retention deletes from one repository: everything but
/// the newest `keep` tags and the `protected` digests (deployed ones). A
/// digest is deleted only when no kept tag points at it.
pub fn select_deletions(
    tags: &[TagInfo],
    keep: usize,
    protected: &BTreeSet<String>,
) -> Vec<String> {
    // Retention's own tags ([`KEEP_TAG`]) and previews' tags
    // ([`is_preview_tag`]) never count as one of the newest: a preview's
    // image is kept while it is deployed (protected), and no longer.
    let aside = |t: &TagInfo| t.tag.starts_with(KEEP_TAG) || is_preview_tag(&t.tag);
    let mut sorted: Vec<&TagInfo> = tags.iter().collect();
    sorted.sort_by(|a, b| {
        aside(a)
            .cmp(&aside(b))
            .then_with(|| b.pushed_at.cmp(&a.pushed_at))
            .then_with(|| b.tag.cmp(&a.tag))
    });
    let newest = sorted.iter().filter(|t| !aside(t)).count().min(keep);
    let mut kept: BTreeSet<&str> = protected.iter().map(String::as_str).collect();
    for t in sorted.iter().take(newest) {
        kept.insert(&t.digest);
    }
    let mut seen = BTreeSet::new();
    sorted
        .iter()
        .skip(newest)
        .filter(|t| !kept.contains(t.digest.as_str()))
        .map(|t| t.digest.clone())
        .filter(|d| seen.insert(d.clone()))
        .collect()
}

/// Tags retention puts on deployed digests (`isb-keep-<12 hex>`), so the
/// registry's untagged-manifest collection spares them.
pub const KEEP_TAG: &str = "isb-keep-";

/// A preview's image tag: `pr-<number>-<sha>`.
pub fn is_preview_tag(t: &str) -> bool {
    preview_tag_number(t).is_some()
}

/// The pull request number of a preview tag (`pr-12-abc` -> 12).
pub fn preview_tag_number(t: &str) -> Option<u64> {
    let rest = t.strip_prefix("pr-")?;
    let (n, sha) = rest.split_once('-')?;
    if sha.is_empty() || n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    n.parse().ok()
}

/// The daemon's handle on the registry.
pub struct Registry {
    base: Client,
    info: Info,
    remote: oci::Remote,
    /// `<state>/registry`, for the push index; `None` read-only.
    dir: Option<PathBuf>,
    /// Pushes and the index against garbage collection.
    lock: Mutex<()>,
}

static SHARED: OnceLock<Arc<Registry>> = OnceLock::new();

/// Make `r` the registry [`Registry::shared`] returns (the daemon does this
/// at start, with its own state directory).
pub fn install(r: Arc<Registry>) {
    let _ = SHARED.set(r);
}

impl Registry {
    /// The registry as set up on this host, or `None` when it is not.
    /// `state` is the daemon's state directory, for the push index.
    pub fn open(base: &Client, state: Option<&Path>) -> Result<Option<Registry>> {
        let Some(info) = info(base)? else {
            return Ok(None);
        };
        let remote = oci::Remote::new(&info.url(), Some(&info.ca_pem), Duration::from_secs(600))?;
        Ok(Some(Registry {
            base: base.clone().project("default"),
            remote,
            info,
            dir: state.map(|s| s.join("registry")),
            lock: Mutex::new(()),
        }))
    }

    /// The installed registry, else the host's with the default state dir.
    pub fn shared(base: &Client) -> Result<Arc<Registry>> {
        if let Some(r) = SHARED.get() {
            return Ok(r.clone());
        }
        let state = crate::stack::Store::default_dir();
        let state = std::env::var_os("ISB_SERVE_STATE_DIR")
            .map(PathBuf::from)
            .unwrap_or(state);
        Registry::open(base, Some(&state))?
            .map(Arc::new)
            .ok_or_else(not_set_up)
    }

    pub fn info(&self) -> &Info {
        &self.info
    }

    pub fn remote(&self) -> &oci::Remote {
        &self.remote
    }

    fn index_path(&self) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| d.join("pushes.json"))
    }

    fn load_index(&self) -> PushIndex {
        self.index_path()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn save_index(&self, idx: &PushIndex) -> Result<()> {
        let Some(p) = self.index_path() else {
            return Ok(());
        };
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = p.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(idx)?)?;
        std::fs::rename(tmp, p)?;
        Ok(())
    }

    /// Push an OCI layout tar as `<org>/<app>:<tag>`. Returns the digest.
    pub fn push(
        &self,
        org: &OrgId,
        app: &str,
        tag: &str,
        tar: &Path,
        log: &mut dyn FnMut(&str),
    ) -> Result<String> {
        if !valid_app(app) {
            return Err(Error::invalid(format!(
                "app name {app:?}: [a-z0-9][a-z0-9._-]*"
            )));
        }
        if !valid_tag(tag) {
            return Err(Error::invalid(format!(
                "tag {tag:?}: [A-Za-z0-9_][A-Za-z0-9_.-]*"
            )));
        }
        let layout = oci::Layout::open(tar)?;
        let _g = self.lock.lock().unwrap();
        let r = repo(org, app);
        let digest = self.remote.push(&layout, &r, tag, log)?;
        let mut idx = self.load_index();
        idx.repos
            .entry(r)
            .or_default()
            .insert(tag.to_string(), (digest.clone(), crate::stack::now_secs()));
        self.save_index(&idx)?;
        Ok(digest)
    }

    /// The digest an org's `registry:` image names now.
    pub fn resolve(&self, org: &OrgId, r: &ImageRef) -> Result<String> {
        let name = repo(org, &r.app);
        let (reference, sep) = match &r.digest {
            Some(d) => (d.as_str(), '@'),
            None => (r.tag_or_latest(), ':'),
        };
        self.remote.resolve(&name, reference)?.ok_or_else(|| {
            Error::NotFound(format!(
                "image registry:{} in org {org} (no {name}{sep}{reference} in the local registry)",
                r.render()
            ))
        })
    }

    /// Repositories and their tags, of one org or all.
    pub fn list(&self, org: Option<&OrgId>) -> Result<Vec<RepoInfo>> {
        let idx = self.load_index();
        let mut out = Vec::new();
        for name in self.remote.catalog()? {
            let Some((o, app)) = name.split_once('/') else {
                continue;
            };
            if org.is_some_and(|x| x.as_str() != o) {
                continue;
            }
            let mut tags = Vec::new();
            for t in self.remote.tags(&name)? {
                let Some(d) = self.remote.resolve(&name, &t)? else {
                    continue;
                };
                let pushed_at = idx
                    .repos
                    .get(&name)
                    .and_then(|m| m.get(&t))
                    .filter(|(pd, _)| *pd == d)
                    .map(|(_, at)| *at)
                    .unwrap_or(0);
                tags.push(TagInfo {
                    tag: t,
                    digest: d,
                    pushed_at,
                });
            }
            if tags.is_empty() {
                continue;
            }
            tags.sort_by(|a, b| {
                b.pushed_at
                    .cmp(&a.pushed_at)
                    .then_with(|| a.tag.cmp(&b.tag))
            });
            out.push(RepoInfo {
                org: o.to_string(),
                app: app.to_string(),
                repo: name.clone(),
                tags,
            });
        }
        Ok(out)
    }

    /// Delete the manifests of `<org>/<app>` whose every tag `doomed`
    /// picks, except `spare` digests (what is deployed). A digest that
    /// another tag still names stays, and so does that tag: the registry
    /// deletes manifests, not tags. Returns `digest (tags)` of each one
    /// deleted. Blobs go at the next `registry gc`.
    pub fn delete_tags(
        &self,
        org: &OrgId,
        app: &str,
        doomed: &dyn Fn(&str) -> bool,
        spare: &BTreeSet<String>,
    ) -> Result<Vec<String>> {
        let _g = self.lock.lock().unwrap();
        let r = repo(org, app);
        let mut by_digest: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for t in self.remote.tags(&r)? {
            if let Some(d) = self.remote.resolve(&r, &t)? {
                by_digest.entry(d).or_default().push(t);
            }
        }
        let mut out = Vec::new();
        for (d, tags) in &by_digest {
            if spare.contains(d) || !tags.iter().all(|t| doomed(t)) {
                continue;
            }
            self.remote.delete_manifest(&r, d)?;
            out.push(format!("{d} ({})", tags.join(", ")));
        }
        if !out.is_empty() {
            let mut idx = self.load_index();
            if let Some(m) = idx.repos.get_mut(&r) {
                m.retain(|_, (d, _)| !out.iter().any(|x| x.starts_with(d.as_str())));
            }
            self.save_index(&idx)?;
        }
        Ok(out)
    }

    /// Retention: per repository keep the newest `keep` tags and every
    /// `protected` `repo@digest` (what deployed stacks, current and
    /// previous, run), delete the other manifests, then have the registry
    /// collect unreferenced blobs. Pushes wait meanwhile.
    pub fn gc(
        &self,
        keep: usize,
        protected: &BTreeSet<String>,
        dry_run: bool,
        log: &mut dyn FnMut(&str),
    ) -> Result<GcReport> {
        let _g = self.lock.lock().unwrap();
        let mut report = GcReport {
            dry_run,
            ..Default::default()
        };
        // A deployed digest may have lost its tag (the tag moved on): give
        // it one of retention's own, so the untagged-manifest collection
        // below spares it.
        if !dry_run {
            for p in protected {
                let Some((repo, digest)) = p.split_once('@') else {
                    continue;
                };
                let tag = format!("{KEEP_TAG}{}", &digest[7..19.min(digest.len())]);
                if self.remote.resolve(repo, &tag)?.as_deref() == Some(digest) {
                    continue;
                }
                if let Some((mt, bytes)) = self.remote.get_manifest(repo, digest)? {
                    self.remote.put_manifest(repo, &tag, &mt, &bytes)?;
                }
            }
        }
        for r in self.list(None)? {
            let prot: BTreeSet<String> = protected
                .iter()
                .filter_map(|p| p.strip_prefix(&format!("{}@", r.repo)).map(String::from))
                .collect();
            let del = select_deletions(&r.tags, keep, &prot);
            report.kept += r.tags.iter().filter(|t| !del.contains(&t.digest)).count();
            for d in del {
                let tags: Vec<&str> = r
                    .tags
                    .iter()
                    .filter(|t| t.digest == d)
                    .map(|t| t.tag.as_str())
                    .collect();
                let what = format!("{}@{d} ({})", r.repo, tags.join(", "));
                log(&format!(
                    "{}{what}",
                    if dry_run {
                        "would delete "
                    } else {
                        "deleting "
                    }
                ));
                if !dry_run {
                    self.remote.delete_manifest(&r.repo, &d)?;
                }
                report.deleted.push(what);
            }
        }
        if dry_run {
            return Ok(report);
        }
        let mut idx = self.load_index();
        for (repo, tags) in idx.repos.iter_mut() {
            tags.retain(|_, (d, _)| {
                !report
                    .deleted
                    .iter()
                    .any(|x| x.starts_with(&format!("{repo}@{d}")))
            });
        }
        self.save_index(&idx)?;
        // Untagged manifests (left by moved tags) and the blobs nothing
        // references any more. The registry is idle (pushes hold the lock),
        // which garbage-collect requires.
        log("collecting untagged manifests and unreferenced blobs");
        let s = sys(&self.base);
        let sb = crate::sandbox::Sandbox::get(&s, INSTANCE)?;
        let out = sb
            .exec_stream(
                [
                    "/bin/registry",
                    "garbage-collect",
                    "--delete-untagged",
                    "/etc/docker/registry/config.yml",
                ],
                crate::exec::ExecOptions::default()
                    .env(
                        "REGISTRY_STORAGE_FILESYSTEM_ROOTDIRECTORY",
                        "/var/lib/registry",
                    )
                    .env("REGISTRY_STORAGE_DELETE_ENABLED", "true")
                    .timeout(Duration::from_secs(1800)),
            )?
            .collect_output()?;
        let text = format!("{}{}", out.stdout_text(), out.stderr_text());
        let tail: Vec<&str> = text.lines().rev().take(20).collect();
        report.collect = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
        if !out.success() {
            return Err(Error::invalid(format!(
                "registry garbage-collect failed ({}): {}",
                out.exit_code, report.collect
            )));
        }
        // The registry caches blob descriptors in memory: restart it so a
        // collected blob is not reported as present to the next push.
        s.mutate(
            "PUT",
            &format!("/1.0/instances/{INSTANCE}/state"),
            Some(&json!({"action": "restart", "timeout": 30, "force": true})),
            &format!("restart {INSTANCE}"),
            s.get_timeouts().other,
        )?;
        wait_up(&self.info, Duration::from_secs(60))?;
        Ok(report)
    }
}

fn not_set_up() -> Error {
    Error::invalid(
        "no local registry on this host: run `isb registry setup`, then `sudo isb host setup`",
    )
}

/// The registry `base` can reach, for resolving `registry:` images outside
/// the daemon (CLI deploys, plans).
pub fn require_info(base: &Client) -> Result<Info> {
    info(base)?.ok_or_else(not_set_up)
}

/// What deployed stacks reference, as `repo@digest`: retention never takes
/// these.
pub fn protected_by(defs: &[Arc<crate::stack::StackDef>]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for d in defs {
        let mut cur: Option<&crate::stack::StackDef> = Some(d);
        while let Some(def) = cur {
            for (svc, spec) in &def.file.services {
                let Some(r) = spec.image.strip_prefix("registry:") else {
                    continue;
                };
                let Ok(r) = ImageRef::parse(r) else { continue };
                let digest = r.digest.clone().or_else(|| def.images.get(svc).cloned());
                if let Some(dg) = digest {
                    out.insert(format!("{}@{dg}", repo(&def.org, &r.app)));
                }
            }
            cur = def.previous.as_deref();
        }
    }
    out
}

/// The registry's status for `isb registry ls` and tools.
pub fn status_json(r: &Registry) -> Value {
    json!({"addr": r.info.addr, "url": r.info.url()})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(n: u8) -> String {
        oci::digest_of(&[n])
    }

    #[test]
    fn refs_parse_and_never_name_another_org() {
        let r = ImageRef::parse("web:v1").unwrap();
        assert_eq!((r.app.as_str(), r.tag.as_deref()), ("web", Some("v1")));
        let r = ImageRef::parse("web").unwrap();
        assert_eq!(r.tag_or_latest(), "latest");
        let dg = d(1);
        let r = ImageRef::parse(&format!("web:v2@{dg}")).unwrap();
        assert_eq!(r.digest.as_deref(), Some(dg.as_str()));
        assert_eq!(r.render(), format!("web:v2@{dg}"));
        let org = OrgId::new("acme").unwrap();
        assert_eq!(r.pull_alias(&org), format!("acme/web@{dg}"));
        assert_eq!(
            ImageRef::parse("web:v1").unwrap().pull_alias(&org),
            "acme/web:v1"
        );
        assert_eq!(
            ImageRef::parse("web:v1").unwrap().pinned(&dg).render(),
            format!("web:v1@{dg}")
        );
        for bad in [
            "other/web:v1",
            "../web",
            "Web",
            "web:bad tag",
            "web@sha256:abc",
            "",
            "web:",
            ":v1",
            "web:-x",
        ] {
            assert!(ImageRef::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn retention_keeps_newest_and_deployed() {
        let t = |tag: &str, n: u8, at: u64| TagInfo {
            tag: tag.into(),
            digest: d(n),
            pushed_at: at,
        };
        let tags = vec![
            t("v1", 1, 100),
            t("v2", 2, 200),
            t("v3", 3, 300),
            t("v4", 4, 400),
            // Same image as v4 under another, older tag: v4 keeps it.
            t("old", 4, 50),
            t("unknown", 5, 0),
        ];
        let none = BTreeSet::new();
        let del = select_deletions(&tags, 2, &none);
        assert_eq!(del, vec![d(2), d(1), d(5)]);
        // A deployed digest survives however old.
        let prot: BTreeSet<String> = [d(1)].into();
        assert_eq!(select_deletions(&tags, 2, &prot), vec![d(2), d(5)]);
        assert!(select_deletions(&tags, 10, &none).is_empty());
        assert_eq!(select_deletions(&tags, 0, &none).len(), 5);
        // Retention's own tags are not among the newest, and go once what
        // they kept is no longer deployed.
        let mut with_keep = tags.clone();
        with_keep.push(t(&format!("{KEEP_TAG}000000000001"), 1, 999));
        assert_eq!(select_deletions(&with_keep, 2, &prot), vec![d(2), d(5)]);
        assert_eq!(
            select_deletions(&with_keep, 2, &none),
            vec![d(2), d(1), d(5)]
        );
        // Preview tags are never among the newest: kept while deployed,
        // deleted once not.
        let mut with_pr = tags.clone();
        with_pr.push(t("pr-7-abc", 8, 9999));
        with_pr.push(t("pr-7-def", 9, 9998));
        let prot9: BTreeSet<String> = [d(9)].into();
        assert_eq!(
            select_deletions(&with_pr, 2, &prot9),
            vec![d(2), d(1), d(5), d(8)]
        );
        assert!(is_preview_tag("pr-12-0123abc"));
        assert_eq!(preview_tag_number("pr-12-0123abc"), Some(12));
        for t in ["pr-x-abc", "pr-12", "pr--a", "v1", "pr-12-"] {
            assert!(!is_preview_tag(t), "{t}");
        }
    }

    #[test]
    fn deployed_images_are_protected() {
        let dg = d(7);
        let file: crate::spec::ComposeFile = serde_yaml_ng::from_str(&format!(
            "services:\n  web: {{image: 'registry:web:v2'}}\n  api: {{image: 'registry:api@{dg}'}}\n  db: {{image: 'docker:postgres'}}\n"
        ))
        .unwrap();
        let mut def = crate::stack::StackDef {
            name: "s".into(),
            org: OrgId::new("acme").unwrap(),
            file: file.clone(),
            base_dir: "/".into(),
            secrets: Default::default(),
            force: Default::default(),
            images: [("web".to_string(), d(2))].into(),
            deployed_at: 0,
            deployed_by: String::new(),
            previous: None,
        };
        let mut prev = def.clone();
        prev.images = [("web".to_string(), d(1))].into();
        def.previous = Some(Box::new(prev));
        let p = protected_by(&[Arc::new(def)]);
        assert!(p.contains(&format!("acme/web@{}", d(2))));
        assert!(
            p.contains(&format!("acme/web@{}", d(1))),
            "the previous deployment"
        );
        assert!(p.contains(&format!("acme/api@{dg}")));
        assert_eq!(p.len(), 3);
    }

    #[test]
    fn gc_against_a_fake_registry() {
        let (base, st) = oci::tests::fake();
        let remote = oci::Remote::new(&base, None, Duration::from_secs(10)).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry {
            base: Client::with_socket("/nonexistent"),
            info: Info {
                addr: base.trim_start_matches("http://").into(),
                ca_pem: String::new(),
            },
            remote,
            dir: Some(dir.path().join("registry")),
            lock: Mutex::new(()),
        };
        let org = OrgId::new("acme").unwrap();
        let mut digests = Vec::new();
        for i in 0..3u8 {
            let (t, _) = oci::tests::image_tar(&[i; 10]);
            let p = dir.path().join(format!("{i}.tar"));
            std::fs::write(&p, t).unwrap();
            digests.push(
                reg.push(&org, "web", &format!("v{i}"), &p, &mut |_| {})
                    .unwrap(),
            );
            // Distinct push times, newest last.
            let mut idx = reg.load_index();
            idx.repos
                .get_mut("acme/web")
                .unwrap()
                .get_mut(&format!("v{i}"))
                .unwrap()
                .1 = 1000 + i as u64;
            reg.save_index(&idx).unwrap();
        }
        let ls = reg.list(Some(&org)).unwrap();
        assert_eq!(
            ls[0]
                .tags
                .iter()
                .map(|t| t.tag.as_str())
                .collect::<Vec<_>>(),
            ["v2", "v1", "v0"]
        );
        assert!(
            reg.list(Some(&OrgId::new("other").unwrap()))
                .unwrap()
                .is_empty()
        );
        let r = reg
            .resolve(&org, &ImageRef::parse("web:v1").unwrap())
            .unwrap();
        assert_eq!(r, digests[1]);
        assert!(
            reg.resolve(
                &OrgId::new("other").unwrap(),
                &ImageRef::parse("web:v1").unwrap()
            )
            .is_err()
        );
        // Dry: keep 1, and v0 is deployed.
        let prot: BTreeSet<String> = [format!("acme/web@{}", digests[0])].into();
        let rep = reg.gc(1, &prot, true, &mut |_| {}).unwrap();
        assert_eq!(rep.deleted.len(), 1);
        assert!(rep.deleted[0].contains(&digests[1]) && rep.deleted[0].contains("(v1)"));
        assert!(
            st.lock()
                .unwrap()
                .log
                .iter()
                .all(|l| !l.starts_with("DELETE"))
        );
    }
}
