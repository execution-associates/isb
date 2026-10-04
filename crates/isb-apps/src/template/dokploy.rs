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

use super::shared::*;
pub use super::shared::{Report, Status, image_ref, key_name};
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
    finish(t, tx)
}

/// Each Dokploy variable as a native one; returns the Dokploy variables.
fn native_vars(toml: &Toml, tx: &mut Tx) -> BTreeMap<String, String> {
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
    dvars
}

/// The .env: each entry a native expression, and the order they came in.
fn native_env(
    toml: &Toml,
    dvars: &BTreeMap<String, String>,
    tx: &mut Tx,
) -> (BTreeMap<String, String>, Vec<String>) {
    let mut env: BTreeMap<String, String> = BTreeMap::new();
    let mut env_order: Vec<String> = Vec::new();
    for (k, v) in &toml.env {
        let e = tx.expr(v, &format!("env_{k}"), dvars);
        if !env.contains_key(k) {
            env_order.push(k.clone());
        }
        env.insert(k.clone(), e);
    }
    (env, env_order)
}

/// What only shows once every service is translated: shared volumes,
/// variables nothing sets, and domains for services the compose file lacks.
fn check_services(acc: service::Acc, toml: &Toml, keys: &BTreeMap<String, String>, tx: &mut Tx) {
    for (v, users) in &acc.vol_users {
        if users.len() > 1 {
            tx.refuse(format!(
                "volume {v} is shared by {} (an app's volumes are its own)",
                users.iter().cloned().collect::<Vec<_>>().join(", ")
            ));
        }
    }
    for v in acc.unset {
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
}

fn translate_inner(meta: &Meta, compose: &str, toml_text: &str, tx: &mut Tx) -> Option<Template> {
    let toml = parse_toml(toml_text, tx)?;
    let dvars = native_vars(&toml, tx);
    let (env, env_order) = native_env(&toml, &dvars, tx);
    let mounts: BTreeMap<String, String> = toml
        .mounts
        .iter()
        .map(|(p, c)| (p.clone(), tx.expr(c, "file", &dvars)))
        .collect();

    // The compose file.
    let doc = parse_compose(compose, tx)?;
    let Some(top) = ymap(&doc) else {
        tx.refuse("docker-compose.yml is not a mapping");
        return None;
    };
    check_top_level(top, tx);
    let declared_vols = declared_volumes(top, tx);
    let Some(services) = yget(top, "services").and_then(ymap) else {
        tx.refuse("docker-compose.yml has no services");
        return None;
    };
    let (keys, aliases) = service_names(services, tx);
    let sh = service::Shared {
        env: &env,
        env_order: &env_order,
        mounts: &mounts,
        declared_vols: &declared_vols,
        keys: &keys,
        aliases: &aliases,
        domains: &toml.domains,
        dvars: &dvars,
    };
    // A volume used by two services cannot be two apps' own volumes.
    let mut acc = service::Acc {
        unset: BTreeSet::new(),
        vol_users: BTreeMap::new(),
    };
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
        if let Some(app) = service::translate(&sh, &mut acc, &name, key, m, tx) {
            apps.push(app);
        }
    }
    check_services(acc, &toml, &keys, tx);
    if apps.is_empty() {
        tx.refuse("no service to deploy");
        return None;
    }
    if apps.len() > 1 {
        tx.note("its services reach each other as <app>.<project>-<env> (an org's service names)");
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

pub(in crate::template) mod service;
#[cfg(test)]
mod tests;
