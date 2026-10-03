//! Resolve a spec into desired incus state, and diff it against actual state.
//!
//! Both halves are pure (host facts and actual state are passed in), which is what
//! makes the reconcile rules unit-testable. The rules that matter most:
//!
//! - A device that is already correct is never touched. Re-adding a disk device
//!   remounts it, which silently kills inotify watches a live dev server holds
//!   (Vite keeps answering 200 while HMR goes quiet).
//! - Device names are deterministic, from the spec, never random.
//! - isb never removes config keys or devices it was not told about, unless asked
//!   to prune devices. Another tool (or another spec for the same instance) may
//!   own them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::flex::parse_duration;
use crate::idmap::{self, SubIds};
use crate::spec::{
    ExecDefaults, InstanceType, MountType, PortBind, ReadyCheck, RestartMode, SandboxSpec,
};

pub type Props = BTreeMap<String, String>;

/// Facts about the host that resolution depends on.
#[derive(Debug, Clone, Default)]
pub struct HostFacts {
    pub subids: SubIds,
    /// Storage pools that exist, in server order.
    pub pools: Vec<String>,
    /// Rewrite bind sources starting with `.0` to start with `.1` instead: the
    /// path incusd resolves can differ from the one this process sees (see
    /// [`HostFacts::detect_path_map`]).
    pub path_map: Option<(String, String)>,
    /// The server can seed a new volume from the image (`disk_initial_copy`).
    pub initial_copy: bool,
    /// The only directory incusd can see bind sources under, when it runs
    /// elsewhere: on macOS, the home directory shared with the `isb machine`.
    pub shared_root: Option<String>,
    /// The org the sandbox goes in (from the client's incus project):
    /// where `registry:` images resolve. `None` outside isb's projects.
    pub org: Option<crate::org::OrgId>,
    /// The local registry's `host:port`, when one is set up.
    pub registry: Option<String>,
}

impl HostFacts {
    /// Where bind sources must be translated before incusd sees them.
    ///
    /// `ISB_HOST_PATH_MAP=from=to` sets it explicitly. Otherwise, inside an
    /// agent-workspace box, `$HOME` is a bind mount of a directory on the box's own
    /// host and incusd resolves disk sources in its own mount view, where only the
    /// host-side path exists; the box publishes that path in
    /// `/etc/workspace/guest-home`. Without the rewrite, adding the device fails
    /// with "Missing source path" for a directory `ls` lists happily.
    pub fn detect_path_map() -> Option<(String, String)> {
        if let Ok(m) = std::env::var("ISB_HOST_PATH_MAP") {
            if let Some((a, b)) = m.split_once('=') {
                if !a.is_empty() && !b.is_empty() {
                    return Some((
                        a.trim_end_matches('/').into(),
                        b.trim_end_matches('/').into(),
                    ));
                }
            }
            return None;
        }
        let gh = std::fs::read_to_string("/etc/workspace/guest-home").ok()?;
        let gh = gh.trim().trim_end_matches('/');
        let home = std::env::var("HOME").ok()?;
        let home = home.trim_end_matches('/');
        if gh.is_empty() || home.is_empty() {
            return None;
        }
        Some((home.to_string(), gh.to_string()))
    }

    pub fn translate(&self, path: &str) -> String {
        if let Some((from, to)) = &self.path_map {
            if let Some(rest) = path.strip_prefix(from.as_str()) {
                if rest.is_empty() || rest.starts_with('/') {
                    return format!("{to}{rest}");
                }
            }
        }
        path.to_string()
    }

    /// `auto`: `incus-zfs`, else `default`, else the first pool.
    pub fn pick_pool(&self, requested: Option<&str>) -> Result<String> {
        match requested {
            Some(p) if p != "auto" && !p.is_empty() => {
                if self.pools.is_empty() || self.pools.iter().any(|x| x == p) {
                    Ok(p.to_string())
                } else {
                    Err(Error::invalid(format!(
                        "storage pool {p:?} does not exist (have: {})",
                        self.pools.join(", ")
                    )))
                }
            }
            _ => ["incus-zfs", "default"]
                .iter()
                .find(|c| self.pools.iter().any(|p| p == *c))
                .map(|s| s.to_string())
                .or_else(|| self.pools.first().cloned())
                .ok_or_else(|| Error::invalid("no storage pools exist")),
        }
    }
}

/// A named volume that must exist before the instance.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EnsureVolume {
    pub pool: String,
    pub name: String,
    pub config: Props,
    pub external: bool,
}

/// chown a mount point (and root-owned parents inside the owner's home).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OwnerFixup {
    pub device: String,
    pub path: String,
    pub owner: String,
}

/// A desired device.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DesiredDevice {
    pub props: Props,
    /// Host-bound proxy that may move up to this many ports on conflict.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search: Option<u16>,
}

/// Where the image comes from.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ImageSource {
    /// The spec string, for messages.
    pub spec: String,
    /// `None` for a local image.
    pub server: Option<String>,
    pub protocol: Option<String>,
    pub alias: String,
    /// A `registry:` image, not yet bound to an org and the local registry
    /// ([`ImageSource::bind`]); `alias` is then `APP[:TAG][@DIGEST]`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub local_registry: bool,
}

/// Whether a registry host (`host[:port]`) is this machine's loopback,
/// where only the local registry listens.
fn is_loopback_host(hostport: &str) -> bool {
    let host = if let Some(rest) = hostport.strip_prefix('[') {
        rest.split(']').next().unwrap_or(rest)
    } else {
        hostport
            .rsplit_once(':')
            .map(|(h, _)| h)
            .unwrap_or(hostport)
    };
    let host = host.to_ascii_lowercase();
    host == "localhost"
        || host.ends_with(".localhost")
        || host == "0.0.0.0"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback() || ip.is_unspecified())
}

/// OCI registries known by a short prefix: `docker:nginx:1.27`.
const OCI_REGISTRIES: &[(&str, &str)] = &[
    ("docker", "https://docker.io"),
    ("ghcr", "https://ghcr.io"),
    ("quay", "https://quay.io"),
];

impl ImageSource {
    pub fn parse(s: &str) -> Result<Self> {
        if s.is_empty() {
            return Err(Error::invalid("image is required"));
        }
        if let Some((remote, alias)) = s.split_once(':') {
            if let Some((_, server)) = OCI_REGISTRIES.iter().find(|(k, _)| *k == remote) {
                return Ok(ImageSource {
                    spec: s.into(),
                    server: Some(server.to_string()),
                    protocol: Some("oci".into()),
                    alias: oci_reference(alias, remote == "docker")?,
                    local_registry: false,
                });
            }
            if remote == "registry" {
                // registry:app:tag, the org's own image in the local registry.
                let r = crate::registry::ImageRef::parse(alias)?;
                return Ok(ImageSource {
                    spec: s.into(),
                    server: None,
                    protocol: Some("oci".into()),
                    alias: r.render(),
                    local_registry: true,
                });
            }
            if remote == "oci" {
                // oci:registry.example.com/team/app:tag
                let (host, path) = alias.split_once('/').ok_or_else(|| {
                    Error::invalid(format!("{s:?}: an oci: image is oci:REGISTRY/PATH[:TAG]"))
                })?;
                // The local registry is on loopback and holds every org's
                // images: it is reached only as `registry:`, which stays in
                // the org.
                if is_loopback_host(host) {
                    return Err(Error::invalid(format!(
                        "{s:?}: a loopback registry is the local one; name its images as registry:APP:TAG"
                    )));
                }
                return Ok(ImageSource {
                    spec: s.into(),
                    server: Some(format!("https://{host}")),
                    protocol: Some("oci".into()),
                    alias: oci_reference(path, false)?,
                    local_registry: false,
                });
            }
            let (server, protocol) = match remote {
                "images" => ("https://images.linuxcontainers.org", "simplestreams"),
                "ubuntu" => ("https://cloud-images.ubuntu.com/releases", "simplestreams"),
                "ubuntu-daily" => ("https://cloud-images.ubuntu.com/daily", "simplestreams"),
                "ubuntu-minimal" => (
                    "https://cloud-images.ubuntu.com/minimal/releases",
                    "simplestreams",
                ),
                other => {
                    return Err(Error::invalid(format!(
                        "unknown image remote {other:?} in {s:?} (known: images, ubuntu, ubuntu-daily, ubuntu-minimal, and OCI registries docker, ghcr, quay, oci:REGISTRY/...; local images need no prefix)"
                    )));
                }
            };
            return Ok(ImageSource {
                spec: s.into(),
                server: Some(server.into()),
                protocol: Some(protocol.into()),
                alias: alias.into(),
                local_registry: false,
            });
        }
        Ok(ImageSource {
            spec: s.into(),
            server: None,
            protocol: None,
            alias: s.into(),
            local_registry: false,
        })
    }

    /// Bind a `registry:` image to `org`'s repository in the local registry
    /// at `addr`. Anything else is returned as is.
    pub fn bind(mut self, org: Option<&crate::org::OrgId>, addr: Option<&str>) -> Result<Self> {
        if !self.local_registry {
            return Ok(self);
        }
        let org = org.ok_or_else(|| {
            Error::invalid(format!(
                "{:?}: registry: images belong to an org; this project is not one",
                self.spec
            ))
        })?;
        let addr = addr.ok_or_else(|| {
            Error::invalid(format!(
                "{:?}: no local registry on this host (isb registry setup)",
                self.spec
            ))
        })?;
        let r = crate::registry::ImageRef::parse(&self.alias)?;
        self.alias = r.pull_alias(org);
        self.server = Some(format!("https://{addr}"));
        self.local_registry = false;
        Ok(self)
    }

    /// An OCI (docker) image: an application container whose process is the
    /// instance's init.
    pub fn is_oci(&self) -> bool {
        self.protocol.as_deref() == Some("oci")
    }

    /// The `source` object for `POST /1.0/instances`. A local image is given by
    /// fingerprint once resolved, by alias otherwise.
    pub fn to_api(&self, local_fingerprint: Option<&str>) -> Value {
        match (&self.server, local_fingerprint) {
            (Some(server), _) => json!({
                "type": "image", "mode": "pull", "server": server,
                "protocol": self.protocol, "alias": self.alias,
            }),
            (None, Some(fp)) => json!({"type": "image", "fingerprint": fp}),
            (None, None) => json!({"type": "image", "alias": self.alias}),
        }
    }
}

/// `nginx` -> `library/nginx:latest` on Docker Hub, as docker spells it.
fn oci_reference(r: &str, docker_hub: bool) -> Result<String> {
    if r.is_empty() || r.contains(char::is_whitespace) {
        return Err(Error::invalid(format!("invalid OCI image reference {r:?}")));
    }
    let mut r = r.to_string();
    if docker_hub && !r.contains('/') {
        r = format!("library/{r}");
    }
    let last = r.rsplit('/').next().unwrap_or(&r);
    if !last.contains(':') && !last.contains('@') {
        r.push_str(":latest");
    }
    Ok(r)
}

/// Quote argv for `oci.entrypoint`, which incus splits on whitespace with
/// quotes grouping. There is no escape character, so an argument may not
/// contain both kinds of quote.
pub fn oci_command_line(argv: &[String]) -> std::result::Result<String, String> {
    argv.iter()
        .map(|a| {
            if !a.is_empty() && !a.contains(|c: char| c.is_whitespace() || c == '"' || c == '\'') {
                Ok(a.clone())
            } else if !a.contains('"') {
                Ok(format!("\"{a}\""))
            } else if !a.contains('\'') {
                Ok(format!("'{a}'"))
            } else {
                Err(format!(
                    "argument {a:?} has both ' and \" in it, which an OCI command line cannot carry; use a script"
                ))
            }
        })
        .collect::<std::result::Result<Vec<_>, _>>()
        .map(|v| v.join(" "))
}

/// A spec resolved against the host: exactly what incus should hold.
#[derive(Debug, Clone, Serialize)]
pub struct Desired {
    pub name: String,
    pub instance_type: InstanceType,
    pub image: ImageSource,
    pub pool: String,
    pub profiles: Vec<String>,
    pub config: Props,
    /// Includes the `root` disk (create-only; never reconciled).
    pub devices: BTreeMap<String, DesiredDevice>,
    pub volumes: Vec<EnsureVolume>,
    pub owners: Vec<OwnerFixup>,
    pub ready: Vec<ReadyCheck>,
    #[serde(skip)]
    pub ready_timeout: Duration,
    pub exec: ExecDefaults,
    /// The idmap mode when the spec set one (not for `{raw: ...}`): an absent
    /// `raw.idmap` is then a decision, not an omission.
    #[serde(skip)]
    pub idmap_mode: Option<crate::spec::IdmapMode>,
    /// Config keys holding secret values (`environment.KEY` from
    /// `{secret: NAME}`): set, but never shown in plans or reports.
    #[serde(skip)]
    pub sensitive: BTreeSet<String>,
}

/// Named-volume definitions available to a sandbox (from a compose file's
/// top-level `volumes:`).
pub type VolumeDefs = BTreeMap<String, crate::spec::NamedVolumeSpec>;

/// Valid incus instance name: <= 63 chars, [a-zA-Z0-9-], starts with a letter,
/// does not end with '-'.
pub fn validate_instance_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 63
        && name.starts_with(|c: char| c.is_ascii_alphabetic())
        && !name.ends_with('-')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "invalid sandbox name {name:?}: use at most 63 of [a-z0-9-], starting with a letter"
        )))
    }
}

/// Deterministic device name for a guest mount path.
pub fn device_name_for_path(guest: &str) -> String {
    let mut s = String::new();
    for c in guest.to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c);
        } else if !s.ends_with('-') {
            s.push('-');
        }
    }
    let s = s.trim_matches('-').to_string();
    let s = if s.is_empty() { "mount".to_string() } else { s };
    if s.len() <= 48 {
        return s;
    }
    format!(
        "{}-{:08x}",
        s[s.len() - 39..].trim_start_matches('-'),
        fnv32(guest)
    )
}

fn fnv32(s: &str) -> u32 {
    let mut h: u32 = 0x811c9dc5;
    for b in s.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x01000193);
    }
    h
}

/// Split `tcp:1.2.3.4:5173` into ("tcp", "1.2.3.4", 5173). IPv6 in brackets works.
pub fn split_addr(addr: &str) -> Option<(&str, &str, u16)> {
    let (proto, rest) = addr.split_once(':')?;
    if !matches!(proto, "tcp" | "udp") {
        return None;
    }
    let (host, port) = rest.rsplit_once(':')?;
    Some((proto, host, port.parse().ok()?))
}

/// Expand a proxy address to incus' `proto:host:port` form.
///
/// Accepts `5173`, `HOST:5173`, `5173/udp`, `tcp:5173`, and full
/// `tcp:HOST:PORT` / `udp:HOST:PORT` / `unix:PATH`. The protocol defaults to
/// tcp and the host to `default_host`. IPv6 hosts go in brackets
/// (`[::1]:5173`). The port may be a range or list as incus allows
/// (`8000-8010`, `80,443`).
pub fn normalize_addr(addr: &str, default_host: &str) -> std::result::Result<String, String> {
    let a = addr.trim();
    if a.is_empty() {
        return Err("empty address".into());
    }
    if let Some(path) = a.strip_prefix("unix:") {
        if path.is_empty() {
            return Err(format!("{addr:?}: unix: needs a path"));
        }
        return Ok(a.to_string());
    }
    let (proto, rest) = match a.split_once(':') {
        Some((p @ ("tcp" | "udp"), rest)) => (p, rest),
        _ => match a.rsplit_once('/') {
            Some((rest, p @ ("tcp" | "udp"))) => (p, rest),
            Some((_, other)) if !other.contains(':') => {
                return Err(format!("{addr:?}: unknown protocol {other:?} (tcp or udp)"));
            }
            _ => ("tcp", a),
        },
    };
    let (host, port) = if rest.starts_with('[') {
        let end = rest
            .find(']')
            .ok_or_else(|| format!("{addr:?}: unclosed [ in IPv6 host"))?;
        let port = rest[end + 1..]
            .strip_prefix(':')
            .ok_or_else(|| format!("{addr:?}: expected [IPv6]:PORT"))?;
        (&rest[..=end], port)
    } else {
        match rest.rsplit_once(':') {
            Some((h, _)) if h.contains(':') => {
                return Err(format!(
                    "{addr:?}: put an IPv6 host in brackets, e.g. [::1]:5173"
                ));
            }
            Some((h, p)) => (h, p),
            None => (default_host, rest),
        }
    };
    if host.is_empty() {
        return Err(format!("{addr:?}: empty host"));
    }
    let valid_port = !port.is_empty()
        && port.split(',').all(|part| {
            let mut ends = part.splitn(2, '-');
            ends.all(|n| n.parse::<u16>().is_ok_and(|n| n > 0))
        });
    if !valid_port {
        return Err(format!(
            "{addr:?}: expected PORT, HOST:PORT or PROTO:HOST:PORT (e.g. 5173, 0.0.0.0:5173, udp:5353)"
        ));
    }
    Ok(format!("{proto}:{host}:{port}"))
}

fn default_port_name(bind: PortBind, listen: &str) -> String {
    match split_addr(listen) {
        Some(("tcp", _, port)) => format!("port-{}-{port}", bind.as_str()),
        Some((proto, _, port)) => format!("port-{}-{proto}-{port}", bind.as_str()),
        None => format!("port-{}-{}", bind.as_str(), device_name_for_path(listen)),
    }
}

fn expand_home(p: &str) -> String {
    if p == "~" || p.starts_with("~/") {
        if let Ok(h) = std::env::var("HOME") {
            return format!("{}{}", h.trim_end_matches('/'), &p[1..]);
        }
    }
    p.to_string()
}

/// Make a bind source absolute (against `base`) and resolve symlinks, so the
/// device source does not depend on how the caller reached the directory.
pub fn resolve_host_path(p: &str, base: &Path) -> Result<String> {
    let expanded = expand_home(p);
    let path = PathBuf::from(&expanded);
    let abs = if path.is_absolute() {
        path
    } else {
        base.join(path)
    };
    let canon = abs.canonicalize().map_err(|e| {
        Error::invalid(format!("bind source {} does not exist: {e}", abs.display()))
    })?;
    Ok(canon.to_string_lossy().into_owned())
}

/// Translate a docker memory size (`512m`, `8g`, `1073741824`) to
/// incus' units (`512MiB`, `8GiB`, bytes). Docker's units are binary, so `g`
/// and `GB` are GiB. incus' binary units (`8GiB`) and `50%` pass through.
pub fn memory_limit(m: &str) -> std::result::Result<String, String> {
    let t = m.trim();
    let split = t
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(t.len());
    let (num, unit) = (&t[..split], t[split..].trim());
    if num.is_empty() || num.parse::<u64>().is_err() {
        // incus parses sizes as integers, so 1.5g has to be written 1536m.
        return Err(format!("{m:?} is not a whole size (e.g. 512m, 8g, 8GiB)"));
    }
    let suffix = match unit.to_ascii_lowercase().as_str() {
        "" | "b" => "",
        "k" | "kb" => "KiB",
        "m" | "mb" => "MiB",
        "g" | "gb" => "GiB",
        "t" | "tb" => "TiB",
        // incus' own binary spellings, and percentages.
        "%" | "kib" | "mib" | "gib" | "tib" => return Ok(t.to_string()),
        _ => {
            return Err(format!(
                "{m:?}: unknown unit {unit:?} (b, k, m, g, t, KiB, MiB, GiB, TiB or %)"
            ));
        }
    };
    Ok(format!("{num}{suffix}"))
}

/// Resolve a spec. `base` anchors relative bind paths.
#[expect(
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    reason = "predates the lint ratchet; split it when next changed"
)]
pub fn resolve(
    spec: &SandboxSpec,
    defs: &VolumeDefs,
    host: &HostFacts,
    base: &Path,
) -> Result<Desired> {
    let name = spec
        .name
        .clone()
        .ok_or_else(|| Error::invalid("sandbox name is required"))?;
    validate_instance_name(&name)?;
    let image = ImageSource::parse(&spec.image)
        .and_then(|i| i.bind(host.org.as_ref(), host.registry.as_deref()))
        .map_err(|e| Error::invalid(format!("{name}: {e}")))?;
    let pool = host.pick_pool(spec.storage.as_deref())?;
    let vm = spec.instance_type == InstanceType::VirtualMachine;
    let oci = image.is_oci();
    if oci && vm {
        return Err(Error::invalid(format!(
            "{name}: OCI images run as containers, not VMs"
        )));
    }
    if spec.entrypoint.is_some() && !oci {
        return Err(Error::invalid(format!(
            "{name}: entrypoint is for OCI images; use command"
        )));
    }
    if vm {
        if spec.privileged.is_some() {
            return Err(Error::invalid(format!(
                "{name}: privileged is container-only"
            )));
        }
        if let Some(i) = &spec.idmap {
            if !matches!(
                i,
                crate::spec::IdmapSpec::Mode(
                    crate::spec::IdmapMode::Auto | crate::spec::IdmapMode::None
                )
            ) {
                return Err(Error::invalid(format!(
                    "{name}: idmap is container-only (VM shares go over virtiofs)"
                )));
            }
        }
        for p in &spec.ports {
            if p.bind == PortBind::Guest {
                return Err(Error::invalid(format!(
                    "{name}: incus VMs only support bind: host proxies (in NAT mode)"
                )));
            }
        }
    }

    let mut config = Props::new();
    match (&spec.cpus, &spec.cpuset) {
        (Some(_), Some(_)) => {
            return Err(Error::invalid(format!(
                "{name}: set cpus (a count) or cpuset (which CPUs), not both"
            )));
        }
        (Some(c), None) => {
            if !c.trim().parse::<u32>().is_ok_and(|n| n > 0) {
                return Err(Error::invalid(format!(
                    "{name}: cpus is a whole number of CPUs, got {c:?} (pin CPUs with cpuset: \"0-3\")"
                )));
            }
            config.insert("limits.cpu".into(), c.trim().to_string());
        }
        (None, Some(set)) => {
            config.insert("limits.cpu".into(), set.clone());
        }
        (None, None) => {}
    }
    if let Some(m) = &spec.memory {
        let m = memory_limit(m).map_err(|e| Error::invalid(format!("{name}: mem_limit: {e}")))?;
        config.insert("limits.memory".into(), m);
    }
    if let Some(p) = spec.privileged {
        config.insert("security.privileged".into(), p.to_string());
    }
    let mut idmap_mode = None;
    if let Some(i) = spec.idmap.as_ref().filter(|_| !vm) {
        idmap_mode = match i {
            crate::spec::IdmapSpec::Mode(m) => Some(*m),
            crate::spec::IdmapSpec::Map(m) => Some(m.mode),
            crate::spec::IdmapSpec::Raw(_) => None,
        };
        if let Some(v) = idmap::resolve(i, &host.subids) {
            config.insert("raw.idmap".into(), v);
        }
    }
    for (k, v) in &spec.labels {
        if k.is_empty() || k.contains(char::is_whitespace) {
            return Err(Error::invalid(format!("{name}: invalid label key {k:?}")));
        }
        config.insert(format!("user.{k}"), v.clone());
    }
    for (k, v) in &spec.env {
        config.insert(format!("environment.{k}"), v.clone());
    }
    if let Some(r) = spec.restart {
        // incus' default (no boot.autostart) already restores the state the
        // instance had at shutdown, which is exactly unless-stopped.
        if matches!(r, RestartMode::Always | RestartMode::OnFailure) {
            config.insert("boot.autostart".into(), "true".into());
        }
        if r.is_long_running() {
            config.insert("boot.autorestart".into(), "true".into());
        }
    }
    if oci {
        let mut line: Vec<String> = spec.entrypoint.clone().unwrap_or_default();
        line.extend(spec.command.clone().unwrap_or_default());
        if !line.is_empty() {
            let l = oci_command_line(&line).map_err(|e| Error::invalid(format!("{name}: {e}")))?;
            config.insert("oci.entrypoint".into(), l);
        }
        if let Some(w) = &spec.working_dir {
            config.insert("oci.cwd".into(), w.clone());
        }
        if let Some(u) = &spec.user {
            let (uid, gid) = u.split_once(':').unwrap_or((u, u));
            if uid.parse::<u32>().is_err() || gid.parse::<u32>().is_err() {
                return Err(Error::invalid(format!(
                    "{name}: an OCI image's user must be numeric (uid or uid:gid), got {u:?}"
                )));
            }
            config.insert("oci.uid".into(), uid.into());
            config.insert("oci.gid".into(), gid.into());
        }
    }
    for (k, v) in &spec.raw_config {
        config.insert(k.clone(), v.clone());
    }

    let mut devices: BTreeMap<String, DesiredDevice> = BTreeMap::new();
    let mut add_dev = |dname: String, dev: DesiredDevice| -> Result<()> {
        if devices.insert(dname.clone(), dev).is_some() {
            return Err(Error::invalid(format!(
                "{name}: device name {dname:?} is used twice"
            )));
        }
        Ok(())
    };
    add_dev(
        "root".into(),
        DesiredDevice {
            props: Props::from([
                ("type".into(), "disk".into()),
                ("path".into(), "/".into()),
                ("pool".into(), pool.clone()),
            ]),
            search: None,
        },
    )?;

    let mut volumes: Vec<EnsureVolume> = Vec::new();
    let mut owners = Vec::new();
    let mut guest_paths = BTreeSet::new();
    for v in &spec.volumes {
        let guest = &v.target;
        if !guest.starts_with('/') {
            return Err(Error::invalid(format!(
                "{name}: mount path {guest:?} must be absolute"
            )));
        }
        let guest_norm = guest.trim_end_matches('/').to_string();
        let guest_norm = if guest_norm.is_empty() {
            "/".to_string()
        } else {
            guest_norm
        };
        if !guest_paths.insert(guest_norm.clone()) {
            return Err(Error::invalid(format!("{name}: {guest} is mounted twice")));
        }
        let dname = v
            .device
            .clone()
            .unwrap_or_else(|| device_name_for_path(&guest_norm));
        let mut props = Props::from([
            ("type".into(), "disk".into()),
            ("path".into(), guest_norm.clone()),
        ]);
        match v.mount_type {
            MountType::Bind => {
                if v.owner.is_some() {
                    return Err(Error::invalid(format!(
                        "{name}: {guest}: owner is only for named volumes (isb never chowns host paths)"
                    )));
                }
                if v.pool.is_some() || v.external || v.volume.nocopy {
                    return Err(Error::invalid(format!(
                        "{name}: {guest}: pool, external and nocopy are only for named volumes"
                    )));
                }
                let src = resolve_host_path(&v.source, base)?;
                if let Some(root) = &host.shared_root {
                    let r = root.trim_end_matches('/');
                    if src != r && !src.starts_with(&format!("{r}/")) {
                        return Err(Error::invalid(format!(
                            "{name}: {guest}: bind source {src} is outside {r}, the only \
                             directory shared with the isb machine"
                        )));
                    }
                }
                props.insert("source".into(), host.translate(&src));
            }
            MountType::Volume => {
                let def = defs.get(&v.source);
                // A compose file names its volumes `<project>_<key>`; a bare
                // spec names the incus volume directly.
                let n = &def
                    .and_then(|d| d.name.clone())
                    .unwrap_or_else(|| v.source.clone());
                let vpool = match v.pool.as_deref().or(def.and_then(|d| d.pool.as_deref())) {
                    Some(p) if p != "auto" => host.pick_pool(Some(p))?,
                    _ => pool.clone(),
                };
                props.insert("pool".into(), vpool.clone());
                props.insert("source".into(), n.clone());
                // docker seeds an empty named volume with the image's content at
                // the target. incus does the same with initial.copy, containers
                // only; an older server just mounts it empty, as isb always did.
                if !vm && host.initial_copy && !v.volume.nocopy {
                    props.insert("initial.copy".into(), "true".into());
                }
                let ev = EnsureVolume {
                    pool: vpool,
                    name: n.clone(),
                    config: def.map(|d| d.config.clone()).unwrap_or_default(),
                    external: v.external || def.is_some_and(|d| d.external),
                };
                if !volumes.contains(&ev) {
                    volumes.push(ev);
                }
                if let Some(o) = &v.owner {
                    owners.push(OwnerFixup {
                        device: dname.clone(),
                        path: guest_norm.clone(),
                        owner: o.clone(),
                    });
                }
            }
        }
        if v.read_only {
            props.insert("readonly".into(), "true".into());
        }
        for k in v.options.keys() {
            if matches!(k.as_str(), "type" | "path" | "source" | "pool" | "readonly") {
                return Err(Error::invalid(format!(
                    "{name}: {guest}: options.{k} would override a core property; use the field instead"
                )));
            }
        }
        for (k, val) in &v.options {
            props.insert(k.clone(), val.clone());
        }
        add_dev(
            dname,
            DesiredDevice {
                props,
                search: None,
            },
        )?;
    }

    for p in &spec.ports {
        // Docker-style shorthand: the protocol defaults to tcp and the host to
        // 127.0.0.1. A VM's connect side defaults to 0.0.0.0 instead, which is
        // how incus' NAT mode finds the VM's own address.
        let connect_host = if vm { "0.0.0.0" } else { "127.0.0.1" };
        let listen = normalize_addr(&p.listen, "127.0.0.1")
            .map_err(|e| Error::invalid(format!("{name}: port listen: {e}")))?;
        let connect = normalize_addr(&p.connect, connect_host)
            .map_err(|e| Error::invalid(format!("{name}: port connect: {e}")))?;
        if p.search.is_some() {
            if split_addr(&connect).is_none_or(|(_, h, _)| h != connect_host) {
                return Err(Error::invalid(format!(
                    "{name}: a published port range connects to the guest's default address ({connect_host}), not {connect}"
                )));
            }
            if p.bind != PortBind::Host {
                return Err(Error::invalid(format!(
                    "{name}: port search only applies to bind: host"
                )));
            }
            if split_addr(&listen).is_none() {
                return Err(Error::invalid(format!(
                    "{name}: port search needs a single tcp or udp listen port"
                )));
            }
        }
        let dname = p
            .name
            .clone()
            .unwrap_or_else(|| default_port_name(p.bind, &listen));
        let mut props = Props::from([
            ("type".into(), "proxy".into()),
            ("bind".into(), p.bind.as_str().into()),
            ("listen".into(), listen),
            ("connect".into(), connect),
        ]);
        if vm {
            // incus proxies into a VM only in NAT mode.
            props.insert("nat".into(), "true".into());
        }
        for (k, val) in &p.options {
            if matches!(k.as_str(), "type" | "bind" | "listen" | "connect") {
                return Err(Error::invalid(format!(
                    "{name}: port options.{k} would override a core property; use the field instead"
                )));
            }
            props.insert(k.clone(), val.clone());
        }
        add_dev(
            dname,
            DesiredDevice {
                props,
                search: p.search.filter(|n| *n > 0),
            },
        )?;
    }

    let mut root_extra = Props::new();
    for (dname, props) in &spec.raw_devices {
        if dname == "root" {
            // Tune the root disk (size, ...): merged over the generated one below.
            root_extra.extend(props.clone());
            continue;
        }
        if !props.contains_key("type") {
            return Err(Error::invalid(format!(
                "{name}: raw device {dname:?} needs a type"
            )));
        }
        add_dev(
            dname.clone(),
            DesiredDevice {
                props: props.clone(),
                search: None,
            },
        )?;
    }

    if let Some(root) = devices.get_mut("root") {
        root.props.extend(root_extra);
    }

    let ready_timeout = match &spec.ready_timeout {
        Some(s) => parse_duration(s).map_err(|e| Error::invalid(format!("{name}: {e}")))?,
        // A container is usable about a second after Running; a VM boots a
        // kernel and its agent (50-90s under nested virtualization, plus the
        // reboot a cloud image does on first boot).
        None if vm => Duration::from_secs(300),
        None => Duration::from_secs(60),
    };

    Ok(Desired {
        name,
        instance_type: spec.instance_type,
        image,
        pool,
        profiles: spec
            .profiles
            .clone()
            .unwrap_or_else(|| vec!["default".into()]),
        config,
        devices,
        volumes,
        owners,
        ready: spec.ready.clone().unwrap_or_else(|| {
            if vm {
                vec![ReadyCheck::Running, ReadyCheck::Agent]
            } else {
                vec![ReadyCheck::Running]
            }
        }),
        ready_timeout,
        exec: spec.exec_defaults(),
        idmap_mode,
        sensitive: spec
            .env
            .secrets
            .keys()
            .map(|k| format!("environment.{k}"))
            .collect(),
    })
}

/// Instance state as incus reports it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Actual {
    pub status: String,
    pub config: Props,
    /// Instance-local devices (not the ones inherited from profiles).
    pub devices: BTreeMap<String, Props>,
    pub profiles: Vec<String>,
    pub instance_type: String,
}

impl Actual {
    pub fn from_api(v: &Value) -> Actual {
        let strmap = |v: Option<&Value>| -> Props {
            v.and_then(Value::as_object)
                .map(|m| {
                    m.iter()
                        .map(|(k, v)| {
                            let s = match v {
                                Value::String(s) => s.clone(),
                                other => other.to_string(),
                            };
                            (k.clone(), s)
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        let devices = v
            .get("devices")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .map(|(k, d)| (k.clone(), strmap(Some(d))))
                    .collect()
            })
            .unwrap_or_default();
        Actual {
            status: v
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            config: strmap(v.get("config")),
            devices,
            profiles: v
                .get("profiles")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            instance_type: v
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        }
    }

    pub fn running(&self) -> bool {
        self.status.eq_ignore_ascii_case("running")
    }
}

/// One step of a plan.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    CreateVolume {
        pool: String,
        volume: String,
        config: Props,
    },
    CreateInstance {
        image: String,
        pool: String,
        config: Props,
        devices: BTreeMap<String, Props>,
        profiles: Vec<String>,
    },
    SetConfig {
        key: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        from: Option<String>,
        to: String,
        /// Only takes effect after a restart.
        restart: bool,
        /// A secret value: `from` and `to` say `(secret)`, and the value set
        /// is the desired config's.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        secret: bool,
    },
    AddDevice {
        device: String,
        props: Props,
    },
    /// Replace a wrong device. For a disk that is a remount inside the guest.
    ReplaceDevice {
        device: String,
        /// The existing device being replaced (may differ in name).
        replaces: String,
        from: Props,
        to: Props,
    },
    RemoveDevice {
        device: String,
        props: Props,
    },
    StartInstance,
    /// Add a host-bound proxy, moving up to `search` ports past a taken one.
    AddPort {
        device: String,
        props: Props,
        search: u16,
    },
    FixOwner {
        path: String,
        owner: String,
    },
    /// Something isb will not change (fixed at creation, or ambiguous). Informational.
    Note {
        message: String,
    },
}

impl Action {
    /// Whether this action changes anything.
    pub fn is_change(&self) -> bool {
        !matches!(self, Action::Note { .. })
    }
}

impl std::fmt::Display for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let props = |p: &Props| {
            p.iter()
                .filter(|(k, _)| k.as_str() != "type")
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        match self {
            Action::CreateVolume { pool, volume, .. } => {
                write!(f, "+ volume {volume} (pool {pool})")
            }
            Action::CreateInstance {
                image,
                pool,
                devices,
                ..
            } => write!(
                f,
                "+ create from {image} on pool {pool} with {} device(s)",
                devices.len()
            ),
            Action::SetConfig {
                key,
                from,
                to,
                restart,
                ..
            } => write!(
                f,
                "~ config {key}: {} -> {to}{}",
                from.as_deref().unwrap_or("(unset)"),
                if *restart {
                    " (takes effect on restart)"
                } else {
                    ""
                }
            ),
            Action::AddDevice { device, props: p } => write!(f, "+ device {device}: {}", props(p)),
            Action::ReplaceDevice {
                device,
                replaces,
                from,
                to,
            } => {
                if device == replaces {
                    write!(f, "~ device {device}: {} -> {}", props(from), props(to))
                } else {
                    write!(
                        f,
                        "~ device {replaces} -> {device}: {} -> {}",
                        props(from),
                        props(to)
                    )
                }
            }
            Action::RemoveDevice { device, .. } => write!(f, "- device {device}"),
            Action::StartInstance => write!(f, "> start"),
            Action::AddPort {
                device,
                props: p,
                search,
            } => write!(f, "+ port {device}: {} (search {search})", props(p)),
            Action::FixOwner { path, owner } => write!(f, "~ chown {owner} {path}"),
            Action::Note { message } => write!(f, "  note: {message}"),
        }
    }
}

/// The plan for one sandbox.
#[derive(Debug, Clone, Serialize)]
pub struct SandboxPlan {
    pub name: String,
    /// Existing status (`None`: does not exist yet).
    pub status: Option<String>,
    pub actions: Vec<Action>,
}

impl SandboxPlan {
    /// No changes (notes only).
    pub fn is_noop(&self) -> bool {
        !self.actions.iter().any(Action::is_change)
    }
}

/// Options for [`diff`].
#[derive(Debug, Clone, Copy, Default)]
pub struct DiffOptions {
    /// Remove instance-local devices not in the spec (never `root`).
    pub prune_devices: bool,
}

/// Keys whose value is a boolean where `false` is the same as absent.
const FALSE_IS_ABSENT: &[&str] = &["readonly", "shift", "nat"];

fn normalize(p: &Props) -> Props {
    let mut out = p.clone();
    for k in FALSE_IS_ABSENT {
        if out.get(*k).map(String::as_str) == Some("false") {
            out.remove(*k);
        }
    }
    if out.get("type").map(String::as_str) == Some("disk") {
        for k in ["source", "path"] {
            if let Some(v) = out.get_mut(k) {
                if v.len() > 1 {
                    *v = v.trim_end_matches('/').to_string();
                }
            }
        }
    }
    out
}

/// Whether an existing device satisfies a desired one.
pub fn device_matches(desired: &DesiredDevice, actual: &Props) -> bool {
    let mut d = normalize(&desired.props);
    let mut a = normalize(actual);
    // initial.copy only acts the first time a volume is used, so a disk that
    // differs in nothing else is correct, and replacing it would remount it.
    if d.get("type").map(String::as_str) == Some("disk") {
        d.remove("initial.copy");
        a.remove("initial.copy");
    }
    if d == a {
        return true;
    }
    let Some(n) = desired.search else {
        return false;
    };
    // A searched port is correct anywhere in its range.
    let (Some(dl), Some(al)) = (d.get("listen"), a.get("listen")) else {
        return false;
    };
    let (Some((dp, dh, dport)), Some((ap, ah, aport))) = (split_addr(dl), split_addr(al)) else {
        return false;
    };
    if dp != ap || dh != ah || aport < dport || aport as u32 > dport as u32 + n as u32 {
        return false;
    }
    let strip = |m: &Props| {
        let mut m = m.clone();
        m.remove("listen");
        m
    };
    strip(&d) == strip(&a)
}

/// What plans and reports show for a secret value.
pub const REDACTED: &str = "(secret)";

fn restart_needed(key: &str) -> bool {
    key.starts_with("raw.") || key.starts_with("security.") || key.starts_with("oci.")
}

/// Diff desired against actual (`None`: the instance does not exist).
/// `volumes_missing` lists named volumes (pool, name) that do not exist yet.
#[expect(
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    reason = "predates the lint ratchet; split it when next changed"
)]
pub fn diff(
    desired: &Desired,
    actual: Option<&Actual>,
    volumes_missing: &[(String, String)],
    opts: DiffOptions,
) -> Result<SandboxPlan> {
    let mut actions = Vec::new();
    for v in &desired.volumes {
        if volumes_missing.contains(&(v.pool.clone(), v.name.clone())) {
            if v.external {
                return Err(Error::invalid(format!(
                    "volume {} is external but does not exist in pool {}",
                    v.name, v.pool
                )));
            }
            actions.push(Action::CreateVolume {
                pool: v.pool.clone(),
                volume: v.name.clone(),
                config: v.config.clone(),
            });
        }
    }

    let Some(actual) = actual else {
        actions.push(Action::CreateInstance {
            image: desired.image.spec.clone(),
            pool: desired.pool.clone(),
            config: desired
                .config
                .iter()
                .map(|(k, v)| {
                    let v = if desired.sensitive.contains(k) {
                        REDACTED.to_string()
                    } else {
                        v.clone()
                    };
                    (k.clone(), v)
                })
                .collect(),
            devices: desired
                .devices
                .iter()
                .filter(|(_, d)| d.search.is_none())
                .map(|(k, d)| (k.clone(), d.props.clone()))
                .collect(),
            profiles: desired.profiles.clone(),
        });
        actions.push(Action::StartInstance);
        push_searched_ports(desired, &mut actions);
        for o in &desired.owners {
            actions.push(Action::FixOwner {
                path: o.path.clone(),
                owner: o.owner.clone(),
            });
        }
        return Ok(SandboxPlan {
            name: desired.name.clone(),
            status: None,
            actions,
        });
    };

    // Fixed-at-creation properties: report, never change.
    if !actual.instance_type.is_empty() && actual.instance_type != desired.instance_type.as_api() {
        actions.push(Action::Note {
            message: format!(
                "type is {} (spec: {}); fixed at creation",
                actual.instance_type,
                desired.instance_type.as_api()
            ),
        });
    }
    if let Some(root) = actual.devices.get("root") {
        if let Some(p) = root.get("pool") {
            if *p != desired.pool {
                actions.push(Action::Note {
                    message: format!(
                        "root disk is on pool {p} (spec: {}); fixed at creation",
                        desired.pool
                    ),
                });
            }
        }
    }
    if actual.profiles != desired.profiles {
        actions.push(Action::Note {
            message: format!(
                "profiles are [{}] (spec: [{}]); fixed at creation",
                actual.profiles.join(", "),
                desired.profiles.join(", ")
            ),
        });
    }

    for (k, v) in &desired.config {
        let cur = actual.config.get(k);
        if cur != Some(v) {
            let secret = desired.sensitive.contains(k);
            let hide = |s: &String| if secret { REDACTED.into() } else { s.clone() };
            actions.push(Action::SetConfig {
                key: k.clone(),
                from: cur.map(hide),
                to: hide(v),
                restart: restart_needed(k),
                secret,
            });
        }
    }
    if !desired.config.contains_key("raw.idmap") && actual.config.contains_key("raw.idmap") {
        let why = match desired.idmap_mode {
            Some(crate::spec::IdmapMode::Auto) => Some("not needed on this host"),
            Some(crate::spec::IdmapMode::None) => Some("the spec says idmap: none"),
            _ => None,
        };
        if let Some(why) = why {
            actions.push(Action::Note {
                message: format!(
                    "raw.idmap is set but {why}; isb never removes config keys, unset it by hand"
                ),
            });
        }
    }

    let mut new_devices: Vec<String> = Vec::new();
    let mut claimed: BTreeSet<String> = BTreeSet::new();
    let mut deferred_ports = Vec::new();
    for (name, want) in &desired.devices {
        if name == "root" {
            continue;
        }
        if let Some(have) = actual.devices.get(name) {
            claimed.insert(name.clone());
            if !device_matches(want, have) && want.search.is_some() {
                // Out of its range or otherwise wrong: drop it and search again,
                // rather than replacing it at a port that may be taken.
                actions.push(Action::RemoveDevice {
                    device: name.clone(),
                    props: have.clone(),
                });
                deferred_ports.push(name.clone());
            } else if !device_matches(want, have) {
                actions.push(Action::ReplaceDevice {
                    device: name.clone(),
                    replaces: name.clone(),
                    from: have.clone(),
                    to: want.props.clone(),
                });
                new_devices.push(name.clone());
            }
            continue;
        }
        // Not present under this name. An equivalent device under another name
        // satisfies it (adopting what another tool created); a disk at the same
        // guest path that differs has to be replaced, since two disks cannot
        // share a mount point.
        let same_path = |p: &Props| {
            want.props.get("type").map(String::as_str) == Some("disk")
                && p.get("type").map(String::as_str) == Some("disk")
                && normalize(p).get("path") == normalize(&want.props).get("path")
        };
        if let Some((other, have)) = actual.devices.iter().find(|(n, p)| {
            *n != "root" && !desired.devices.contains_key(*n) && device_matches(want, p)
        }) {
            claimed.insert(other.clone());
            actions.push(Action::Note {
                message: format!("device {name} already present as {other}; left as is"),
            });
            let _ = have;
            continue;
        }
        if let Some((other, have)) = actual
            .devices
            .iter()
            .find(|(n, p)| *n != "root" && !desired.devices.contains_key(*n) && same_path(p))
        {
            claimed.insert(other.clone());
            actions.push(Action::ReplaceDevice {
                device: name.clone(),
                replaces: other.clone(),
                from: have.clone(),
                to: want.props.clone(),
            });
            new_devices.push(name.clone());
            continue;
        }
        if want.search.is_some() {
            deferred_ports.push(name.clone());
        } else {
            actions.push(Action::AddDevice {
                device: name.clone(),
                props: want.props.clone(),
            });
            new_devices.push(name.clone());
        }
    }

    if opts.prune_devices {
        for (name, props) in &actual.devices {
            if name != "root" && !desired.devices.contains_key(name) && !claimed.contains(name) {
                actions.push(Action::RemoveDevice {
                    device: name.clone(),
                    props: props.clone(),
                });
            }
        }
    }

    if !actual.running() {
        actions.push(Action::StartInstance);
    }
    for name in deferred_ports {
        let d = &desired.devices[&name];
        actions.push(Action::AddPort {
            device: name.clone(),
            props: d.props.clone(),
            search: d.search.unwrap_or(0),
        });
    }
    for o in &desired.owners {
        if new_devices.contains(&o.device) {
            actions.push(Action::FixOwner {
                path: o.path.clone(),
                owner: o.owner.clone(),
            });
        }
    }

    Ok(SandboxPlan {
        name: desired.name.clone(),
        status: Some(actual.status.clone()),
        actions,
    })
}

fn push_searched_ports(desired: &Desired, actions: &mut Vec<Action>) {
    for (name, d) in &desired.devices {
        if let Some(n) = d.search {
            actions.push(Action::AddPort {
                device: name.clone(),
                props: d.props.clone(),
                search: n,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{IdmapMode, IdmapSpec, NamedVolumeSpec, PortBinding, Volume};

    const TITAN: &str = "root:1000000:1000000000\nroot:1000:1\n";

    fn host() -> HostFacts {
        HostFacts {
            subids: SubIds {
                subuid: TITAN.into(),
                subgid: TITAN.into(),
                caller_owned: false,
            },
            pools: vec!["container-roots".into(), "default".into()],
            path_map: None,
            initial_copy: false,
            shared_root: None,
            org: None,
            registry: None,
        }
    }

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn lasso_spec(web: &str) -> SandboxSpec {
        SandboxSpec::new("dev-lasso-x-12345678", "dev-base")
            .cpus(8)
            .memory("8GiB")
            .privileged(false)
            .idmap(IdmapSpec::Mode(IdmapMode::Auto))
            .label("lasso.worktree", "/w")
            .label("lasso.web", web)
            .volume("/home/dev/repo/src/web", Volume::bind(web).device("web"))
            .volume(
                "/home/dev/.bun/install/cache",
                Volume::named("lasso-bun-cache")
                    .device("bun-cache")
                    .owner("dev"),
            )
    }

    /// What incus would hold after creating `d`.
    fn actual_from(d: &Desired) -> Actual {
        Actual {
            status: "Running".into(),
            config: d.config.clone(),
            devices: d
                .devices
                .iter()
                .map(|(k, v)| (k.clone(), v.props.clone()))
                .collect(),
            profiles: d.profiles.clone(),
            instance_type: "container".into(),
        }
    }

    #[test]
    fn pool_auto_prefers_incus_zfs_then_default() {
        let mut h = host();
        assert_eq!(h.pick_pool(None).unwrap(), "default");
        h.pools.push("incus-zfs".into());
        assert_eq!(h.pick_pool(Some("auto")).unwrap(), "incus-zfs");
        h.pools = vec!["only".into()];
        assert_eq!(h.pick_pool(None).unwrap(), "only");
        assert!(h.pick_pool(Some("nope")).is_err());
        h.pools.clear();
        assert!(h.pick_pool(None).is_err());
    }

    #[test]
    fn named_volumes_seed_from_the_image_when_the_server_can() {
        let t = tmp();
        let web = t.path().to_str().unwrap();
        let spec = lasso_spec(web).volume("/srv/plain", Volume::named("plain").nocopy(true));
        let copy = |h: &HostFacts, s: &SandboxSpec| {
            let d = resolve(s, &VolumeDefs::new(), h, Path::new("/")).unwrap();
            ["bun-cache", "web", "srv-plain"]
                .map(|k| d.devices[k].props.get("initial.copy").cloned())
        };
        // An older server: no key anywhere, the behavior isb always had.
        assert_eq!(copy(&host(), &spec), [None, None, None]);
        let mut h = host();
        h.initial_copy = true;
        // Named volumes only, and nocopy opts out.
        assert_eq!(copy(&h, &spec), [Some("true".into()), None, None]);
        // Not in a VM: incus only seeds container volumes.
        let mut vm = SandboxSpec::new("vm-x", "dev-base").volume("/c", Volume::named("c"));
        vm.instance_type = InstanceType::VirtualMachine;
        let d = resolve(&vm, &VolumeDefs::new(), &h, Path::new("/")).unwrap();
        assert!(!d.devices["c"].props.contains_key("initial.copy"));
        // nocopy on a bind mount is an error, as are pool and external.
        let bad = lasso_spec(web).volume("/x", Volume::bind(web).nocopy(true));
        assert!(resolve(&bad, &VolumeDefs::new(), &h, Path::new("/")).is_err());
    }

    #[test]
    fn initial_copy_alone_never_replaces_a_disk() {
        let t = tmp();
        let web = t.path().to_str().unwrap();
        // A sandbox made before the server could seed volumes, then reconciled
        // after the upgrade: its volume must not be remounted.
        let old = resolve(
            &lasso_spec(web),
            &VolumeDefs::new(),
            &host(),
            Path::new("/"),
        )
        .unwrap();
        let mut h = host();
        h.initial_copy = true;
        let new = resolve(&lasso_spec(web), &VolumeDefs::new(), &h, Path::new("/")).unwrap();
        assert!(new.devices["bun-cache"].props.contains_key("initial.copy"));
        assert!(device_matches(
            &new.devices["bun-cache"],
            &actual_from(&old).devices["bun-cache"]
        ));
    }

    #[test]
    fn resolves_lasso_shape() {
        let t = tmp();
        let web = t.path().to_str().unwrap();
        let d = resolve(
            &lasso_spec(web),
            &VolumeDefs::new(),
            &host(),
            Path::new("/"),
        )
        .unwrap();
        assert_eq!(d.pool, "default");
        assert_eq!(d.config["limits.cpu"], "8");
        assert_eq!(d.config["limits.memory"], "8GiB");
        assert_eq!(d.config["security.privileged"], "false");
        assert_eq!(d.config["raw.idmap"], "both 1000 1000");
        assert_eq!(d.config["user.lasso.worktree"], "/w");
        let web_dev = &d.devices["web"].props;
        assert_eq!(web_dev["path"], "/home/dev/repo/src/web");
        let canon = std::fs::canonicalize(web).unwrap();
        assert_eq!(web_dev["source"], canon.to_str().unwrap());
        let bun = &d.devices["bun-cache"].props;
        assert_eq!(bun["pool"], "default");
        assert_eq!(bun["source"], "lasso-bun-cache");
        assert_eq!(d.devices["root"].props["pool"], "default");
        assert_eq!(d.volumes.len(), 1);
        assert_eq!(d.owners[0].path, "/home/dev/.bun/install/cache");
    }

    #[test]
    fn fresh_plan_creates_then_starts_then_chowns() {
        let t = tmp();
        let d = resolve(
            &lasso_spec(t.path().to_str().unwrap()),
            &VolumeDefs::new(),
            &host(),
            Path::new("/"),
        )
        .unwrap();
        let missing = vec![("default".to_string(), "lasso-bun-cache".to_string())];
        let p = diff(&d, None, &missing, DiffOptions::default()).unwrap();
        let kinds: Vec<&str> = p
            .actions
            .iter()
            .map(|a| match a {
                Action::CreateVolume { .. } => "vol",
                Action::CreateInstance { .. } => "create",
                Action::StartInstance => "start",
                Action::FixOwner { .. } => "chown",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, vec!["vol", "create", "start", "chown"]);
        // Every mount is in the create request, before first boot.
        match &p.actions[1] {
            Action::CreateInstance { devices, .. } => {
                assert!(devices.contains_key("web"));
                assert!(devices.contains_key("bun-cache"));
                assert!(devices.contains_key("root"));
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn ensure_on_correct_instance_is_noop() {
        let t = tmp();
        let d = resolve(
            &lasso_spec(t.path().to_str().unwrap()),
            &VolumeDefs::new(),
            &host(),
            Path::new("/"),
        )
        .unwrap();
        let mut a = actual_from(&d);
        // Things incus adds and other tools add must not register as drift.
        a.config.insert("volatile.uuid".into(), "x".into());
        a.config.insert("user.someone.else".into(), "y".into());
        a.devices.insert(
            "icon".into(),
            Props::from([
                ("type".into(), "disk".into()),
                ("path".into(), "/home/dev/repo/docs/icon".into()),
                ("source".into(), "/x/docs/icon".into()),
            ]),
        );
        a.devices.insert(
            "vite".into(),
            Props::from([
                ("type".into(), "proxy".into()),
                ("bind".into(), "host".into()),
                ("listen".into(), "tcp:1.2.3.4:5174".into()),
                ("connect".into(), "tcp:127.0.0.1:5173".into()),
            ]),
        );
        let p = diff(&d, Some(&a), &[], DiffOptions::default()).unwrap();
        assert!(p.is_noop(), "{:?}", p.actions);
        assert!(p.actions.is_empty(), "{:?}", p.actions);
    }

    #[test]
    fn readonly_false_equals_absent() {
        let t = tmp();
        let d = resolve(
            &lasso_spec(t.path().to_str().unwrap()),
            &VolumeDefs::new(),
            &host(),
            Path::new("/"),
        )
        .unwrap();
        let mut a = actual_from(&d);
        a.devices
            .get_mut("web")
            .unwrap()
            .insert("readonly".into(), "false".into());
        let src = a.devices["web"]["source"].clone();
        a.devices
            .get_mut("web")
            .unwrap()
            .insert("source".into(), format!("{src}/"));
        assert!(
            diff(&d, Some(&a), &[], DiffOptions::default())
                .unwrap()
                .is_noop()
        );
    }

    #[test]
    fn only_the_wrong_device_is_replaced() {
        let t = tmp();
        let d = resolve(
            &lasso_spec(t.path().to_str().unwrap()),
            &VolumeDefs::new(),
            &host(),
            Path::new("/"),
        )
        .unwrap();
        let mut a = actual_from(&d);
        a.devices
            .get_mut("web")
            .unwrap()
            .insert("source".into(), "/somewhere/else".into());
        let p = diff(&d, Some(&a), &[], DiffOptions::default()).unwrap();
        assert_eq!(p.actions.len(), 1, "{:?}", p.actions);
        match &p.actions[0] {
            Action::ReplaceDevice { device, from, .. } => {
                assert_eq!(device, "web");
                assert_eq!(from["source"], "/somewhere/else");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn missing_device_is_added_and_owner_fixed() {
        let t = tmp();
        let d = resolve(
            &lasso_spec(t.path().to_str().unwrap()),
            &VolumeDefs::new(),
            &host(),
            Path::new("/"),
        )
        .unwrap();
        let mut a = actual_from(&d);
        a.devices.remove("bun-cache");
        let p = diff(&d, Some(&a), &[], DiffOptions::default()).unwrap();
        assert!(matches!(&p.actions[0], Action::AddDevice { device, .. } if device == "bun-cache"));
        assert!(matches!(&p.actions[1], Action::FixOwner { .. }));
        assert_eq!(p.actions.len(), 2);
    }

    #[test]
    fn equivalent_device_under_other_name_is_adopted() {
        let t = tmp();
        let d = resolve(
            &lasso_spec(t.path().to_str().unwrap()),
            &VolumeDefs::new(),
            &host(),
            Path::new("/"),
        )
        .unwrap();
        let mut a = actual_from(&d);
        let web = a.devices.remove("web").unwrap();
        a.devices.insert("legacy-web".into(), web);
        let p = diff(&d, Some(&a), &[], DiffOptions::default()).unwrap();
        assert!(p.is_noop(), "{:?}", p.actions);
        // Same path, different source: replaced under our name (two disks cannot
        // share a mount point).
        a.devices
            .get_mut("legacy-web")
            .unwrap()
            .insert("source".into(), "/other".into());
        let p = diff(&d, Some(&a), &[], DiffOptions::default()).unwrap();
        assert!(matches!(&p.actions[0],
            Action::ReplaceDevice { device, replaces, .. } if device == "web" && replaces == "legacy-web"));
    }

    #[test]
    fn config_drift_and_restart_flag() {
        let t = tmp();
        let d = resolve(
            &lasso_spec(t.path().to_str().unwrap()),
            &VolumeDefs::new(),
            &host(),
            Path::new("/"),
        )
        .unwrap();
        let mut a = actual_from(&d);
        a.config.insert("limits.cpu".into(), "4".into());
        a.config.remove("raw.idmap");
        let p = diff(&d, Some(&a), &[], DiffOptions::default()).unwrap();
        assert!(p.actions.contains(&Action::SetConfig {
            key: "limits.cpu".into(),
            from: Some("4".into()),
            to: "8".into(),
            restart: false,
            secret: false,
        }));
        assert!(p.actions.contains(&Action::SetConfig {
            key: "raw.idmap".into(),
            from: None,
            to: "both 1000 1000".into(),
            restart: true,
            secret: false,
        }));
    }

    #[test]
    fn secret_environment_is_set_but_never_shown() {
        let t = tmp();
        let mut s = lasso_spec(t.path().to_str().unwrap());
        s.env.secrets.insert("TOKEN".into(), "tok".into());
        s.env.vars.insert("TOKEN".into(), "hunter2".into());
        let d = resolve(&s, &VolumeDefs::new(), &host(), Path::new("/")).unwrap();
        assert_eq!(d.config["environment.TOKEN"], "hunter2");
        let created = diff(&d, None, &[], DiffOptions::default()).unwrap();
        let shown = format!(
            "{:?} {}",
            created.actions,
            serde_json::to_string(&created).unwrap()
        );
        assert!(!shown.contains("hunter2"), "{shown}");
        let mut a = actual_from(&d);
        a.config
            .insert("environment.TOKEN".into(), "old-value".into());
        let p = diff(&d, Some(&a), &[], DiffOptions::default()).unwrap();
        let shown = format!(
            "{} {}",
            p.actions
                .iter()
                .map(|a| a.to_string())
                .collect::<Vec<_>>()
                .join("; "),
            serde_json::to_string(&p).unwrap()
        );
        assert!(
            !shown.contains("hunter2") && !shown.contains("old-value"),
            "{shown}"
        );
        assert!(p.actions.iter().any(|a| matches!(
            a,
            Action::SetConfig { key, secret: true, .. } if key == "environment.TOKEN"
        )));
    }

    #[test]
    fn stopped_instance_is_started_after_changes() {
        let t = tmp();
        let d = resolve(
            &lasso_spec(t.path().to_str().unwrap()),
            &VolumeDefs::new(),
            &host(),
            Path::new("/"),
        )
        .unwrap();
        let mut a = actual_from(&d);
        a.status = "Stopped".into();
        a.config.insert("limits.memory".into(), "1GiB".into());
        let p = diff(&d, Some(&a), &[], DiffOptions::default()).unwrap();
        assert!(matches!(p.actions[0], Action::SetConfig { .. }));
        assert_eq!(p.actions[1], Action::StartInstance);
    }

    #[test]
    fn prune_removes_unknown_devices_but_never_root() {
        let t = tmp();
        let d = resolve(
            &lasso_spec(t.path().to_str().unwrap()),
            &VolumeDefs::new(),
            &host(),
            Path::new("/"),
        )
        .unwrap();
        let mut a = actual_from(&d);
        a.devices.insert(
            "extra".into(),
            Props::from([("type".into(), "none".into())]),
        );
        let p = diff(&d, Some(&a), &[], DiffOptions::default()).unwrap();
        assert!(p.is_noop());
        let p = diff(
            &d,
            Some(&a),
            &[],
            DiffOptions {
                prune_devices: true,
            },
        )
        .unwrap();
        assert_eq!(p.actions.len(), 1);
        assert!(matches!(&p.actions[0], Action::RemoveDevice { device, .. } if device == "extra"));
    }

    #[test]
    fn searched_port_matches_anywhere_in_range() {
        let t = tmp();
        let spec = lasso_spec(t.path().to_str().unwrap()).port(
            PortBinding::host("tcp:100.1.2.3:5173", "tcp:127.0.0.1:5173")
                .name("vite")
                .search(50),
        );
        let d = resolve(&spec, &VolumeDefs::new(), &host(), Path::new("/")).unwrap();
        let mut a = actual_from(&d);
        a.devices
            .get_mut("vite")
            .unwrap()
            .insert("listen".into(), "tcp:100.1.2.3:5190".into());
        assert!(
            diff(&d, Some(&a), &[], DiffOptions::default())
                .unwrap()
                .is_noop()
        );
        a.devices
            .get_mut("vite")
            .unwrap()
            .insert("listen".into(), "tcp:100.1.2.3:5300".into());
        let p = diff(&d, Some(&a), &[], DiffOptions::default()).unwrap();
        assert!(matches!(&p.actions[0], Action::RemoveDevice { device, .. } if device == "vite"));
        assert!(matches!(&p.actions[1], Action::AddPort { search: 50, .. }));
        // Missing: deferred until after start so the search runs against live binds.
        a.devices.remove("vite");
        let p = diff(&d, Some(&a), &[], DiffOptions::default()).unwrap();
        assert!(matches!(&p.actions[0], Action::AddPort { search: 50, .. }));
        // Fresh: not in the create request.
        let p = diff(&d, None, &[], DiffOptions::default()).unwrap();
        match &p.actions[0] {
            Action::CreateInstance { devices, .. } => assert!(!devices.contains_key("vite")),
            other => panic!("{other:?}"),
        }
        assert!(matches!(&p.actions[2], Action::AddPort { .. }));
    }

    #[test]
    fn port_shorthand() {
        let n = |a: &str| normalize_addr(a, "127.0.0.1");
        assert_eq!(n("5173").unwrap(), "tcp:127.0.0.1:5173");
        assert_eq!(n("0.0.0.0:5173").unwrap(), "tcp:0.0.0.0:5173");
        assert_eq!(n("5353/udp").unwrap(), "udp:127.0.0.1:5353");
        assert_eq!(n("10.0.0.1:5353/udp").unwrap(), "udp:10.0.0.1:5353");
        assert_eq!(n("tcp:5173").unwrap(), "tcp:127.0.0.1:5173");
        assert_eq!(n("udp:5353").unwrap(), "udp:127.0.0.1:5353");
        assert_eq!(n("tcp:100.1.2.3:5173").unwrap(), "tcp:100.1.2.3:5173");
        assert_eq!(n("[::1]:5173").unwrap(), "tcp:[::1]:5173");
        assert_eq!(n("tcp:[::1]:5173").unwrap(), "tcp:[::1]:5173");
        assert_eq!(n("8000-8010").unwrap(), "tcp:127.0.0.1:8000-8010");
        assert_eq!(n("80,443").unwrap(), "tcp:127.0.0.1:80,443");
        assert_eq!(n("unix:/run/x.sock").unwrap(), "unix:/run/x.sock");
        assert_eq!(normalize_addr("80", "0.0.0.0").unwrap(), "tcp:0.0.0.0:80");
        for bad in [
            "",
            "abc",
            "0",
            "70000",
            "::1:80",
            "5173/sctp",
            "unix:",
            ":80",
            "tcp:",
            "[::1]80",
        ] {
            assert!(n(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn shorthand_ports_match_existing_full_form_devices() {
        let t = tmp();
        let spec = lasso_spec(t.path().to_str().unwrap())
            .port(PortBinding::host("5173", "5173").name("vite"))
            .port(PortBinding::guest("8190", "tcp:127.0.0.1:9000").name("backend"));
        let d = resolve(&spec, &VolumeDefs::new(), &host(), Path::new("/")).unwrap();
        assert_eq!(d.devices["vite"].props["listen"], "tcp:127.0.0.1:5173");
        assert_eq!(d.devices["vite"].props["connect"], "tcp:127.0.0.1:5173");
        assert_eq!(d.devices["backend"].props["listen"], "tcp:127.0.0.1:8190");
        let a = actual_from(&d);
        assert!(
            diff(&d, Some(&a), &[], DiffOptions::default())
                .unwrap()
                .is_noop()
        );
        // A VM's connect side defaults to 0.0.0.0 (incus NAT finds the VM).
        let mut vm = spec.clone();
        vm.instance_type = crate::spec::InstanceType::VirtualMachine;
        vm.privileged = None;
        vm.idmap = None;
        vm.ports.retain(|p| p.bind == PortBind::Host);
        let d = resolve(&vm, &VolumeDefs::new(), &host(), Path::new("/")).unwrap();
        assert_eq!(d.devices["vite"].props["connect"], "tcp:0.0.0.0:5173");
        // Default names use the expanded address.
        let spec =
            lasso_spec(t.path().to_str().unwrap()).port(PortBinding::host("5353/udp", "53/udp"));
        let d = resolve(&spec, &VolumeDefs::new(), &host(), Path::new("/")).unwrap();
        assert!(d.devices.contains_key("port-host-udp-5353"));
    }

    #[test]
    fn both_port_directions() {
        let t = tmp();
        let spec = lasso_spec(t.path().to_str().unwrap())
            .port(PortBinding::guest(
                "tcp:127.0.0.1:8190",
                "tcp:127.0.0.1:8191",
            ))
            .port(PortBinding::host(
                "tcp:127.0.0.1:5173",
                "tcp:127.0.0.1:5173",
            ));
        let d = resolve(&spec, &VolumeDefs::new(), &host(), Path::new("/")).unwrap();
        assert_eq!(d.devices["port-guest-8190"].props["bind"], "guest");
        assert_eq!(d.devices["port-host-5173"].props["bind"], "host");
    }

    #[test]
    fn bind_sources_stay_under_the_shared_root() {
        let t = tmp();
        let root = t.path().canonicalize().unwrap();
        let mut h = host();
        h.shared_root = Some(root.to_string_lossy().into_owned());
        let base = lasso_spec(root.to_str().unwrap());
        let r = |s: &SandboxSpec| resolve(s, &VolumeDefs::new(), &h, Path::new("/"));
        assert!(r(&base).is_ok());
        let e = r(&base.clone().volume("/x", Volume::bind("/")))
            .unwrap_err()
            .to_string();
        assert!(e.contains("shared with the isb machine"), "{e}");
    }

    #[test]
    fn validation_errors() {
        let t = tmp();
        let base = lasso_spec(t.path().to_str().unwrap());
        let r = |s: &SandboxSpec| resolve(s, &VolumeDefs::new(), &host(), Path::new("/"));
        let mut s = base.clone();
        s.name = Some("1bad".into());
        assert!(r(&s).is_err());
        let s = base.clone().volume("rel/path", Volume::bind("/"));
        assert!(r(&s).is_err());
        let s = base
            .clone()
            .volume("/x", Volume::bind("/definitely/not/here"));
        assert!(r(&s).unwrap_err().to_string().contains("does not exist"));
        let s = base.clone().volume("/x", Volume::bind("/").owner("dev"));
        assert!(r(&s).is_err());
        let s = base.clone().volume("/x", Volume::bind("/").pool("p"));
        assert!(r(&s).is_err());
        let s = base
            .clone()
            .port(PortBinding::guest("tcp:1.2.3.4:1", "tcp:1.2.3.4:2").search(3));
        assert!(r(&s).is_err());
        let s = base
            .clone()
            .port(PortBinding::host("1.2.3.4:x", "tcp:1.2.3.4:2"));
        assert!(r(&s).is_err());
        let s = base
            .clone()
            .volume("/y", Volume::bind("/").option("source", "/etc"));
        assert!(r(&s).unwrap_err().to_string().contains("core property"));
        let s = base.clone().volume("/y", Volume::bind("/").device("web"));
        assert!(r(&s).unwrap_err().to_string().contains("used twice"));
    }

    #[test]
    fn docker_memory_units() {
        assert_eq!(memory_limit("512m").unwrap(), "512MiB");
        assert_eq!(memory_limit("8g").unwrap(), "8GiB");
        // Docker reads GB as GiB too.
        assert_eq!(memory_limit("8GB").unwrap(), "8GiB");
        assert_eq!(memory_limit("8GiB").unwrap(), "8GiB");
        assert_eq!(memory_limit("50%").unwrap(), "50%");
        assert_eq!(memory_limit("1073741824").unwrap(), "1073741824");
        assert!(memory_limit("1.5g").is_err());
        assert!(memory_limit("8 parsecs").is_err());
    }

    #[test]
    fn cpus_and_cpuset() {
        let t = tmp();
        let r = |s: &SandboxSpec| resolve(s, &VolumeDefs::new(), &host(), Path::new("/"));
        let base = lasso_spec(t.path().to_str().unwrap());
        assert_eq!(r(&base).unwrap().config["limits.cpu"], "8");
        let mut s = base.clone();
        s.cpus = None;
        s.cpuset = Some("0-3".into());
        assert_eq!(r(&s).unwrap().config["limits.cpu"], "0-3");
        s.cpus = Some("2".into());
        assert!(r(&s).unwrap_err().to_string().contains("not both"));
        let mut s = base.clone();
        s.cpus = Some("0-3".into());
        assert!(r(&s).unwrap_err().to_string().contains("cpuset"));
    }

    #[test]
    fn named_volume_uses_its_declared_name() {
        let t = tmp();
        let mut defs = VolumeDefs::new();
        defs.insert(
            "lasso-bun-cache".into(),
            NamedVolumeSpec {
                name: Some("lasso-dev_lasso-bun-cache".into()),
                ..Default::default()
            },
        );
        let d = resolve(
            &lasso_spec(t.path().to_str().unwrap()),
            &defs,
            &host(),
            Path::new("/"),
        )
        .unwrap();
        assert_eq!(
            d.devices["bun-cache"].props["source"],
            "lasso-dev_lasso-bun-cache"
        );
        assert_eq!(d.volumes[0].name, "lasso-dev_lasso-bun-cache");
    }

    #[test]
    fn a_searched_port_connects_to_the_default_address() {
        let t = tmp();
        let r = |s: &SandboxSpec| resolve(s, &VolumeDefs::new(), &host(), Path::new("/"));
        let base = lasso_spec(t.path().to_str().unwrap());
        let ok = base
            .clone()
            .port(PortBinding::host("tcp:1.2.3.4:5173", "tcp:127.0.0.1:5173").search(5));
        assert_eq!(r(&ok).unwrap().devices["port-host-5173"].search, Some(5));
        let bad = base
            .clone()
            .port(PortBinding::host("tcp:1.2.3.4:5173", "tcp:10.0.0.2:5173").search(5));
        assert!(r(&bad).unwrap_err().to_string().contains("default address"));
    }

    #[test]
    fn vm_rules() {
        use crate::spec::InstanceType;
        let t = tmp();
        let r = |s: &SandboxSpec| resolve(s, &VolumeDefs::new(), &host(), Path::new("/"));
        let mut s = lasso_spec(t.path().to_str().unwrap());
        s.instance_type = InstanceType::VirtualMachine;
        // privileged and explicit idmap are container-only.
        assert!(r(&s).unwrap_err().to_string().contains("container-only"));
        s.privileged = None;
        let d = r(&s).unwrap();
        // idmap: auto is a no-op for a VM; the default readiness waits for the agent.
        assert!(!d.config.contains_key("raw.idmap"));
        assert_eq!(d.ready, vec![ReadyCheck::Running, ReadyCheck::Agent]);
        assert_eq!(d.ready_timeout, Duration::from_secs(300));
        let s2 = s
            .clone()
            .port(PortBinding::host("tcp:0.0.0.0:80", "tcp:10.0.0.2:80"));
        assert_eq!(r(&s2).unwrap().devices["port-host-80"].props["nat"], "true");
        let s3 = s
            .clone()
            .port(PortBinding::guest("tcp:127.0.0.1:1", "tcp:127.0.0.1:2"));
        assert!(r(&s3).is_err());
        s.idmap = Some(IdmapSpec::Mode(IdmapMode::Always));
        assert!(r(&s).is_err());
        // `vm` is accepted as shorthand in YAML.
        let f: crate::spec::ComposeFile =
            serde_yaml_ng::from_str("services:\n  a: {image: x, type: vm}\n").unwrap();
        assert_eq!(f.services["a"].instance_type, InstanceType::VirtualMachine);
    }

    #[test]
    fn external_volume_must_exist() {
        let t = tmp();
        let mut defs = VolumeDefs::new();
        defs.insert(
            "lasso-bun-cache".into(),
            NamedVolumeSpec {
                external: true,
                ..Default::default()
            },
        );
        let d = resolve(
            &lasso_spec(t.path().to_str().unwrap()),
            &defs,
            &host(),
            Path::new("/"),
        )
        .unwrap();
        let missing = vec![("default".to_string(), "lasso-bun-cache".to_string())];
        assert!(diff(&d, None, &missing, DiffOptions::default()).is_err());
    }

    #[test]
    fn path_translation() {
        let mut h = host();
        h.path_map = Some(("/home/u".into(), "/srv/box/home".into()));
        assert_eq!(h.translate("/home/u/src/web"), "/srv/box/home/src/web");
        assert_eq!(h.translate("/home/u"), "/srv/box/home");
        assert_eq!(h.translate("/home/user2/x"), "/home/user2/x");
        assert_eq!(h.translate("/elsewhere"), "/elsewhere");
    }

    #[test]
    fn device_names_are_deterministic() {
        assert_eq!(
            device_name_for_path("/home/dev/.bun/install/cache"),
            "home-dev-bun-install-cache"
        );
        let long = "/a/very/long/path/that/goes/on/and/on/and/on/forever/and/ever/amen";
        let n = device_name_for_path(long);
        assert!(n.len() <= 48, "{n}");
        assert_eq!(n, device_name_for_path(long));
        assert_ne!(n, device_name_for_path(&format!("{long}2")));
    }

    #[test]
    fn image_sources() {
        let i = ImageSource::parse("images:debian/12").unwrap();
        assert_eq!(
            i.server.as_deref(),
            Some("https://images.linuxcontainers.org")
        );
        assert_eq!(i.alias, "debian/12");
        let i = ImageSource::parse("dev-base").unwrap();
        assert!(i.server.is_none());
        assert_eq!(i.to_api(Some("abc"))["fingerprint"], "abc");
        assert!(ImageSource::parse("nope:x").is_err());
        assert!(ImageSource::parse("").is_err());
    }
}
