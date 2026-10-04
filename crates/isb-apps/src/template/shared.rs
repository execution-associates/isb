//! What the Dokploy and Coolify translations share: the report, the
//! bookkeeping a translation keeps, compose-file helpers and the compose
//! keys isb refuses, notes or ignores.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_yaml_ng::Value as Y;

use super::{Template, Variable};

/// How a translation went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Means the same in isb.
    Clean,
    /// Deployable; the notes say what differs.
    Notes,
    /// Not deployable; the refusals say why.
    Refused,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub status: Status,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub refusals: Vec<String>,
}

pub(super) struct Tx {
    pub(super) notes: Vec<String>,
    pub(super) refusals: Vec<String>,
    /// Native variables made so far.
    pub(super) vars: Vec<Variable>,
    /// Dokploy variable name -> native name.
    pub(super) names: BTreeMap<String, String>,
}

impl Tx {
    pub(super) fn note(&mut self, s: impl Into<String>) {
        let s = s.into();
        if !self.notes.contains(&s) {
            self.notes.push(s);
        }
    }

    pub(super) fn refuse(&mut self, s: impl Into<String>) {
        let s = s.into();
        if !self.refusals.contains(&s) {
            self.refusals.push(s);
        }
    }

    pub(super) fn fresh_name(&self, base: &str) -> String {
        let base = var_name(base);
        let taken = |n: &str| self.vars.iter().any(|v| v.name == n);
        if !taken(&base) {
            return base;
        }
        (2..)
            .map(|i| format!("{base}_{i}"))
            .find(|n| !taken(n))
            .expect("an unused name")
    }
}

/// A Dokploy (or env) name as a native variable name.
pub(super) fn var_name(s: &str) -> String {
    let mut out: String = s
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if !out.starts_with(|c: char| c.is_ascii_lowercase()) {
        out = format!("v_{out}");
    }
    out.truncate(60);
    out
}

/// A compose service (or volume) name as an app key / volume name.
pub fn key_name(s: &str) -> String {
    let k = crate::compose::sanitize_name(s);
    let k = k.strip_prefix("isb-").map(String::from).unwrap_or(k);
    let mut k: String = k.chars().take(30).collect();
    while k.ends_with('-') {
        k.pop();
    }
    if k.is_empty() || !k.starts_with(|c: char| c.is_ascii_lowercase()) {
        k = format!("s{k}");
    }
    k.chars().take(30).collect()
}

pub(super) fn lit(s: &str) -> String {
    s.replace('$', "$$")
}

pub(super) fn secretish(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["pass", "secret", "token", "key", "salt", "private", "jwt"]
        .iter()
        .any(|w| n.contains(w))
}

pub(super) fn yscalar(v: &Y) -> Option<String> {
    match v {
        Y::String(s) => Some(s.clone()),
        Y::Number(n) => Some(n.to_string()),
        Y::Bool(b) => Some(b.to_string()),
        Y::Null => Some(String::new()),
        _ => None,
    }
}

/// docker's image reference as isb's.
pub fn image_ref(image: &str) -> String {
    let (first, rest) = match image.split_once('/') {
        Some((f, r)) => (f, Some(r)),
        None => (image, None),
    };
    let is_host =
        rest.is_some() && (first.contains('.') || first.contains(':') || first == "localhost");
    match (is_host, rest) {
        (true, Some(r)) => match first {
            "docker.io" | "index.docker.io" | "registry-1.docker.io" => format!("docker:{r}"),
            "ghcr.io" => format!("ghcr:{r}"),
            "quay.io" => format!("quay:{r}"),
            _ => format!("oci:{image}"),
        },
        _ => format!("docker:{image}"),
    }
}

/// Rewrite references to the template's services (compose names) in a
/// native expression's literal text to `${host:KEY}`: after `//` or `@`,
/// before `:<port>`, or the whole value when `whole`. Returns the names
/// rewritten.
pub(super) fn rewrite_hosts(
    expr: &str,
    services: &[(String, String)],
    whole: bool,
) -> (String, Vec<String>) {
    let mut hits = Vec::new();
    if whole {
        for (name, key) in services {
            if expr == lit(name) {
                return (format!("${{host:{key}}}"), vec![name.clone()]);
            }
        }
    }
    // Work on literal stretches only: between ${...} references.
    let mut out = String::new();
    let mut rest = expr;
    loop {
        let (litpart, tail) = match next_ref(rest) {
            Some((a, b)) => (&rest[..a], Some((a, b))),
            None => (rest, None),
        };
        out.push_str(&rewrite_lit(litpart, services, &mut hits));
        match tail {
            Some((a, b)) => {
                out.push_str(&rest[a..b]);
                rest = &rest[b..];
            }
            None => break,
        }
    }
    (out, hits)
}

/// The next `${...}` in a native expression (skipping `$$`).
fn next_ref(s: &str) -> Option<(usize, usize)> {
    let b = s.as_bytes();
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i] == b'$' && b[i + 1] == b'$' {
            i += 2;
        } else if b[i] == b'$' && b[i + 1] == b'{' {
            let end = s[i..].find('}')? + i + 1;
            return Some((i, end));
        } else {
            i += 1;
        }
    }
    None
}

fn rewrite_lit(s: &str, services: &[(String, String)], hits: &mut Vec<String>) -> String {
    let word = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-');
    let mut out = String::new();
    let mut i = 0;
    'outer: while i < s.len() {
        for (name, key) in services {
            if !s[i..].starts_with(name.as_str()) {
                continue;
            }
            let before = &s[..i];
            let after = &s[i + name.len()..];
            let left_ok = before.chars().next_back().is_none_or(|c| !word(c));
            let right_ok = after.chars().next().is_none_or(|c| !word(c));
            if !(left_ok && right_ok) {
                continue;
            }
            let url_host = before.ends_with("//") || before.ends_with('@');
            let host_port =
                after.starts_with(':') && after[1..].starts_with(|c: char| c.is_ascii_digit());
            if url_host || host_port {
                out.push_str(&format!("${{host:{key}}}"));
                if !hits.contains(name) {
                    hits.push(name.clone());
                }
                i += name.len();
                continue 'outer;
            }
        }
        let c = s[i..].chars().next().expect("in bounds");
        out.push(c);
        i += c.len_utf8();
    }
    out
}

pub(super) fn hostish_key(k: &str) -> bool {
    let k = k.to_ascii_uppercase();
    [
        "HOST", "SERVER", "ADDR", "ENDPOINT", "URL", "URI", "DSN", "BROKER", "NODES", "UPSTREAM",
        "BACKEND",
    ]
    .iter()
    .any(|w| k.contains(w))
}

/// The compose keys that cannot be honoured without weakening isolation
/// or changing what the app is.
pub(super) const REFUSED_KEYS: &[(&str, &str)] = &[
    ("privileged", "privileged mode"),
    ("cap_add", "added capabilities"),
    ("devices", "host devices"),
    ("device_cgroup_rules", "device cgroup rules"),
    ("gpus", "GPUs"),
    ("pid", "the host's process namespace"),
    ("ipc", "a shared IPC namespace"),
    ("userns_mode", "a user-namespace mode"),
    ("uts", "the host's UTS namespace"),
    ("cgroup", "a cgroup namespace mode"),
    ("cgroup_parent", "a cgroup parent"),
    ("sysctls", "sysctls"),
    ("runtime", "another container runtime"),
    ("volumes_from", "volumes_from"),
    ("extra_hosts", "extra_hosts (host-gateway reaches the host)"),
    ("dns", "custom DNS servers"),
    ("dns_search", "custom DNS search domains"),
    (
        "build",
        "an image built from source (use an app with a git source)",
    ),
    ("extends", "extends"),
    ("secrets", "compose secrets"),
    ("configs", "compose configs"),
    ("post_start", "lifecycle hooks"),
    ("pre_stop", "lifecycle hooks"),
    ("isolation", "an isolation technology"),
];

/// Keys whose effect isb does not apply; the app still works, so they are
/// notes.
pub(super) const NOTED_KEYS: &[(&str, &str)] = &[
    ("ulimits", "ulimits are not applied (incus defaults)"),
    ("shm_size", "shm_size is not applied"),
    (
        "tmpfs",
        "tmpfs mounts are not made; the paths are on the root filesystem",
    ),
    ("stop_signal", "stop_signal is not applied"),
    ("stop_grace_period", "stop_grace_period is not applied"),
    ("read_only", "a read-only root filesystem is not applied"),
    (
        "cap_drop",
        "dropped capabilities are not applied (it runs as an unprivileged incus container)",
    ),
    (
        "security_opt",
        "security_opt is not applied (it runs as an unprivileged incus container)",
    ),
    ("init", "init is not applied"),
    (
        "platform",
        "platform is ignored; the host's architecture is pulled",
    ),
    (
        "hostname",
        "its hostname is not set; apps reach it as <app>.<stack>",
    ),
    ("domainname", "domainname is not set"),
    ("container_name", "container_name is ignored"),
    ("mem_reservation", "mem_reservation is not applied"),
    ("memswap_limit", "memswap_limit is not applied"),
    ("pids_limit", "pids_limit is not applied"),
    ("cpu_shares", "cpu_shares is not applied"),
    ("cpuset", "cpuset is not applied"),
    ("oom_kill_disable", "oom_kill_disable is not applied"),
    ("oom_score_adj", "oom_score_adj is not applied"),
    ("storage_opt", "storage_opt is not applied"),
    ("blkio_config", "blkio_config is not applied"),
    ("mac_address", "mac_address is not applied"),
    ("stop_grace_period", "stop_grace_period is not applied"),
];

/// Keys that mean nothing here.
pub(super) const IGNORED_KEYS: &[&str] = &[
    "image",
    "exclude_from_hc",
    "restart",
    "logging",
    "tty",
    "stdin_open",
    "pull_policy",
    "expose",
    "networks",
    "labels",
    "environment",
    "env_file",
    "volumes",
    "ports",
    "command",
    "entrypoint",
    "healthcheck",
    "depends_on",
    "user",
    "working_dir",
    "deploy",
    "mem_limit",
    "cpus",
    "links",
    "network_mode",
    "scale",
    "profiles",
    "develop",
    "annotations",
    "attach",
];

pub(super) fn ymap(v: &Y) -> Option<&serde_yaml_ng::Mapping> {
    v.as_mapping()
}

pub(super) fn yget<'a>(m: &'a serde_yaml_ng::Mapping, k: &str) -> Option<&'a Y> {
    m.get(Y::String(k.into()))
}

/// Entries of a list-or-map (`environment`, `labels`) as pairs; a bare
/// list entry has no value.
pub(super) fn pairs(v: &Y) -> Vec<(String, Option<String>)> {
    match v {
        Y::Sequence(s) => s
            .iter()
            .filter_map(yscalar)
            .map(|e| match e.split_once('=') {
                Some((k, v)) => (k.to_string(), Some(v.to_string())),
                None => (e, None),
            })
            .collect(),
        Y::Mapping(m) => m
            .iter()
            .filter_map(|(k, v)| {
                let k = yscalar(k)?;
                Some((k, if v.is_null() { None } else { yscalar(v) }))
            })
            .collect(),
        _ => vec![],
    }
}

pub(super) fn words(v: &Y) -> Option<Vec<String>> {
    match v {
        Y::String(s) => crate::flex::split_words(s).ok(),
        Y::Sequence(s) => s.iter().map(yscalar).collect(),
        Y::Null => Some(vec![]),
        _ => None,
    }
}

/// The compose file, parsed with its merge keys applied.
pub(super) fn parse_compose(compose: &str, tx: &mut Tx) -> Option<Y> {
    let mut doc: Y = match serde_yaml_ng::from_str(compose) {
        Ok(v) => v,
        Err(e) => {
            tx.refuse(format!("docker-compose.yml does not parse: {e}"));
            return None;
        }
    };
    if let Err(e) = doc.apply_merge() {
        tx.refuse(format!("docker-compose.yml merge keys: {e}"));
        return None;
    }
    Some(doc)
}

/// Refuse or note top-level compose keys isb does not use.
pub(super) fn check_top_level(top: &serde_yaml_ng::Mapping, tx: &mut Tx) {
    for (k, _) in top {
        let k = yscalar(k).unwrap_or_default();
        if !matches!(
            k.as_str(),
            "services" | "volumes" | "networks" | "version" | "name"
        ) && !k.starts_with("x-")
        {
            if k == "secrets" || k == "configs" {
                tx.refuse(format!("top-level compose {k}"));
            } else {
                tx.note(format!("top-level compose key {k} is ignored"));
            }
        }
    }
}

/// The top-level volumes; a volume isb cannot make is refused.
pub(super) fn declared_volumes(
    top: &serde_yaml_ng::Mapping,
    tx: &mut Tx,
) -> serde_yaml_ng::Mapping {
    let declared_vols = yget(top, "volumes")
        .and_then(ymap)
        .cloned()
        .unwrap_or_default();
    for (k, v) in &declared_vols {
        let k = yscalar(k).unwrap_or_default();
        if let Some(m) = ymap(v) {
            if yget(m, "external").and_then(Y::as_bool) == Some(true) {
                tx.refuse(format!("volume {k} is external (it must exist beforehand)"));
            }
            if let Some(d) = yget(m, "driver").and_then(yscalar) {
                if d != "local" {
                    tx.refuse(format!("volume {k} uses driver {d}"));
                }
            }
            if yget(m, "driver_opts").is_some() {
                tx.refuse(format!("volume {k} has driver_opts (a bind or NFS mount)"));
            }
        }
    }
    declared_vols
}

/// Each service's app name, and every name a service answers to, longest
/// first (so `db-replica` is not matched as `db`).
pub(super) fn service_names(
    services: &serde_yaml_ng::Mapping,
    tx: &mut Tx,
) -> (BTreeMap<String, String>, Vec<(String, String)>) {
    // Service names, keys, and every name a service answers to.
    let mut keys: BTreeMap<String, String> = BTreeMap::new();
    let mut aliases: Vec<(String, String)> = Vec::new();
    for (name, s) in services {
        let name = yscalar(name).unwrap_or_default();
        if s.as_mapping().and_then(|m| yget(m, "profiles")).is_some() {
            tx.note(format!(
                "{name} has compose profiles (off by default in docker); it is not deployed"
            ));
            continue;
        }
        let key = key_name(&name);
        if keys.values().any(|k| k == &key) {
            tx.refuse(format!("services {name} and another both become app {key}"));
        }
        keys.insert(name.clone(), key.clone());
        aliases.push((name.clone(), key.clone()));
        if let Some(m) = ymap(s) {
            aliases.extend(other_names(m, &name).into_iter().map(|h| (h, key.clone())));
        }
    }
    // Longest names first, so `db-replica` is not matched as `db`.
    aliases.sort_by_key(|a| std::cmp::Reverse(a.0.len()));
    (keys, aliases)
}

/// The other names a service answers to: its hostname, container name and
/// network aliases.
fn other_names(m: &serde_yaml_ng::Mapping, name: &str) -> Vec<String> {
    let mut out = Vec::new();
    for k in ["hostname", "container_name"] {
        if let Some(h) = yget(m, k).and_then(yscalar) {
            if h != name && !h.contains('$') {
                out.push(h);
            }
        }
    }
    if let Some(Y::Mapping(nets)) = yget(m, "networks") {
        for (_, n) in nets {
            if let Some(Y::Sequence(al)) = ymap(n).and_then(|n| yget(n, "aliases")) {
                out.extend(al.iter().filter_map(yscalar));
            }
        }
    }
    out
}

/// A translation's outcome: the template when nothing was refused and it
/// validates, and the report.
pub(super) fn finish(t: Option<Template>, mut tx: Tx) -> (Option<Template>, Report) {
    let t = match t {
        Some(t) if tx.refusals.is_empty() => match t.validate() {
            Ok(()) => Some(t),
            Err(e) => {
                tx.refuse(format!("the translation does not validate: {e}"));
                None
            }
        },
        _ => None,
    };
    let status = if !tx.refusals.is_empty() {
        Status::Refused
    } else if tx.notes.is_empty() {
        Status::Clean
    } else {
        Status::Notes
    };
    (
        t,
        Report {
            status,
            notes: tx.notes,
            refusals: tx.refusals,
        },
    )
}
