//! Templates: one-click apps.
//!
//! A template is metadata, variables and a set of apps. Deploying one into
//! a project environment fills the variables (from the caller, a generator
//! or a default), renders each app template into an ordinary
//! [`crate::app::AppSpec`], and creates the apps; each then deploys like any
//! app, so the app pages, deployments, rollbacks and env editor work for
//! them. A template's app is named `<instance>-<key>` (the main app just
//! `<instance>`), and apps reach each other as `<app>.<project>-<env>`,
//! written `${host:KEY}` in a template.
//!
//! Secret variables (generated passwords and keys) become org secrets
//! (`tpl.<instance>.<var>`); a value built from one (a connection string)
//! becomes its own secret, and so does every file. Stored app definitions
//! hold only references.
//!
//! [`catalog`] holds the built-in templates and the catalogs a platform
//! admin adds; [`dokploy`] translates Dokploy's format into this one.

pub mod catalog;
pub mod dokploy;
pub mod generate;
mod planning;

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::app::{AppSpec, Resources};
use crate::error::{Error, Result};
use crate::org::OrgId;

/// A template, as written in YAML (docs/templates.md).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Template {
    /// `[a-z0-9-]`, unique in its catalog.
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// The template's own version (usually the app's).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub version: String,
    /// A logo URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// `website`, `docs`, `source`, ... to URLs.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub links: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variables: Vec<Variable>,
    pub apps: Vec<AppTemplate>,
    /// The app named after the instance (default: the only app).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main: Option<String>,
    /// What to know after deploying (first-run steps, default logins).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// What a variable holds, and how it gets a value nobody gave.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VarKind {
    /// Text the deployer gives (or `default`).
    #[default]
    String,
    Email,
    Url,
    Int,
    /// A hostname for the ingress; empty means a generated one (`host: auto`).
    Domain,
    /// Generated: `length` letters and digits (default 32).
    Password,
    /// Generated: `bytes` random bytes, base64 (default 32).
    Base64,
    /// Generated: `bytes` random bytes, hex (default 32).
    Hex,
    /// Generated: a random UUID.
    Uuid,
    /// Generated: a free host TCP port.
    Port,
    /// Generated: `length` lowercase letters (default 8).
    Username,
    /// Generated: now (or `at`), in seconds (or `unit: ms`).
    Timestamp,
    /// Generated: an HS256 JWT signed with variable `jwt.secret`.
    Jwt,
}

impl VarKind {
    fn generated(self) -> bool {
        !matches!(
            self,
            VarKind::String | VarKind::Email | VarKind::Url | VarKind::Int | VarKind::Domain
        )
    }

    fn secret_by_default(self) -> bool {
        matches!(
            self,
            VarKind::Password | VarKind::Base64 | VarKind::Hex | VarKind::Jwt
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JwtSpec {
    /// The variable holding the signing secret.
    pub secret: String,
    /// The claims: an expression that renders to a JSON object. Default
    /// `{"iss": "isb", "iat": now, "exp": now + 10 years}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Variable {
    /// `[a-z][a-z0-9_]*`.
    pub name: String,
    #[serde(rename = "type", default)]
    pub kind: VarKind,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Used when the deployer gives nothing; may use `${...}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// A computed value (`${...}` over other variables); not an input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Must the deployer give it? Default: a text variable without a
    /// default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_length: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_length: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<i64>,
    /// Stored as an org secret. Default: passwords, keys and JWTs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jwt: Option<JwtSpec>,
    /// Timestamp: a date instead of now (`2030-01-01T00:00:00Z`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    /// Timestamp: `s` (default) or `ms`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

impl Variable {
    pub fn is_input(&self) -> bool {
        self.value.is_none()
    }

    fn required(&self) -> bool {
        self.required.unwrap_or(
            self.value.is_none() && self.default.is_none() && !self.kind.generated() && {
                self.kind != VarKind::Domain
            },
        )
    }

    fn declared_secret(&self) -> bool {
        self.secret.unwrap_or(self.kind.secret_by_default())
    }
}

/// A file an app gets, rendered and stored as an org secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileTemplate {
    pub path: String,
    pub content: String,
    /// Octal; default `0444` (config files are read by whatever user the
    /// image runs as).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

/// One app of a template: the fields of an [`AppSpec`] with an image
/// source, as strings that may use `${var}` and `${host:KEY}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppTemplate {
    /// The key: `[a-z0-9-]`; the app is `<instance>-<key>`.
    pub name: String,
    /// An isb image reference (`docker:louislam/uptime-kuma:1`).
    pub image: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// `{host, path?, port?, https?, strip_prefix?, ...}` as the ingress
    /// takes them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domains: Vec<serde_json::Map<String, Value>>,
    /// Named volumes, `NAME:/path[:ro]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<String>,
    /// Published host ports (compose syntax).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replicas: Option<u32>,
    /// The whole command line (replaces the image's entrypoint too, as
    /// isb's `command` does): argv, or a line split like a shell would.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Value>,
    /// Arguments after the image's own entrypoint (docker's `command`):
    /// the entrypoint is read from the image when the template is deployed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub healthcheck: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<Resources>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<FileTemplate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    /// Apps deployed (and converged) before this one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
}

// --- expressions ----------------------------------------------------------

/// A piece of a template string.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Lit(String),
    Var(String),
    Host(String),
}

/// `${name}` is a variable, `${host:KEY}` an app's service name, `$$` a
/// literal `$`; any other `$` is literal. Other `${...}` forms are errors.
fn parse_expr(s: &str) -> std::result::Result<Vec<Part>, String> {
    let mut out = Vec::new();
    let mut lit = String::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'$' && b.get(i + 1) == Some(&b'$') {
            lit.push('$');
            i += 2;
        } else if b[i] == b'$' && b.get(i + 1) == Some(&b'{') {
            let end = s[i + 2..]
                .find('}')
                .ok_or_else(|| format!("unterminated ${{ in {s:?}"))?;
            let inner = &s[i + 2..i + 2 + end];
            if !lit.is_empty() {
                out.push(Part::Lit(std::mem::take(&mut lit)));
            }
            if let Some(k) = inner.strip_prefix("host:") {
                out.push(Part::Host(k.to_string()));
            } else if valid_var_name(inner) {
                out.push(Part::Var(inner.to_string()));
            } else {
                return Err(format!(
                    "${{{inner}}}: only ${{variable}} and ${{host:APP}} are expanded (write $${{ for a literal ${{)"
                ));
            }
            i += 2 + end + 1;
        } else {
            let c = s[i..].chars().next().unwrap_or('\0');
            lit.push(c);
            i += c.len_utf8();
        }
    }
    if !lit.is_empty() {
        out.push(Part::Lit(lit));
    }
    Ok(out)
}

fn valid_var_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.starts_with(|c: char| c.is_ascii_lowercase())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn valid_key(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 30
        && s.starts_with(|c: char| c.is_ascii_lowercase())
        && !s.ends_with('-')
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// A rendered piece: text anyone may see, or a secret variable's value.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Seg {
    Lit(String),
    Secret { var: String, value: String },
}

fn concat(segs: &[Seg]) -> String {
    segs.iter()
        .map(|s| match s {
            Seg::Lit(t) => t.as_str(),
            Seg::Secret { value, .. } => value.as_str(),
        })
        .collect()
}

fn has_secret(segs: &[Seg]) -> bool {
    segs.iter().any(|s| matches!(s, Seg::Secret { .. }))
}

/// Every string a template holds, with where it is (for error messages),
/// and whether it is the host of a domain.
fn template_strings(t: &Template) -> Vec<(String, String)> {
    let mut v = Vec::new();
    for var in &t.variables {
        for (what, s) in [("default", &var.default), ("value", &var.value)] {
            if let Some(s) = s {
                v.push((format!("variable {} {what}", var.name), s.clone()));
            }
        }
        if let Some(j) = &var.jwt {
            if let Some(p) = &j.payload {
                v.push((format!("variable {} jwt payload", var.name), p.clone()));
            }
        }
    }
    for a in &t.apps {
        let at = |w: &str| format!("app {} {w}", a.name);
        v.push((at("image"), a.image.clone()));
        for (k, s) in &a.env {
            v.push((at(&format!("env {k}")), s.clone()));
        }
        for d in &a.domains {
            for (k, s) in d {
                if let Some(s) = s.as_str() {
                    v.push((at(&format!("domain {k}")), s.to_string()));
                }
            }
        }
        for s in a.volumes.iter().chain(&a.ports) {
            v.push((at("volumes/ports"), s.clone()));
        }
        for c in [&a.command, &a.args, &a.healthcheck].into_iter().flatten() {
            collect_strings(c, &mut |s| {
                v.push((at("command/healthcheck"), s.to_string()))
            });
        }
        for f in &a.files {
            v.push((at(&format!("file {}", f.path)), f.content.clone()));
        }
        for s in [&a.user, &a.working_dir].into_iter().flatten() {
            v.push((at("user/working_dir"), s.clone()));
        }
    }
    v
}

fn collect_strings(v: &Value, f: &mut dyn FnMut(&str)) {
    match v {
        Value::String(s) => f(s),
        Value::Array(a) => a.iter().for_each(|x| collect_strings(x, f)),
        Value::Object(o) => o.values().for_each(|x| collect_strings(x, f)),
        _ => {}
    }
}

fn argv(v: &Value, what: &str) -> Result<Vec<String>> {
    match v {
        Value::String(s) => {
            crate::flex::split_words(s).map_err(|e| Error::invalid(format!("{what}: {e}")))
        }
        Value::Array(a) => a
            .iter()
            .map(|x| match x {
                Value::String(s) => Ok(s.clone()),
                Value::Number(n) => Ok(n.to_string()),
                Value::Bool(b) => Ok(b.to_string()),
                _ => Err(Error::invalid(format!("{what}: arguments are strings"))),
            })
            .collect(),
        _ => Err(Error::invalid(format!(
            "{what}: a list of arguments, or a command line"
        ))),
    }
}

impl Template {
    /// Parse a template from YAML (or JSON) text.
    pub fn from_yaml(text: &str) -> Result<Template> {
        let v: Value =
            serde_yaml_ng::from_str(text).map_err(|e| Error::invalid(format!("template: {e}")))?;
        let t: Template =
            serde_json::from_value(v).map_err(|e| Error::invalid(format!("template: {e}")))?;
        t.validate()?;
        Ok(t)
    }

    pub fn var(&self, name: &str) -> Option<&Variable> {
        self.variables.iter().find(|v| v.name == name)
    }

    /// The key of the app named after the instance, if any.
    pub fn main_key(&self) -> Option<&str> {
        match &self.main {
            Some(m) => Some(m.as_str()),
            None if self.apps.len() == 1 => Some(self.apps[0].name.as_str()),
            None => None,
        }
    }

    /// Everything checkable without values: names, references, cycles.
    pub fn validate(&self) -> Result<()> {
        let bad = |m: String| Err(Error::invalid(format!("template {}: {m}", self.id)));
        if !valid_key(&self.id) {
            return bad("the id is [a-z0-9-], starting with a letter".into());
        }
        if self.name.trim().is_empty() {
            return bad("a template needs a name".into());
        }
        if self.apps.is_empty() {
            return bad("a template needs at least one app".into());
        }
        let mut names = BTreeSet::new();
        for v in &self.variables {
            if !valid_var_name(&v.name) {
                return bad(format!(
                    "variable {:?}: [a-z][a-z0-9_]*, at most 64 characters",
                    v.name
                ));
            }
            if !names.insert(v.name.as_str()) {
                return bad(format!("variable {} is declared twice", v.name));
            }
            if v.kind == VarKind::Jwt && v.jwt.is_none() {
                return bad(format!("variable {}: a jwt needs jwt.secret", v.name));
            }
            if let Some(j) = &v.jwt {
                if !self.variables.iter().any(|x| x.name == j.secret) {
                    return bad(format!(
                        "variable {}: jwt.secret names no variable ({})",
                        v.name, j.secret
                    ));
                }
            }
            if v.value.is_some() && v.kind.generated() {
                return bad(format!(
                    "variable {}: a generated type takes no value",
                    v.name
                ));
            }
        }
        let mut keys = BTreeSet::new();
        for a in &self.apps {
            if !valid_key(&a.name) {
                return bad(format!(
                    "app {:?}: [a-z0-9-], starting with a letter, at most 30",
                    a.name
                ));
            }
            if !keys.insert(a.name.as_str()) {
                return bad(format!("app {} is declared twice", a.name));
            }
            if a.command.is_some() && a.args.is_some() {
                return bad(format!(
                    "app {}: give command (the whole command line) or args (after the image's entrypoint), not both",
                    a.name
                ));
            }
        }
        if let Some(m) = &self.main {
            if !keys.contains(m.as_str()) {
                return bad(format!("main names no app ({m})"));
            }
        }
        for a in &self.apps {
            for d in &a.depends_on {
                if !keys.contains(d.as_str()) || d == &a.name {
                    return bad(format!("app {}: depends_on {d}: no such other app", a.name));
                }
            }
        }
        self.order()?;
        for (at, s) in template_strings(self) {
            let parts = match parse_expr(&s) {
                Ok(p) => p,
                Err(e) => return bad(format!("{at}: {e}")),
            };
            for p in parts {
                match p {
                    Part::Var(v) if !names.contains(v.as_str()) => {
                        return bad(format!("{at}: ${{{v}}} names no variable"));
                    }
                    Part::Host(k) if !keys.contains(k.as_str()) => {
                        return bad(format!("{at}: ${{host:{k}}} names no app"));
                    }
                    _ => {}
                }
            }
        }
        self.var_order()?;
        Ok(())
    }

    /// App keys in deploy order: dependencies first, else as written.
    pub fn order(&self) -> Result<Vec<String>> {
        let mut done: Vec<String> = Vec::new();
        let mut left: Vec<&AppTemplate> = self.apps.iter().collect();
        while !left.is_empty() {
            let i = left
                .iter()
                .position(|a| a.depends_on.iter().all(|d| done.contains(d)))
                .ok_or_else(|| {
                    Error::invalid(format!(
                        "template {}: depends_on has a cycle among {}",
                        self.id,
                        left.iter()
                            .map(|a| a.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
                })?;
            done.push(left.remove(i).name.clone());
        }
        Ok(done)
    }

    /// Variables in an order where each comes after those it uses.
    fn var_order(&self) -> Result<Vec<&Variable>> {
        let deps = |v: &Variable| -> Vec<String> {
            let mut out = Vec::new();
            for s in [&v.value, &v.default].into_iter().flatten() {
                for p in parse_expr(s).unwrap_or_default() {
                    if let Part::Var(n) = p {
                        out.push(n);
                    }
                }
            }
            if let Some(j) = &v.jwt {
                out.push(j.secret.clone());
                for p in j
                    .payload
                    .as_deref()
                    .map(parse_expr)
                    .and_then(|r| r.ok())
                    .unwrap_or_default()
                {
                    if let Part::Var(n) = p {
                        out.push(n);
                    }
                }
            }
            out
        };
        let mut done: Vec<&Variable> = Vec::new();
        let mut left: Vec<&Variable> = self.variables.iter().collect();
        while !left.is_empty() {
            let i = left
                .iter()
                .position(|v| deps(v).iter().all(|d| done.iter().any(|x| &x.name == d)))
                .ok_or_else(|| {
                    Error::invalid(format!(
                        "template {}: variables refer to each other in a cycle among {}",
                        self.id,
                        left.iter()
                            .map(|v| v.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
                })?;
            done.push(left.remove(i));
        }
        Ok(done)
    }
}

// --- instantiation --------------------------------------------------------

/// Where a template is deployed, and the deployer's values.
#[derive(Debug, Clone)]
pub struct Params {
    pub org: OrgId,
    pub project: String,
    pub environment: String,
    /// Names the apps (`<instance>-<key>`) and the secrets.
    pub instance: String,
    pub values: BTreeMap<String, String>,
}

/// An image's entrypoint (`None`: it has none), for apps with `args`.
pub type EntrypointFn = dyn Fn(&str) -> Result<Option<Vec<String>>> + Send + Sync;

/// What the host contributes.
pub struct Context<'a> {
    /// For generated (`host: auto`) names.
    pub public_ip: Option<IpAddr>,
    pub entrypoint: &'a EntrypointFn,
    pub free_port: &'a (dyn Fn() -> Option<u16> + Send + Sync),
}

/// A secret the deploy creates.
#[derive(Debug, Clone, Serialize)]
pub struct PlannedSecret {
    pub name: String,
    /// What it holds (`variable db_password`, `app web env DATABASE_URL`).
    pub holds: String,
    #[serde(skip)]
    pub value: String,
}

/// A variable's outcome; secret values are never shown.
#[derive(Debug, Clone, Serialize)]
pub struct PlannedVar {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    pub secret: bool,
    /// `given`, `generated`, `default` or `computed`.
    pub source: String,
}

/// Everything a deploy will create.
#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub template: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub version: String,
    pub instance: String,
    pub project: String,
    pub environment: String,
    pub stack: String,
    pub apps: Vec<AppSpec>,
    /// App names, in deploy order.
    pub order: Vec<String>,
    pub secrets: Vec<PlannedSecret>,
    pub variables: Vec<PlannedVar>,
    /// `https://host/path` for each concrete domain.
    pub urls: Vec<String>,
    pub notes: Vec<String>,
}

/// The secret a template variable is stored as.
pub fn var_secret(instance: &str, var: &str) -> String {
    format!("tpl.{instance}.{var}")
}

/// The prefix of every secret an instance owns.
pub fn secret_prefix(instance: &str) -> String {
    format!("tpl.{instance}.")
}

/// The app name for template app `key` in `instance`.
pub fn app_name(instance: &str, key: &str, main: bool) -> String {
    if main || key == instance {
        instance.to_string()
    } else if key.starts_with(&format!("{instance}-")) {
        key.to_string()
    } else {
        format!("{instance}-{key}")
    }
}

#[derive(Debug, Clone)]
struct Resolved {
    /// `None`: a generated domain on a server without a public address.
    value: Option<String>,
    secret: bool,
    /// For a domain variable: it means `host: auto`.
    auto: bool,
}

struct Renderer<'a> {
    p: &'a Params,
    stack: String,
    names: BTreeMap<String, String>,
    vars: BTreeMap<String, Resolved>,
}

impl Renderer<'_> {
    fn render(&self, s: &str, at: &str) -> Result<Vec<Seg>> {
        let parts = parse_expr(s).map_err(|e| Error::invalid(format!("{at}: {e}")))?;
        let mut out = Vec::new();
        for p in parts {
            match p {
                Part::Lit(l) => out.push(Seg::Lit(l)),
                Part::Host(k) => {
                    if self.p.org.is_legacy_default() {
                        return Err(Error::invalid(format!(
                            "{at}: apps reach each other by service name, which this host's default org (incus' own default project) has none of; deploy this template in another org"
                        )));
                    }
                    let n = self.names.get(&k).ok_or_else(|| {
                        Error::invalid(format!("{at}: ${{host:{k}}} names no app"))
                    })?;
                    out.push(Seg::Lit(format!("{n}.{}", self.stack)));
                }
                Part::Var(v) => {
                    let r = self
                        .vars
                        .get(&v)
                        .ok_or_else(|| Error::invalid(format!("{at}: ${{{v}}} is not set yet")))?;
                    let value = r.value.clone().ok_or_else(|| {
                        Error::invalid(format!(
                            "{at}: ${{{v}}} is a generated hostname, and this server has no public address for one (isb serve --ingress-public-ip); give {v} a domain"
                        ))
                    })?;
                    out.push(if r.secret {
                        Seg::Secret { var: v, value }
                    } else {
                        Seg::Lit(value)
                    });
                }
            }
        }
        Ok(out)
    }

    /// A string that must not hold a secret.
    fn plain(&self, s: &str, at: &str) -> Result<String> {
        let segs = self.render(s, at)?;
        if has_secret(&segs) {
            return Err(Error::invalid(format!(
                "{at}: a secret variable cannot go here (only into env, files, command and healthcheck)"
            )));
        }
        Ok(concat(&segs))
    }
}

fn validate_input(v: &Variable, s: &str) -> std::result::Result<(), String> {
    let n = s.chars().count() as u32;
    if let Some(m) = v.min_length {
        if n < m {
            return Err(format!("at least {m} characters"));
        }
    }
    if let Some(m) = v.max_length {
        if n > m {
            return Err(format!("at most {m} characters"));
        }
    }
    if !v.choices.is_empty() && !v.choices.iter().any(|c| c == s) {
        return Err(format!("one of {}", v.choices.join(", ")));
    }
    if s.contains('\0') {
        return Err("no NUL characters".into());
    }
    match v.kind {
        VarKind::Email => {
            let ok = s.split_once('@').is_some_and(|(a, d)| {
                !a.is_empty() && d.contains('.') && !d.starts_with('.') && !d.ends_with('.')
            }) && !s.contains(char::is_whitespace);
            if !ok {
                return Err("an email address".into());
            }
        }
        VarKind::Url => {
            if !(s.starts_with("http://") || s.starts_with("https://"))
                || s.contains(char::is_whitespace)
            {
                return Err("an http(s) URL".into());
            }
        }
        VarKind::Int | VarKind::Port | VarKind::Timestamp => {
            let i: i64 = s.parse().map_err(|_| "a whole number".to_string())?;
            let (lo, hi) = match v.kind {
                VarKind::Port => (Some(1), Some(65535)),
                _ => (v.min, v.max),
            };
            if lo.is_some_and(|l| i < l) || hi.is_some_and(|h| i > h) {
                return Err(format!(
                    "between {} and {}",
                    lo.map(|x| x.to_string()).unwrap_or("-".into()),
                    hi.map(|x| x.to_string()).unwrap_or("-".into())
                ));
            }
        }
        VarKind::Domain => {
            if !(s.is_empty() || s == "auto" || is_hostname(s)) {
                return Err("a hostname (example.com), or empty for a generated one".into());
            }
        }
        _ => {
            if s.is_empty() && v.required() {
                return Err("required".into());
            }
        }
    }
    Ok(())
}

fn is_hostname(s: &str) -> bool {
    s.len() <= 253
        && s.contains('.')
        && s.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
}

fn sh_single(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The environment variable a secret is reached through in a shell line.
fn secret_env_name(var: &str) -> String {
    format!("ISB_TPL_{}", var.to_ascii_uppercase())
}

/// Render a template into what a deploy creates. Pure apart from the
/// generators and `ctx`.
pub fn plan(t: &Template, p: &Params, ctx: &Context) -> Result<Plan> {
    t.validate()?;
    crate::app::validate_app_name(&p.instance)
        .map_err(|_| Error::invalid(format!("name {:?}: [a-z0-9-], starting with a letter, at most 30 characters (it names the apps)", p.instance)))?;
    let stack = crate::app::stack_name(&p.project, &p.environment)?;
    planning::check_values(t, p)?;
    let names = planning::app_names(t, p)?;
    let uses = planning::domain_uses(t);
    let mut r = Renderer {
        p,
        stack: stack.clone(),
        names,
        vars: BTreeMap::new(),
    };
    let mut out = planning::Out::default();
    let variables = planning::resolve_vars(t, &mut r, ctx, &uses, &mut out)?;
    let apps = t
        .apps
        .iter()
        .map(|a| planning::plan_app(a, &r, ctx, &uses, &mut out))
        .collect::<Result<Vec<_>>>()?;
    let order = t
        .order()?
        .into_iter()
        .map(|k| r.names[&k].clone())
        .collect();
    let mut seen = BTreeSet::new();
    out.secrets.retain(|s| seen.insert(s.name.clone()));
    out.notes.extend(t.notes.iter().cloned());
    Ok(Plan {
        template: t.id.clone(),
        version: t.version.clone(),
        instance: p.instance.clone(),
        project: p.project.clone(),
        environment: p.environment.clone(),
        stack,
        apps,
        order,
        secrets: out.secrets,
        variables,
        urls: out.urls,
        notes: out.notes,
    })
}

/// An OCI image's entrypoint, read with `skopeo inspect --config` (within
/// a minute). `None`: the image has none.
pub fn skopeo_entrypoint(image: &str) -> Result<Option<Vec<String>>> {
    use std::io::Read;
    use std::time::{Duration, Instant};
    let src = crate::plan::ImageSource::parse(image)?;
    if !src.is_oci() {
        return Err(Error::invalid(format!("{image} is not an OCI image")));
    }
    let host = src
        .server
        .as_deref()
        .and_then(|s| s.strip_prefix("https://"))
        .ok_or_else(|| Error::invalid(format!("{image}: no registry")))?;
    let r = format!("docker://{host}/{}", src.alias);
    let mut child = std::process::Command::new("skopeo")
        .args(["inspect", "--config", &r])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| Error::invalid(format!("skopeo: {e}")))?;
    let mut out = Vec::new();
    let mut stdout = child.stdout.take().expect("piped");
    let reader = std::thread::spawn(move || {
        let _ = stdout.read_to_end(&mut out);
        out
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait()? {
            Some(s) => break s,
            None if started.elapsed() > Duration::from_secs(60) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::invalid(format!("skopeo inspect {r}: timed out")));
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    };
    let out = reader.join().unwrap_or_default();
    if !status.success() {
        return Err(Error::invalid(format!("skopeo inspect {r} failed")));
    }
    let v: Value = serde_json::from_slice(&out)
        .map_err(|e| Error::invalid(format!("skopeo inspect {r}: {e}")))?;
    Ok(v["config"]["Entrypoint"].as_array().map(|a| {
        a.iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect()
    }))
}

/// A deployed instance, kept so it can be listed and removed as one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Instance {
    pub name: String,
    /// `catalog/id`.
    pub template: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub version: String,
    pub project: String,
    pub environment: String,
    pub apps: Vec<String>,
    pub secrets: Vec<String>,
    /// Non-secret variable values.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub variables: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub urls: Vec<String>,
    pub created_at: u64,
    pub created_by: String,
}

#[cfg(test)]
mod tests;
