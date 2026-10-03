//! Dokploy's template format, translated into isb's.
//!
//! A Dokploy template is a `docker-compose.yml` plus a `template.toml`:
//! `[variables]` (values with helpers such as `${password:32}`,
//! `${domain}`, `${base64:64}`, `${uuid}`, `${jwt:secret:payload}`),
//! `[config.env]` (written to the compose project's `.env`, so it feeds
//! both `${VAR}` interpolation and `env_file: .env`), `[[config.domains]]`
//! (service, port, host, path) and `[[config.mounts]]` (file contents the
//! compose file mounts from `../files/<path>`).
//!
//! The translation is strict: each compose service becomes one app, and
//! anything isb's model cannot express or that would weaken isolation
//! (privileged, capabilities, devices, host namespaces, host paths, the
//! docker socket, one volume shared by several services, one-shot jobs) is
//! refused with a reason. What is mapped with a change of meaning is listed
//! in the report's notes; nothing is dropped silently.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::{Value, json};
use serde_yaml_ng::Value as Y;

use super::{AppTemplate, FileTemplate, Template, VarKind, Variable};
use crate::app::Resources;

/// A template's metadata from Dokploy's `meta.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, serde::Deserialize)]
pub struct Meta {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub logo: Option<String>,
    #[serde(default)]
    pub links: BTreeMap<String, String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

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

struct Tx {
    notes: Vec<String>,
    refusals: Vec<String>,
    /// Native variables made so far.
    vars: Vec<Variable>,
    /// Dokploy variable name -> native name.
    names: BTreeMap<String, String>,
}

impl Tx {
    fn note(&mut self, s: impl Into<String>) {
        let s = s.into();
        if !self.notes.contains(&s) {
            self.notes.push(s);
        }
    }

    fn refuse(&mut self, s: impl Into<String>) {
        let s = s.into();
        if !self.refusals.contains(&s) {
            self.refusals.push(s);
        }
    }

    fn fresh_name(&self, base: &str) -> String {
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
fn var_name(s: &str) -> String {
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

fn lit(s: &str) -> String {
    s.replace('$', "$$")
}

fn secretish(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["pass", "secret", "token", "key", "salt", "private", "jwt"]
        .iter()
        .any(|w| n.contains(w))
}

/// One Dokploy helper or reference.
#[derive(Debug, Clone, PartialEq)]
enum Helper {
    Domain,
    Password(u32),
    Base64(u32),
    Hash(u32),
    Uuid,
    RandomPort,
    Email,
    Username,
    Timestamp {
        ms: bool,
        at: Option<String>,
    },
    JwtHex(u32),
    Jwt {
        secret: Option<String>,
        payload: Option<String>,
    },
}

fn parse_helper(inner: &str, vars: &BTreeMap<String, String>) -> Option<Helper> {
    let (head, arg) = match inner.split_once(':') {
        Some((h, a)) => (h, Some(a)),
        None => (inner, None),
    };
    let n = |d: u32| -> Option<u32> {
        match arg {
            None => Some(d),
            Some(a) => a.parse().ok().or(Some(d)),
        }
    };
    Some(match head {
        "domain" if arg.is_none() => Helper::Domain,
        "password" => Helper::Password(n(16)?),
        "base64" => Helper::Base64(n(32)?),
        "hash" => Helper::Hash(n(8)?),
        "uuid" if arg.is_none() => Helper::Uuid,
        "randomPort" if arg.is_none() => Helper::RandomPort,
        "email" if arg.is_none() => Helper::Email,
        "username" if arg.is_none() => Helper::Username,
        "timestamp" | "timestampms" => Helper::Timestamp {
            ms: true,
            at: arg.map(String::from),
        },
        "timestamps" => Helper::Timestamp {
            ms: false,
            at: arg.map(String::from),
        },
        "jwt" => match arg {
            None => Helper::Jwt {
                secret: None,
                payload: None,
            },
            Some(a) if a.len() <= 3 && a.chars().all(|c| c.is_ascii_digit()) => {
                Helper::JwtHex(a.parse().ok()?)
            }
            Some(a) => {
                let (s, p) = match a.split_once(':') {
                    Some((s, p)) => (s, Some(p)),
                    None => (a, None),
                };
                if !vars.contains_key(s) {
                    return None;
                }
                Helper::Jwt {
                    secret: Some(s.to_string()),
                    payload: p.filter(|p| vars.contains_key(*p)).map(String::from),
                }
            }
        },
        _ => return None,
    })
}

/// A Dokploy piece of text.
#[derive(Debug, Clone, PartialEq)]
enum DPart {
    Lit(String),
    Ref(String),
    Helper(Helper),
}

/// Split a Dokploy value into literal text, `${variable}` references and
/// `${helper}`s. Anything else in `${...}` stays literal, as Dokploy
/// leaves it.
fn dparse(s: &str, vars: &BTreeMap<String, String>) -> Vec<DPart> {
    let mut out = Vec::new();
    let mut rest = s;
    let mut lit_buf = String::new();
    while let Some(i) = rest.find("${") {
        let Some(end) = rest[i + 2..].find('}') else {
            break;
        };
        let inner = &rest[i + 2..i + 2 + end];
        lit_buf.push_str(&rest[..i]);
        let part = if vars.contains_key(inner) {
            Some(DPart::Ref(inner.to_string()))
        } else {
            parse_helper(inner, vars).map(DPart::Helper)
        };
        match part {
            Some(p) => {
                if !lit_buf.is_empty() {
                    out.push(DPart::Lit(std::mem::take(&mut lit_buf)));
                }
                out.push(p);
            }
            None => lit_buf.push_str(&rest[i..i + 2 + end + 1]),
        }
        rest = &rest[i + 2 + end + 1..];
    }
    lit_buf.push_str(rest);
    if !lit_buf.is_empty() {
        out.push(DPart::Lit(lit_buf));
    }
    out
}

impl Tx {
    /// A native variable for one helper, named after `base`.
    fn helper_var(&mut self, h: &Helper, base: &str) -> String {
        let name = self.fresh_name(base);
        let mut v = Variable {
            name: name.clone(),
            ..Default::default()
        };
        match h {
            Helper::Domain => v.kind = VarKind::Domain,
            Helper::Password(n) => {
                v.kind = VarKind::Password;
                v.length = Some(*n);
            }
            Helper::Base64(n) => {
                v.kind = VarKind::Base64;
                v.bytes = Some(*n);
            }
            Helper::Hash(n) | Helper::JwtHex(n) => {
                v.kind = VarKind::Hex;
                v.bytes = Some(*n);
                v.secret = Some(true);
            }
            Helper::Uuid => {
                v.kind = VarKind::Uuid;
                v.secret = Some(secretish(base));
            }
            Helper::RandomPort => v.kind = VarKind::Port,
            Helper::Username => v.kind = VarKind::Username,
            Helper::Email => {
                let u = self.fresh_name(&format!("{base}_user"));
                self.vars.push(Variable {
                    name: u.clone(),
                    kind: VarKind::Username,
                    ..Default::default()
                });
                v.kind = VarKind::Email;
                v.default = Some(format!("${{{u}}}@example.com"));
            }
            Helper::Timestamp { ms, at } => {
                v.kind = VarKind::Timestamp;
                v.unit = Some(if *ms { "ms" } else { "s" }.into());
                v.at = at.as_ref().map(|a| lit(a));
            }
            Helper::Jwt { secret, payload } => {
                v.kind = VarKind::Jwt;
                let secret = match secret {
                    Some(s) => self.names.get(s).cloned().unwrap_or_else(|| var_name(s)),
                    None => {
                        let s = self.fresh_name(&format!("{base}_secret"));
                        self.vars.push(Variable {
                            name: s.clone(),
                            kind: VarKind::Password,
                            length: Some(32),
                            ..Default::default()
                        });
                        s
                    }
                };
                v.jwt = Some(super::JwtSpec {
                    secret,
                    payload: payload.as_ref().map(|p| {
                        format!(
                            "${{{}}}",
                            self.names.get(p).cloned().unwrap_or_else(|| var_name(p))
                        )
                    }),
                });
            }
        }
        self.vars.push(v);
        name
    }

    /// A Dokploy value as a native expression; inline helpers become
    /// variables named after `base`.
    fn expr(&mut self, s: &str, base: &str, dvars: &BTreeMap<String, String>) -> String {
        let mut out = String::new();
        for p in dparse(s, dvars) {
            match p {
                DPart::Lit(l) => out.push_str(&lit(&l)),
                DPart::Ref(r) => {
                    let n = self.names.get(&r).cloned().unwrap_or_else(|| var_name(&r));
                    out.push_str(&format!("${{{n}}}"));
                }
                DPart::Helper(h) => {
                    let n = self.helper_var(&h, base);
                    out.push_str(&format!("${{{n}}}"));
                }
            }
        }
        out
    }
}

fn scalar(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => s.clone(),
        toml::Value::Integer(i) => i.to_string(),
        toml::Value::Float(f) => f.to_string(),
        toml::Value::Boolean(b) => b.to_string(),
        other => format!("{other:?}"),
    }
}

fn yscalar(v: &Y) -> Option<String> {
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

/// Compose `${VAR}` interpolation, against the template's `.env`, written
/// as a native expression.
#[expect(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
fn interpolate(
    s: &str,
    env: &BTreeMap<String, String>,
    unset: &mut BTreeSet<String>,
) -> std::result::Result<String, String> {
    let mut out = String::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'$' {
            let c = s[i..].chars().next().unwrap_or('\0');
            out.push_str(&lit(&c.to_string()));
            i += c.len_utf8();
            continue;
        }
        match b.get(i + 1) {
            Some(b'$') => {
                out.push_str("$$");
                i += 2;
            }
            Some(b'{') => {
                // Find the matching brace (defaults may nest ${...}).
                let mut depth = 0;
                let mut j = i + 1;
                let mut end = None;
                while j < b.len() {
                    match b[j] {
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                end = Some(j);
                                break;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
                let end = end.ok_or_else(|| format!("unterminated ${{ in {s:?}"))?;
                let inner = &s[i + 2..end];
                let name_end = inner
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .unwrap_or(inner.len());
                let (name, op) = inner.split_at(name_end);
                if name.is_empty() {
                    return Err(format!("${{{inner}}} is not a variable"));
                }
                let set = env.get(name);
                let empty = set.is_none_or(|v| v.is_empty());
                let pick = |alt: &str, unset: &mut BTreeSet<String>| interpolate(alt, env, unset);
                let val = if let Some(d) = op.strip_prefix(":-") {
                    if empty {
                        pick(d, unset)?
                    } else {
                        set.cloned().unwrap_or_default()
                    }
                } else if let Some(d) = op.strip_prefix('-') {
                    match set {
                        Some(v) => v.clone(),
                        None => pick(d, unset)?,
                    }
                } else if let Some(a) = op.strip_prefix(":+") {
                    if empty {
                        String::new()
                    } else {
                        pick(a, unset)?
                    }
                } else if let Some(a) = op.strip_prefix('+') {
                    if set.is_some() {
                        pick(a, unset)?
                    } else {
                        String::new()
                    }
                } else if op.starts_with(":?") || op.starts_with('?') || op.is_empty() {
                    match set {
                        Some(v) => v.clone(),
                        None => {
                            unset.insert(name.to_string());
                            String::new()
                        }
                    }
                } else {
                    return Err(format!("${{{inner}}}: unsupported interpolation"));
                };
                out.push_str(&val);
                i = end + 1;
            }
            Some(c) if c.is_ascii_alphabetic() || *c == b'_' => {
                let start = i + 1;
                let mut j = start;
                while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                    j += 1;
                }
                let name = &s[start..j];
                match env.get(name) {
                    Some(v) => out.push_str(v),
                    None => {
                        unset.insert(name.to_string());
                    }
                }
                i = j;
            }
            _ => {
                out.push_str("$$");
                i += 1;
            }
        }
    }
    Ok(out)
}

/// Rewrite references to the template's services (compose names) in a
/// native expression's literal text to `${host:KEY}`: after `//` or `@`,
/// before `:<port>`, or the whole value when `whole`. Returns the names
/// rewritten.
fn rewrite_hosts(expr: &str, services: &[(String, String)], whole: bool) -> (String, Vec<String>) {
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

fn hostish_key(k: &str) -> bool {
    let k = k.to_ascii_uppercase();
    [
        "HOST", "SERVER", "ADDR", "ENDPOINT", "URL", "URI", "DSN", "BROKER", "NODES", "UPSTREAM",
        "BACKEND",
    ]
    .iter()
    .any(|w| k.contains(w))
}

/// A parsed `template.toml`.
struct Toml {
    vars: BTreeMap<String, String>,
    env: Vec<(String, String)>,
    domains: Vec<(String, u16, String, String)>,
    mounts: BTreeMap<String, String>,
}

#[expect(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
fn parse_toml(text: &str, tx: &mut Tx) -> Option<Toml> {
    let t: toml::Table = match text.parse() {
        Ok(t) => t,
        Err(e) => {
            tx.refuse(format!("template.toml does not parse: {e}"));
            return None;
        }
    };
    let mut out = Toml {
        vars: BTreeMap::new(),
        env: Vec::new(),
        domains: Vec::new(),
        mounts: BTreeMap::new(),
    };
    if let Some(v) = t.get("variables").and_then(|v| v.as_table()) {
        for (k, v) in v {
            out.vars.insert(k.clone(), scalar(v));
        }
    }
    let cfg = t.get("config").and_then(|v| v.as_table());
    for k in t.keys() {
        if k != "variables" && k != "config" {
            tx.note(format!(
                "template.toml section [{k}] is not used by Dokploy or isb"
            ));
        }
    }
    let Some(cfg) = cfg else { return Some(out) };
    match cfg.get("env") {
        Some(toml::Value::Table(e)) => {
            for (k, v) in e {
                out.env.push((k.clone(), scalar(v)));
            }
        }
        Some(toml::Value::Array(a)) => {
            for item in a {
                match item {
                    toml::Value::String(s) => match s.split_once('=') {
                        Some((k, v)) => out.env.push((k.trim().to_string(), v.to_string())),
                        None => out.env.push((s.trim().to_string(), String::new())),
                    },
                    toml::Value::Table(t) => {
                        for (k, v) in t {
                            out.env.push((k.clone(), scalar(v)));
                        }
                    }
                    other => tx.refuse(format!("config.env entry {other:?} is not KEY=VALUE")),
                }
            }
        }
        Some(other) => tx.refuse(format!("config.env is a {}", other.type_str())),
        None => {}
    }
    if let Some(ds) = cfg.get("domains").and_then(|v| v.as_array()) {
        for d in ds {
            let get = |k: &str| d.get(k).map(scalar);
            let Some(svc) = get("serviceName") else {
                tx.refuse("a config.domains entry lacks serviceName");
                continue;
            };
            let host = get("host").filter(|h| !h.is_empty()).unwrap_or_else(|| {
                tx.note(format!(
                    "config.domains for {svc} has no host; it gets a generated one"
                ));
                "${domain}".into()
            });
            let port = match d.get("port").and_then(|p| p.as_integer()) {
                Some(p) if (1..=65535).contains(&p) => p as u16,
                _ => {
                    tx.refuse(format!("config.domains for {svc}: no valid port"));
                    continue;
                }
            };
            let path = get("path")
                .filter(|p| !p.is_empty())
                .unwrap_or_else(|| "/".into());
            for k in d
                .as_table()
                .map(|t| t.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
            {
                if !matches!(k.as_str(), "serviceName" | "host" | "port" | "path") {
                    tx.note(format!("config.domains key {k} is ignored"));
                }
            }
            out.domains.push((svc, port, host, path));
        }
    }
    if let Some(ms) = cfg.get("mounts").and_then(|v| v.as_array()) {
        for m in ms {
            let path = m.get("filePath").map(scalar);
            let content = m.get("content").map(scalar);
            match (path, content) {
                (Some(p), Some(c)) => {
                    out.mounts.insert(
                        p.trim_start_matches("./")
                            .trim_start_matches('/')
                            .to_string(),
                        c,
                    );
                }
                _ => tx.refuse("a config.mounts entry lacks filePath or content"),
            }
        }
    }
    for k in cfg.keys() {
        if !matches!(k.as_str(), "env" | "domains" | "mounts" | "isolated") {
            tx.note(format!("config.{k} is ignored"));
        }
    }
    Some(out)
}

/// The compose keys that cannot be honoured without weakening isolation
/// or changing what the app is.
const REFUSED_KEYS: &[(&str, &str)] = &[
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
const NOTED_KEYS: &[(&str, &str)] = &[
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
const IGNORED_KEYS: &[&str] = &[
    "image",
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

fn ymap(v: &Y) -> Option<&serde_yaml_ng::Mapping> {
    v.as_mapping()
}

fn yget<'a>(m: &'a serde_yaml_ng::Mapping, k: &str) -> Option<&'a Y> {
    m.get(Y::String(k.into()))
}

/// Entries of a list-or-map (`environment`, `labels`) as pairs; a bare
/// list entry has no value.
fn pairs(v: &Y) -> Vec<(String, Option<String>)> {
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

fn words(v: &Y) -> Option<Vec<String>> {
    match v {
        Y::String(s) => crate::flex::split_words(s).ok(),
        Y::Sequence(s) => s.iter().map(yscalar).collect(),
        Y::Null => Some(vec![]),
        _ => None,
    }
}

/// A Traefik rule's hosts and path prefix, if it is only that.
fn traefik_rule(rule: &str) -> Option<(Vec<String>, Option<String>)> {
    let mut hosts = Vec::new();
    let mut path = None;
    if rule.contains("||") || rule.contains('!') {
        return None;
    }
    for part in rule.split("&&") {
        let part = part
            .trim()
            .trim_start_matches('(')
            .trim_end_matches(')')
            .trim();
        let (f, args) = part.split_once('(')?;
        let args: Vec<String> = args
            .trim_end_matches(')')
            .split(',')
            .map(|a| a.trim().trim_matches(['`', '"', '\'']).to_string())
            .filter(|a| !a.is_empty())
            .collect();
        match f.trim() {
            "Host" => hosts.extend(args),
            "PathPrefix" | "Path" if args.len() == 1 && path.is_none() => {
                path = Some(args[0].clone())
            }
            _ => return None,
        }
    }
    (!hosts.is_empty()).then_some((hosts, path))
}

/// Domains from a service's Traefik labels (already interpolated).
#[expect(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
fn traefik_domains(
    svc: &str,
    labels: &[(String, String)],
    tx: &mut Tx,
) -> Vec<serde_json::Map<String, Value>> {
    let get = |k: &str| labels.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone());
    let enabled = get("traefik.enable").is_none_or(|v| v != "false");
    let mut routers: BTreeSet<String> = BTreeSet::new();
    let mut ports: BTreeMap<String, u16> = BTreeMap::new();
    let mut middlewares: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for (k, v) in labels {
        let Some(rest) = k.strip_prefix("traefik.") else {
            continue;
        };
        if rest.starts_with("tcp.") || rest.starts_with("udp.") {
            tx.refuse(format!("{svc}: Traefik TCP/UDP routing ({k})"));
        } else if let Some(r) = rest.strip_prefix("http.routers.") {
            if let Some((name, _)) = r.split_once('.') {
                routers.insert(name.to_string());
            }
        } else if let Some(s) = rest.strip_prefix("http.services.") {
            if let Some((name, field)) = s.split_once('.') {
                if field == "loadbalancer.server.port" {
                    if let Ok(p) = v.parse() {
                        ports.insert(name.to_string(), p);
                    }
                }
            }
        } else if let Some(m) = rest.strip_prefix("http.middlewares.") {
            if let Some((name, field)) = m.split_once('.') {
                middlewares
                    .entry(name.to_string())
                    .or_default()
                    .push((field.to_string(), v.clone()));
            }
        }
    }
    if !enabled {
        return vec![];
    }
    let mut out = Vec::new();
    for r in routers {
        let field = |f: &str| get(&format!("traefik.http.routers.{r}.{f}"));
        let Some(rule) = field("rule") else { continue };
        let Some((hosts, path)) = traefik_rule(&rule) else {
            tx.refuse(format!(
                "{svc}: Traefik rule {rule:?} is more than Host(...) && PathPrefix(...)"
            ));
            continue;
        };
        let port = field("service")
            .and_then(|s| ports.get(&s).copied())
            .or_else(|| (ports.len() == 1).then(|| *ports.values().next().expect("one")));
        let Some(port) = port else {
            tx.refuse(format!(
                "{svc}: Traefik router {r} has no loadbalancer.server.port to send to"
            ));
            continue;
        };
        let entry = field("entrypoints").unwrap_or_default();
        // HTTPS unless every entrypoint is plain `web`.
        let web_only = !entry.is_empty() && entry.split(',').all(|e| e.trim() == "web");
        let https = !web_only || field("tls").is_some_and(|t| t == "true");
        let mut strip = false;
        for m in field("middlewares")
            .unwrap_or_default()
            .split(',')
            .map(|m| m.trim().split('@').next().unwrap_or("").to_string())
            .filter(|m| !m.is_empty())
        {
            let fields = middlewares.get(&m).cloned().unwrap_or_default();
            let kind = fields
                .first()
                .map(|(f, _)| f.split('.').next().unwrap_or("").to_ascii_lowercase());
            match kind.as_deref() {
                Some("stripprefix") => strip = true,
                Some("redirectscheme") => {
                    tx.note(format!(
                        "{svc}: Traefik redirectscheme is isb's default for an https domain"
                    ));
                }
                Some(k) => tx.refuse(format!("{svc}: Traefik middleware {m} ({k})")),
                None => tx.refuse(format!(
                    "{svc}: Traefik middleware {m} is defined elsewhere"
                )),
            }
        }
        for h in hosts {
            let mut d = serde_json::Map::new();
            d.insert("host".into(), json!(h));
            d.insert("port".into(), json!(port));
            if let Some(p) = &path {
                d.insert("path".into(), json!(p));
            }
            if !https {
                d.insert("https".into(), json!(false));
            }
            if strip {
                d.insert("strip_prefix".into(), json!(true));
            }
            out.push(d);
        }
    }
    if !out.is_empty() {
        tx.note(format!("{svc}: Traefik labels are mapped to domains"));
    }
    out
}

/// Translate one Dokploy template. `None` with refusals when it cannot run
/// on isb.
pub fn translate(meta: &Meta, compose: &str, toml_text: &str) -> (Option<Template>, Report) {
    let mut tx = Tx {
        notes: vec![],
        refusals: vec![],
        vars: vec![],
        names: BTreeMap::new(),
    };
    let t = translate_inner(meta, compose, toml_text, &mut tx);
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

#[expect(
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    clippy::excessive_nesting,
    reason = "predates the lint ratchet; split it when next changed"
)]
fn translate_inner(meta: &Meta, compose: &str, toml_text: &str, tx: &mut Tx) -> Option<Template> {
    let toml = parse_toml(toml_text, tx)?;
    // Variables first: each Dokploy variable becomes a native one.
    for k in toml.vars.keys() {
        let n = tx.fresh_name(k);
        tx.names.insert(k.clone(), n.clone());
        // Reserve the name now so helpers do not take it.
        tx.vars.push(Variable {
            name: n,
            ..Default::default()
        });
    }
    let dvars = toml.vars.clone();
    for (k, raw) in &toml.vars {
        let native = tx.names[k].clone();
        let parts = dparse(raw, &dvars);
        let v = match parts.as_slice() {
            [DPart::Helper(h)] => {
                // The variable is the helper: make it under its own name.
                tx.vars.retain(|v| v.name != native);
                let made = tx.helper_var(h, &native);
                debug_assert_eq!(made, native);
                if matches!(h, Helper::Uuid) {
                    if let Some(v) = tx.vars.iter_mut().find(|v| v.name == made) {
                        v.secret = Some(secretish(k));
                    }
                }
                continue;
            }
            [] => Variable {
                name: native.clone(),
                default: Some(String::new()),
                ..Default::default()
            },
            [DPart::Lit(l)] => Variable {
                name: native.clone(),
                default: Some(lit(l)),
                secret: Some(secretish(k)),
                ..Default::default()
            },
            _ => {
                let e = tx.expr(raw, &native, &dvars);
                Variable {
                    name: native.clone(),
                    default: Some(e),
                    ..Default::default()
                }
            }
        };
        let i = tx
            .vars
            .iter()
            .position(|x| x.name == native)
            .expect("reserved");
        tx.vars[i] = v;
    }
    // The .env: each entry a native expression.
    let mut env: BTreeMap<String, String> = BTreeMap::new();
    let mut env_order: Vec<String> = Vec::new();
    for (k, v) in &toml.env {
        let e = tx.expr(v, &format!("env_{k}"), &dvars);
        if !env.contains_key(k) {
            env_order.push(k.clone());
        }
        env.insert(k.clone(), e);
    }
    let mounts: BTreeMap<String, String> = toml
        .mounts
        .iter()
        .map(|(p, c)| (p.clone(), tx.expr(c, "file", &dvars)))
        .collect();

    // The compose file.
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
    let Some(top) = ymap(&doc) else {
        tx.refuse("docker-compose.yml is not a mapping");
        return None;
    };
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
    let Some(services) = yget(top, "services").and_then(ymap) else {
        tx.refuse("docker-compose.yml has no services");
        return None;
    };
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
            for k in ["hostname", "container_name"] {
                if let Some(h) = yget(m, k).and_then(yscalar) {
                    if h != name && !h.contains('$') {
                        aliases.push((h, key.clone()));
                    }
                }
            }
            if let Some(Y::Mapping(nets)) = yget(m, "networks") {
                for (_, n) in nets {
                    if let Some(Y::Sequence(al)) = ymap(n).and_then(|n| yget(n, "aliases")) {
                        for a in al.iter().filter_map(yscalar) {
                            aliases.push((a, key.clone()));
                        }
                    }
                }
            }
        }
    }
    // Longest names first, so `db-replica` is not matched as `db`.
    aliases.sort_by_key(|a| std::cmp::Reverse(a.0.len()));
    // A volume used by two services cannot be two apps' own volumes.
    let mut vol_users: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    let mut unset = BTreeSet::new();
    let mut apps = Vec::new();
    for (name, s) in services {
        let name = yscalar(name).unwrap_or_default();
        let Some(key) = keys.get(&name).cloned() else {
            continue;
        };
        let Some(m) = ymap(s) else {
            tx.refuse(format!("service {name} is not a mapping"));
            continue;
        };
        let mut ip = |s: &str, tx: &mut Tx| -> String {
            match interpolate(s, &env, &mut unset) {
                Ok(v) => v,
                Err(e) => {
                    tx.refuse(format!("{name}: {e}"));
                    String::new()
                }
            }
        };
        for (k, v) in m {
            let k = yscalar(k).unwrap_or_default();
            if k.starts_with("x-") || IGNORED_KEYS.contains(&k.as_str()) {
                continue;
            }
            if let Some((_, why)) = REFUSED_KEYS.iter().find(|(r, _)| *r == k) {
                let harmless = match k.as_str() {
                    "privileged" => v.as_bool() == Some(false),
                    "pid" | "ipc" | "uts" | "userns_mode" | "cgroup" => yscalar(v)
                        .is_some_and(|s| s.is_empty() || s == "private" || s == "shareable"),
                    "devices" | "cap_add" | "sysctls" | "extra_hosts" | "dns" | "dns_search" => {
                        matches!(v, Y::Sequence(s) if s.is_empty())
                            || matches!(v, Y::Mapping(m) if m.is_empty())
                            || v.is_null()
                    }
                    _ => false,
                };
                if !harmless {
                    tx.refuse(format!("{name}: {why}"));
                }
            } else if let Some((_, why)) = NOTED_KEYS.iter().find(|(r, _)| *r == k) {
                tx.note(format!("{name}: {why}"));
            } else {
                tx.note(format!("{name}: compose key {k} is ignored"));
            }
        }
        // Image.
        let image = match yget(m, "image").and_then(yscalar) {
            Some(i) => image_ref(&ip(&i, tx)),
            None => {
                if yget(m, "build").is_none() {
                    tx.refuse(format!("{name}: no image"));
                }
                continue;
            }
        };
        // Network mode.
        if let Some(nm) = yget(m, "network_mode").and_then(yscalar) {
            if !matches!(nm.as_str(), "" | "bridge" | "default") {
                tx.refuse(format!("{name}: network_mode {nm}"));
            }
        }
        // Restart: a one-shot job would be restarted for ever.
        match yget(m, "restart").and_then(yscalar).as_deref() {
            Some("no") | Some("\"no\"") => {
                tx.refuse(format!(
                    "{name}: restart \"no\" (a one-shot job; apps are kept running)"
                ));
            }
            Some("on-failure") => tx.note(format!(
                "{name}: restart on-failure becomes always (apps are kept running)"
            )),
            None => tx.note(format!("{name}: no restart policy; isb keeps it running")),
            _ => {}
        }
        if let Some(sc) = yget(m, "scale").and_then(yscalar) {
            if sc == "0" {
                tx.note(format!("{name}: scale 0; it is not deployed"));
                continue;
            }
        }
        // Environment: env_file .env first, environment over it.
        let mut senv: BTreeMap<String, String> = BTreeMap::new();
        if let Some(ef) = yget(m, "env_file") {
            let files: Vec<String> = match ef {
                Y::String(s) => vec![s.clone()],
                Y::Sequence(s) => s
                    .iter()
                    .filter_map(|e| {
                        yscalar(e)
                            .or_else(|| ymap(e).and_then(|m| yget(m, "path")).and_then(yscalar))
                    })
                    .collect(),
                _ => vec![],
            };
            for f in files {
                if matches!(f.as_str(), ".env" | "./.env") {
                    for k in &env_order {
                        senv.insert(k.clone(), env[k].clone());
                    }
                } else {
                    tx.refuse(format!("{name}: env_file {f} (only Dokploy's .env exists)"));
                }
            }
        }
        if let Some(e) = yget(m, "environment") {
            for (k, v) in pairs(e) {
                match v {
                    Some(v) => {
                        let x = ip(&v, tx);
                        senv.insert(k, x);
                    }
                    None => match env.get(&k) {
                        Some(x) => {
                            senv.insert(k, x.clone());
                        }
                        None => tx.note(format!(
                            "{name}: environment {k} has no value (it is left out, as docker does)"
                        )),
                    },
                }
            }
        }
        // Volumes.
        let mut volumes = Vec::new();
        let mut files = Vec::new();
        let mut anon = 0;
        for v in yget(m, "volumes")
            .and_then(Y::as_sequence)
            .cloned()
            .unwrap_or_default()
        {
            let (kind, source, target, ro) = match &v {
                Y::String(s) => {
                    let s = ip(s, tx);
                    let parts: Vec<&str> = s.split(':').collect();
                    match parts.as_slice() {
                        [t] => ("anon", String::new(), t.to_string(), false),
                        [src, t] => ("auto", src.to_string(), t.to_string(), false),
                        [src, t, o] => (
                            "auto",
                            src.to_string(),
                            t.to_string(),
                            o.split(',').any(|x| x == "ro"),
                        ),
                        _ => {
                            tx.refuse(format!("{name}: volume {s:?} does not parse"));
                            continue;
                        }
                    }
                }
                Y::Mapping(vm) => {
                    let raw = |k: &str| yget(vm, k).and_then(yscalar);
                    let t = raw("type").unwrap_or_else(|| "volume".into());
                    let src = raw("source").map(|x| ip(&x, tx)).unwrap_or_default();
                    let tgt = raw("target").map(|x| ip(&x, tx)).unwrap_or_default();
                    let ro = yget(vm, "read_only").and_then(Y::as_bool).unwrap_or(false);
                    match t.as_str() {
                        "tmpfs" => {
                            tx.note(format!(
                                "{name}: tmpfs {tgt} is not made; it is on the root filesystem"
                            ));
                            continue;
                        }
                        "bind" | "volume" => {}
                        other => {
                            tx.refuse(format!("{name}: a {other} mount"));
                            continue;
                        }
                    }
                    let kind = if src.is_empty() {
                        "anon"
                    } else if t == "bind" {
                        "bind"
                    } else {
                        "auto"
                    };
                    (kind, src, tgt, ro)
                }
                _ => {
                    tx.refuse(format!("{name}: a volume entry does not parse"));
                    continue;
                }
            };
            if target.contains('$') || !target.starts_with('/') {
                tx.refuse(format!(
                    "{name}: mount target {target:?} is not an absolute path"
                ));
                continue;
            }
            let is_path = source.starts_with('/')
                || source.starts_with('.')
                || source.starts_with('~')
                || kind == "bind";
            if kind == "anon" {
                anon += 1;
                let n = format!("anon-{anon}");
                tx.note(format!(
                    "{name}: anonymous volume {target} becomes the named volume {n}"
                ));
                volumes.push(format!("{n}:{target}{}", if ro { ":ro" } else { "" }));
            } else if !is_path {
                let vn = key_name(&source);
                vol_users
                    .entry(source.clone())
                    .or_default()
                    .insert(name.clone());
                if !declared_vols.contains_key(Y::String(source.clone())) {
                    tx.note(format!(
                        "{name}: volume {source} is not declared at the top level"
                    ));
                }
                volumes.push(format!("{vn}:{target}{}", if ro { ":ro" } else { "" }));
            } else if let Some(rel) = source
                .strip_prefix("../files/")
                .or_else(|| source.strip_prefix("./files/"))
                .or_else(|| source.strip_prefix("files/"))
            {
                let rel = rel.trim_end_matches('/');
                let mut found = false;
                for (p, content) in &mounts {
                    let tgt = if p == rel {
                        Some(target.clone())
                    } else {
                        p.strip_prefix(&format!("{rel}/"))
                            .map(|sub| format!("{}/{sub}", target.trim_end_matches('/')))
                    };
                    if let Some(tgt) = tgt {
                        found = true;
                        files.push(FileTemplate {
                            path: tgt,
                            content: content.clone(),
                            mode: None,
                        });
                    }
                }
                if !found {
                    // A directory Dokploy makes in the deployment's files:
                    // persistent storage, as a named volume.
                    let vn = key_name(rel);
                    vol_users
                        .entry(format!("files/{vn}"))
                        .or_default()
                        .insert(name.clone());
                    tx.note(format!(
                        "{name}: {source} (no content given) becomes the named volume {vn}"
                    ));
                    volumes.push(format!("{vn}:{target}{}", if ro { ":ro" } else { "" }));
                }
            } else if source.contains("docker.sock") {
                tx.refuse(format!("{name}: the docker socket ({source})"));
            } else if matches!(
                source.trim_end_matches('/'),
                "/etc/localtime" | "/etc/timezone" | "/usr/share/zoneinfo"
            ) {
                // The host's clock settings: not mounted (no host path is),
                // which only changes the default time zone.
                tx.note(format!(
                    "{name}: the host's {source} is not mounted; the app keeps its image's time zone (set TZ)"
                ));
            } else if source.starts_with('/') || source.starts_with('~') {
                tx.refuse(format!("{name}: host path {source}"));
            } else {
                // A path in the compose project: per deployment, persistent.
                let vn = key_name(source.trim_start_matches("./").trim_start_matches("../"));
                vol_users
                    .entry(format!("./{vn}"))
                    .or_default()
                    .insert(name.clone());
                tx.note(format!(
                    "{name}: bind {source} becomes the named volume {vn} (it starts as a copy of the image's {target})"
                ));
                volumes.push(format!("{vn}:{target}{}", if ro { ":ro" } else { "" }));
            }
        }
        // Ports.
        let mut ports = Vec::new();
        for p in yget(m, "ports")
            .and_then(Y::as_sequence)
            .cloned()
            .unwrap_or_default()
        {
            let (published, target, proto) = match &p {
                Y::Mapping(pm) => (
                    yget(pm, "published").and_then(yscalar).map(|x| ip(&x, tx)),
                    yget(pm, "target").and_then(yscalar).unwrap_or_default(),
                    yget(pm, "protocol")
                        .and_then(yscalar)
                        .unwrap_or_else(|| "tcp".into()),
                ),
                other => {
                    let s = ip(&yscalar(other).unwrap_or_default(), tx);
                    let (s, proto) = match s.split_once('/') {
                        Some((a, b)) => (a.to_string(), b.to_string()),
                        None => (s, "tcp".into()),
                    };
                    let parts: Vec<&str> = s.rsplitn(2, ':').collect();
                    match parts.as_slice() {
                        [t] => (None, t.to_string(), proto),
                        [t, rest] => (
                            Some(rest.rsplit(':').next().unwrap_or(rest).to_string()),
                            t.to_string(),
                            proto,
                        ),
                        _ => (None, String::new(), proto),
                    }
                }
            };
            let ok_port = |s: &str| s.parse::<u16>().is_ok_and(|p| p > 0);
            match published.filter(|x| !x.is_empty()) {
                None => tx.note(format!(
                    "{name}: port {target} without a host port is not published (docker would pick a random one)"
                )),
                Some(_) if proto != "tcp" => {
                    tx.refuse(format!("{name}: a published {proto} port ({target}/{proto})"));
                }
                Some(h) if ok_port(&h) && ok_port(&target) => {
                    tx.note(format!(
                        "{name}: port {h}:{target} is published on 127.0.0.1 only (isb's default)"
                    ));
                    ports.push(format!("127.0.0.1:{h}:{target}"));
                }
                Some(h) => tx.refuse(format!("{name}: port {h}:{target} (ranges are not supported)")),
            }
        }
        // Command line.
        let entrypoint = yget(m, "entrypoint").map(words);
        let cmd = yget(m, "command").map(words);
        let mut command = None;
        let mut args = None;
        match (entrypoint, cmd) {
            (Some(None), _) | (_, Some(None)) => {
                tx.refuse(format!("{name}: command/entrypoint does not parse"));
            }
            (Some(Some(e)), c) => {
                let mut line: Vec<String> = e.iter().map(|w| ip(w, tx)).collect();
                if let Some(Some(c)) = c {
                    line.extend(c.iter().map(|w| ip(w, tx)));
                }
                if !line.is_empty() {
                    command = Some(json!(line));
                }
            }
            (None, Some(Some(c))) => {
                args = Some(json!(c.iter().map(|w| ip(w, tx)).collect::<Vec<_>>()));
            }
            (None, None) => {}
        }
        // Health check.
        let mut healthcheck = None;
        if let Some(Y::Mapping(h)) = yget(m, "healthcheck") {
            let disabled = yget(h, "disable").and_then(Y::as_bool) == Some(true);
            let test = yget(h, "test");
            let none = matches!(test, Some(Y::Sequence(s)) if s.first().and_then(yscalar).as_deref() == Some("NONE"));
            if !disabled && !none {
                let mut out = serde_json::Map::new();
                match test {
                    Some(Y::String(s)) => {
                        out.insert("test".into(), json!(ip(s, tx)));
                    }
                    Some(Y::Sequence(s)) => {
                        let w: Vec<String> =
                            s.iter().filter_map(yscalar).map(|x| ip(&x, tx)).collect();
                        out.insert("test".into(), json!(w));
                    }
                    _ => {}
                }
                for f in ["interval", "timeout", "start_period", "start_interval"] {
                    if let Some(v) = yget(h, f).and_then(yscalar) {
                        out.insert(f.into(), json!(ip(&v, tx)));
                    }
                }
                if let Some(r) = yget(h, "retries").and_then(Y::as_u64) {
                    out.insert("retries".into(), json!(r));
                }
                if out.contains_key("test") {
                    healthcheck = Some(Value::Object(out));
                }
            }
        }
        // Dependencies.
        let mut depends = Vec::new();
        match yget(m, "depends_on") {
            Some(Y::Sequence(s)) => depends.extend(s.iter().filter_map(yscalar)),
            Some(Y::Mapping(dm)) => {
                for (d, c) in dm {
                    let d = yscalar(d).unwrap_or_default();
                    let cond = ymap(c).and_then(|c| yget(c, "condition")).and_then(yscalar);
                    if cond.as_deref() == Some("service_completed_successfully") {
                        tx.refuse(format!(
                            "{name}: waits for {d} to complete (a one-shot job)"
                        ));
                    }
                    depends.push(d);
                }
            }
            _ => {}
        }
        if let Some(Y::Sequence(l)) = yget(m, "links") {
            tx.note(format!(
                "{name}: links become dependencies; names resolve as <app>.<stack>"
            ));
            for x in l.iter().filter_map(yscalar) {
                depends.push(x.split(':').next().unwrap_or(&x).to_string());
            }
        }
        let mut dep_keys = Vec::new();
        for d in depends {
            match keys.get(&d) {
                Some(k) if !dep_keys.contains(k) && k != &key => dep_keys.push(k.clone()),
                Some(_) => {}
                None => tx.note(format!("{name}: depends on {d}, which is not deployed")),
            }
        }
        // User.
        let user = yget(m, "user")
            .and_then(yscalar)
            .map(|u| ip(&u, tx))
            .and_then(|u| {
                let (a, b) = u.split_once(':').unwrap_or((&u, ""));
                let num = |s: &str| s.is_empty() || s.parse::<u32>().is_ok();
                if a == "root" && (b.is_empty() || b == "root") {
                    Some("0".to_string())
                } else if num(a) && num(b) && !a.is_empty() {
                    Some(u.clone())
                } else {
                    tx.refuse(format!(
                        "{name}: user {u:?} is a name; an OCI image's user must be numeric here"
                    ));
                    None
                }
            });
        let working_dir = yget(m, "working_dir").and_then(yscalar).map(|w| ip(&w, tx));
        // Resources and replicas.
        let mut replicas = None;
        let mut res = Resources::default();
        if let Some(Y::Mapping(d)) = yget(m, "deploy") {
            if let Some(r) = yget(d, "replicas").and_then(Y::as_u64) {
                replicas = Some(r as u32);
            }
            if yget(d, "mode").and_then(yscalar).as_deref() == Some("global") {
                tx.note(format!("{name}: deploy mode global runs one replica"));
            }
            if let Some(lim) = yget(d, "resources")
                .and_then(ymap)
                .and_then(|r| yget(r, "limits"))
                .and_then(ymap)
            {
                if let Some(c) = yget(lim, "cpus").and_then(yscalar) {
                    res.cpus = Some(ip(&c, tx));
                }
                if let Some(mm) = yget(lim, "memory").and_then(yscalar) {
                    res.memory = Some(ip(&mm, tx));
                }
            }
            for k in d.keys().filter_map(yscalar) {
                if !matches!(k.as_str(), "replicas" | "resources" | "mode") {
                    tx.note(format!("{name}: deploy.{k} is ignored"));
                }
            }
        }
        if let Some(c) = yget(m, "cpus").and_then(yscalar) {
            res.cpus = Some(ip(&c, tx));
        }
        if let Some(mm) = yget(m, "mem_limit").and_then(yscalar) {
            res.memory = Some(ip(&mm, tx));
        }
        if let Some(c) = res.cpus.clone().filter(|c| !c.contains("${")) {
            match c.parse::<f64>() {
                Ok(f) if f > 0.0 => {
                    let whole = f.ceil() as u64;
                    if (whole as f64 - f).abs() > f64::EPSILON {
                        tx.note(format!(
                            "{name}: cpus {c} is rounded up to {whole} (isb pins whole CPUs)"
                        ));
                    }
                    res.cpus = Some(whole.to_string());
                }
                _ => {
                    tx.refuse(format!("{name}: cpus {c:?}"));
                    res.cpus = None;
                }
            }
        }
        if let Some(mm) = &res.memory {
            // docker's lower-case units; isb takes 512m, 2g, 1GiB.
            let t = mm.trim().to_ascii_lowercase();
            let t = t.trim_end_matches('b').to_string();
            res.memory = Some(t);
        }
        // Traefik labels.
        let labels: Vec<(String, String)> = yget(m, "labels")
            .map(pairs)
            .unwrap_or_default()
            .into_iter()
            .map(|(k, v)| {
                let v = v.unwrap_or_default();
                (k, ip(&v, tx))
            })
            .collect();
        let mut domains = traefik_domains(&name, &labels, tx);
        if labels.iter().any(|(k, _)| !k.starts_with("traefik.")) {
            tx.note(format!("{name}: its labels are not applied"));
        }
        for (svc, port, host, path) in &toml.domains {
            if svc == &name {
                let mut d = serde_json::Map::new();
                d.insert("host".into(), json!(tx.expr(host, "domain", &dvars)));
                d.insert("port".into(), json!(port));
                if path != "/" {
                    d.insert("path".into(), json!(path));
                }
                domains.push(d);
            }
        }
        // Domains on the same host and path once.
        let mut seen = BTreeSet::new();
        domains.retain(|d| {
            seen.insert((
                d.get("host").cloned().unwrap_or_default().to_string(),
                d.get("path").cloned().unwrap_or_default().to_string(),
            ))
        });
        let port = domains
            .first()
            .and_then(|d| d.get("port"))
            .and_then(Value::as_u64)
            .map(|p| p as u16);
        // Other services named in values: rewrite to isb's service names.
        let others: Vec<(String, String)> =
            aliases.iter().filter(|(_, k)| k != &key).cloned().collect();
        let mut rewritten = BTreeSet::new();
        let mut fix = |e: &str, whole: bool| -> String {
            let (out, hits) = rewrite_hosts(e, &others, whole);
            rewritten.extend(hits);
            out
        };
        let senv: BTreeMap<String, String> = senv
            .into_iter()
            .map(|(k, v)| {
                let w = hostish_key(&k);
                (k, fix(&v, w))
            })
            .collect();
        let fix_args = |v: &Option<Value>, fix: &mut dyn FnMut(&str, bool) -> String| {
            v.as_ref().map(|v| match v {
                Value::Array(a) => json!(
                    a.iter()
                        .map(|x| fix(x.as_str().unwrap_or(""), false))
                        .collect::<Vec<_>>()
                ),
                other => other.clone(),
            })
        };
        let command = fix_args(&command, &mut fix);
        let args = fix_args(&args, &mut fix);
        let healthcheck = healthcheck.map(|mut h| {
            if let Some(t) = h.get("test").cloned() {
                h["test"] = match t {
                    Value::String(s) => json!(fix(&s, false)),
                    Value::Array(a) => json!(
                        a.iter()
                            .map(|x| fix(x.as_str().unwrap_or(""), false))
                            .collect::<Vec<_>>()
                    ),
                    o => o,
                };
            }
            h
        });
        for f in files.iter_mut() {
            f.content = fix(&f.content, false);
        }
        if !rewritten.is_empty() {
            tx.note(format!(
                "{name}: references to {} are rewritten to isb's service names (<app>.<stack>); names an image uses by default are not",
                rewritten.into_iter().collect::<Vec<_>>().join(", ")
            ));
        }
        apps.push(AppTemplate {
            name: key.clone(),
            image,
            env: senv,
            port,
            domains,
            volumes,
            ports,
            replicas,
            command,
            args,
            healthcheck,
            resources: (res.cpus.is_some() || res.memory.is_some()).then_some(res),
            files,
            user,
            working_dir,
            depends_on: dep_keys,
        });
    }
    for (v, users) in &vol_users {
        if users.len() > 1 {
            tx.refuse(format!(
                "volume {v} is shared by {} (an app's volumes are its own)",
                users.iter().cloned().collect::<Vec<_>>().join(", ")
            ));
        }
    }
    for v in unset {
        tx.note(format!(
            "${{{v}}} is not set in the template; it is empty, as in docker compose"
        ));
    }
    for (svc, ..) in &toml.domains {
        if !keys.contains_key(svc) {
            tx.refuse(format!(
                "config.domains names service {svc}, which the compose file lacks"
            ));
        }
    }
    if apps.is_empty() {
        tx.refuse("no service to deploy");
        return None;
    }
    if apps.len() > 1 {
        tx.note("its services reach each other as <app>.<project>-<env> (an org's service names), so it needs a real org (not a host's legacy default org)");
    }
    let main = toml
        .domains
        .first()
        .and_then(|(s, ..)| keys.get(s).cloned())
        .unwrap_or_else(|| apps[0].name.clone());
    // Drop variables nothing uses (helpers in an unused env entry).
    let id = key_name(&meta.id);
    let logo = meta.logo.clone().filter(|l| l.starts_with("https://"));
    Some(Template {
        id,
        name: if meta.name.is_empty() {
            meta.id.clone()
        } else {
            meta.name.clone()
        },
        description: meta.description.clone(),
        version: meta.version.clone(),
        logo,
        tags: meta.tags.clone(),
        links: meta.links.clone(),
        variables: tx.vars.clone(),
        apps,
        main: Some(main),
        notes: vec![],
    })
}

#[cfg(test)]
mod tests;
