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

/// The domain variables used as some app's domain host, with the apps
/// using each (in template order).
fn domain_uses(t: &Template) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for a in &t.apps {
        for d in &a.domains {
            if let Some(Ok(parts)) = d.get("host").and_then(Value::as_str).map(parse_expr) {
                if let [Part::Var(v)] = parts.as_slice() {
                    let e = out.entry(v.clone()).or_default();
                    if !e.contains(&a.name) {
                        e.push(a.name.clone());
                    }
                }
            }
        }
    }
    out
}

fn sh_single(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// `exec ARGS...` for /bin/sh, each argument quoted, a secret's part as
/// `"${ISB_TPL_VAR}"`: the value reaches the process through a variable,
/// never through the stored command line. `used` gets the variables.
fn shell_line(args: Vec<Vec<Seg>>, used: &mut BTreeSet<String>) -> String {
    let mut words = Vec::new();
    for segs in args {
        let mut w = String::new();
        for s in segs {
            match s {
                Seg::Lit(l) if !l.is_empty() => w.push_str(&sh_single(&l)),
                Seg::Lit(_) => {}
                Seg::Secret { var, .. } => {
                    w.push_str(&format!("\"${{{}}}\"", secret_env_name(&var)));
                    used.insert(var);
                }
            }
        }
        if w.is_empty() {
            w.push_str("''");
        }
        words.push(w);
    }
    format!("exec {}", words.join(" "))
}

/// The environment variable a secret is reached through in a shell line.
fn secret_env_name(var: &str) -> String {
    format!("ISB_TPL_{}", var.to_ascii_uppercase())
}

/// Render a template into what a deploy creates. Pure apart from the
/// generators and `ctx`.
#[allow(
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    clippy::excessive_nesting,
    reason = "predates the lint ratchet; split it when next changed"
)]
pub fn plan(t: &Template, p: &Params, ctx: &Context) -> Result<Plan> {
    t.validate()?;
    crate::app::validate_app_name(&p.instance)
        .map_err(|_| Error::invalid(format!("name {:?}: [a-z0-9-], starting with a letter, at most 30 characters (it names the apps)", p.instance)))?;
    let stack = crate::app::stack_name(&p.project, &p.environment)?;
    for k in p.values.keys() {
        match t.var(k) {
            None => {
                return Err(Error::invalid(format!(
                    "{k}: template {} has no such variable",
                    t.id
                )));
            }
            Some(v) if !v.is_input() => {
                return Err(Error::invalid(format!("{k} is computed; it cannot be set")));
            }
            _ => {}
        }
    }
    let main = t.main_key();
    let names: BTreeMap<String, String> = t
        .apps
        .iter()
        .map(|a| {
            (
                a.name.clone(),
                app_name(&p.instance, &a.name, main == Some(a.name.as_str())),
            )
        })
        .collect();
    let uniq: BTreeSet<&String> = names.values().collect();
    if uniq.len() != names.len() {
        return Err(Error::invalid(format!(
            "template {}: two apps would both be named {}; pick another name",
            t.id, p.instance
        )));
    }
    let mut notes = Vec::new();
    let uses = domain_uses(t);
    let mut r = Renderer {
        p,
        stack: stack.clone(),
        names,
        vars: BTreeMap::new(),
    };
    let mut planned_vars = Vec::new();
    let mut secrets: Vec<PlannedSecret> = Vec::new();
    for v in t.var_order()? {
        let at = format!("variable {}", v.name);
        let given = p.values.get(&v.name).filter(|s| !s.is_empty());
        if let Some(g) = given {
            validate_input(v, g).map_err(|e| Error::invalid(format!("{}: {e}", v.name)))?;
        }
        let mut secret = v.declared_secret();
        let mut auto = false;
        let (value, source): (Option<String>, &str) = if let Some(expr) = &v.value {
            let segs = r.render(expr, &at)?;
            secret |= has_secret(&segs);
            (Some(concat(&segs)), "computed")
        } else if v.kind == VarKind::Domain {
            match given.map(String::as_str) {
                Some(h) if h != "auto" => (Some(h.to_string()), "given"),
                _ => {
                    let d = match &v.default {
                        Some(d) => r.plain(d, &at)?,
                        None => String::new(),
                    };
                    if !d.is_empty() && d != "auto" {
                        (Some(d), "default")
                    } else {
                        auto = true;
                        let users = uses.get(&v.name);
                        let anchor = users
                            .and_then(|u| u.first())
                            .map(String::as_str)
                            .or(main)
                            .unwrap_or(t.apps[0].name.as_str());
                        let svc = &r.names[anchor];
                        let host = match ctx.public_ip {
                            Some(ip) => {
                                Some(crate::ingress::domain::auto_host(&p.org, &stack, svc, ip)?)
                            }
                            None => None,
                        };
                        if host.is_none() {
                            notes.push(format!(
                                "{}: a generated hostname needs a public address (isb serve --ingress-public-ip); until then the domain is not served",
                                v.name
                            ));
                        }
                        (host, "generated")
                    }
                }
            }
        } else if let Some(g) = given {
            (Some(g.clone()), "given")
        } else if v.kind.generated() {
            (Some(generate_value(v, &r, ctx, &at)?), "generated")
        } else if let Some(d) = &v.default {
            let segs = r.render(d, &at)?;
            secret |= has_secret(&segs);
            (Some(concat(&segs)), "default")
        } else if v.required() {
            return Err(Error::invalid(format!(
                "{} is required{}",
                v.name,
                if v.description.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", v.description)
                }
            )));
        } else {
            (Some(String::new()), "default")
        };
        if v.default.is_some() && source == "default" {
            if let Some(val) = &value {
                validate_input(v, val)
                    .map_err(|e| Error::invalid(format!("{} default: {e}", v.name)))?;
            }
        }
        if secret {
            let name = var_secret(&p.instance, &v.name);
            crate::secrets::validate_name(&name)?;
            secrets.push(PlannedSecret {
                name,
                holds: format!("variable {}", v.name),
                value: value.clone().unwrap_or_default(),
            });
        }
        planned_vars.push(PlannedVar {
            name: v.name.clone(),
            value: if secret { None } else { value.clone() },
            secret,
            source: source.into(),
        });
        r.vars.insert(
            v.name.clone(),
            Resolved {
                value,
                secret,
                auto,
            },
        );
    }

    let mut apps = Vec::new();
    let mut urls = Vec::new();
    for a in &t.apps {
        let name = r.names[&a.name].clone();
        let at = |w: &str| format!("app {} {w}", a.name);
        let mut env = serde_json::Map::new();
        let mut tpl_env: BTreeSet<String> = BTreeSet::new();
        let secret_ref = |var: &str| json!({"secret": var_secret(&p.instance, var)});
        for (k, s) in &a.env {
            let segs = r.render(s, &at(&format!("env {k}")))?;
            let v = match segs.as_slice() {
                [Seg::Secret { var, .. }] => secret_ref(var),
                _ if has_secret(&segs) => {
                    let sname = format!("tpl.{}.{}.env.{k}", p.instance, a.name);
                    crate::secrets::validate_name(&sname).map_err(|_| {
                        Error::invalid(format!(
                            "{}: env {k}: the name cannot make a secret name",
                            a.name
                        ))
                    })?;
                    secrets.push(PlannedSecret {
                        name: sname.clone(),
                        holds: format!("app {name} env {k}"),
                        value: concat(&segs),
                    });
                    json!({"secret": sname})
                }
                _ => json!(concat(&segs)),
            };
            env.insert(k.clone(), v);
        }
        let command: Option<Vec<String>> = match (&a.command, &a.args) {
            (Some(c), _) | (None, Some(c)) => {
                let mut words = argv(c, &at("command"))?;
                if a.args.is_some() {
                    let image = r.plain(&a.image, &at("image"))?;
                    let ep = (ctx.entrypoint)(&image).map_err(|e| {
                        Error::invalid(format!(
                            "app {}: args need the image's entrypoint, and reading {image} failed: {e}",
                            a.name
                        ))
                    })?;
                    let mut full = ep.unwrap_or_default();
                    full.extend(words);
                    words = full;
                }
                let segs: Vec<Vec<Seg>> = words
                    .iter()
                    .map(|w| r.render(w, &at("command")))
                    .collect::<Result<_>>()?;
                if segs.iter().any(|s| has_secret(s)) {
                    Some(vec![
                        "/bin/sh".into(),
                        "-c".into(),
                        shell_line(segs, &mut tpl_env),
                    ])
                } else {
                    Some(segs.iter().map(|s| concat(s)).collect())
                }
            }
            (None, None) => None,
        };
        let healthcheck = match &a.healthcheck {
            None => None,
            Some(h) => {
                let mut h = h.clone();
                let test = h.get("test").cloned();
                if let Some(test) = test {
                    let (shell, words): (bool, Vec<String>) = match &test {
                        Value::String(s) => (true, vec![s.clone()]),
                        Value::Array(a) => {
                            let w = argv(&test, "healthcheck test")?;
                            match a.first().and_then(Value::as_str) {
                                Some("CMD-SHELL") => (true, w[1..].to_vec()),
                                Some("CMD") => (false, w[1..].to_vec()),
                                _ => (false, w),
                            }
                        }
                        _ => (false, vec![]),
                    };
                    let segs: Vec<Vec<Seg>> = words
                        .iter()
                        .map(|w| r.render(w, &at("healthcheck")))
                        .collect::<Result<_>>()?;
                    let new = if !segs.iter().any(|s| has_secret(s)) {
                        match &test {
                            Value::String(_) => json!(concat(&segs[0])),
                            _ => {
                                let mut v: Vec<String> = Vec::new();
                                if let Some(first) = test
                                    .as_array()
                                    .and_then(|a| a.first())
                                    .and_then(Value::as_str)
                                    .filter(|f| matches!(*f, "CMD" | "CMD-SHELL" | "NONE"))
                                {
                                    v.push(first.into());
                                }
                                v.extend(segs.iter().map(|s| concat(s)));
                                json!(v)
                            }
                        }
                    } else if shell {
                        // Inside a shell line: the secret becomes a variable.
                        let mut line = String::new();
                        for (i, s) in segs.iter().enumerate() {
                            if i > 0 {
                                line.push(' ');
                            }
                            for seg in s {
                                match seg {
                                    Seg::Lit(l) => line.push_str(l),
                                    Seg::Secret { var, .. } => {
                                        line.push_str(&format!("${{{}}}", secret_env_name(var)));
                                        tpl_env.insert(var.clone());
                                    }
                                }
                            }
                        }
                        json!(["CMD-SHELL", line])
                    } else {
                        let l = shell_line(segs, &mut tpl_env);
                        json!(["CMD-SHELL", l])
                    };
                    h["test"] = new;
                }
                if let Some(o) = h.as_object_mut() {
                    for (k, v) in o.iter_mut() {
                        if k != "test" {
                            if let Some(s) = v.as_str() {
                                *v = json!(r.plain(s, &at("healthcheck"))?);
                            }
                        }
                    }
                }
                Some(h)
            }
        };
        for var in &tpl_env {
            env.insert(secret_env_name(var), secret_ref(var));
        }
        if !tpl_env.is_empty() {
            notes.push(format!(
                "app {name}: a secret in its command or healthcheck reaches it as a variable through /bin/sh"
            ));
        }
        let mut domains = Vec::new();
        for d in &a.domains {
            let mut out = serde_json::Map::new();
            for (k, v) in d {
                let nv = match (k.as_str(), v.as_str()) {
                    ("host", Some(h)) => {
                        let parts = parse_expr(h).unwrap_or_default();
                        match parts.as_slice() {
                            [Part::Var(var)]
                                if r.vars.get(var).is_some_and(|x| x.auto)
                                    && uses.get(var).is_some_and(|u| u.len() == 1) =>
                            {
                                json!("auto")
                            }
                            [Part::Var(var)] if r.vars.get(var).is_some_and(|x| x.auto) => {
                                // Shared by several apps: one generated name
                                // for all (the first app's).
                                match &r.vars[var].value {
                                    Some(v) => {
                                        notes.push(format!(
                                            "{var}: the generated name {v} is shared by several apps, so it is written out; an org with a domain allowlist needs it listed"
                                        ));
                                        json!(v)
                                    }
                                    None => json!("auto"),
                                }
                            }
                            _ => json!(r.plain(h, &at("domain host"))?),
                        }
                    }
                    (_, Some(s)) => json!(r.plain(s, &at(&format!("domain {k}")))?),
                    _ => v.clone(),
                };
                out.insert(k.clone(), nv);
            }
            let host = out.get("host").and_then(Value::as_str).unwrap_or("");
            let shown = if host == "auto" {
                d.get("host").and_then(Value::as_str).and_then(|h| {
                    match parse_expr(h).ok()?.as_slice() {
                        [Part::Var(v)] => r.vars.get(v)?.value.clone(),
                        _ => None,
                    }
                })
            } else {
                Some(host.to_string())
            };
            if let Some(h) = shown {
                let https = out.get("https").and_then(Value::as_bool).unwrap_or(true);
                let path = out.get("path").and_then(Value::as_str).unwrap_or("/");
                let url = format!("{}://{h}{path}", if https { "https" } else { "http" });
                if !urls.contains(&url) {
                    urls.push(url);
                }
            }
            domains.push(Value::Object(out));
        }
        let mut files = Vec::new();
        for (i, f) in a.files.iter().enumerate() {
            let path = r.plain(&f.path, &at("file path"))?;
            let sname = format!("tpl.{}.{}.file{}", p.instance, a.name, i + 1);
            crate::secrets::validate_name(&sname)?;
            let content = concat(&r.render(&f.content, &at(&format!("file {path}")))?);
            secrets.push(PlannedSecret {
                name: sname.clone(),
                holds: format!("app {name} file {path}"),
                value: content,
            });
            files.push(json!({"path": path, "secret": sname, "mode": f.mode.clone().unwrap_or_else(|| "0444".into())}));
        }
        let mut spec = json!({
            "name": name,
            "project": p.project,
            "environment": p.environment,
            "source": {"image": r.plain(&a.image, &at("image"))?},
        });
        if !env.is_empty() {
            spec["env"] = Value::Object(env);
        }
        if let Some(port) = a.port {
            spec["port"] = json!(port);
        }
        if !domains.is_empty() {
            spec["domains"] = json!(domains);
        }
        let vols: Vec<String> = a
            .volumes
            .iter()
            .map(|v| r.plain(v, &at("volumes")))
            .collect::<Result<_>>()?;
        if !vols.is_empty() {
            spec["volumes"] = json!(vols);
        }
        let ports: Vec<String> = a
            .ports
            .iter()
            .map(|v| r.plain(v, &at("ports")))
            .collect::<Result<_>>()?;
        if !ports.is_empty() {
            spec["ports"] = json!(ports);
        }
        if let Some(n) = a.replicas {
            spec["replicas"] = json!(n);
        }
        if let Some(c) = command {
            spec["command"] = json!(c);
        }
        if let Some(h) = healthcheck {
            spec["healthcheck"] = h;
        }
        if let Some(res) = &a.resources {
            let mut out = serde_json::Map::new();
            if let Some(c) = &res.cpus {
                let c = r.plain(c, &at("resources"))?;
                // isb pins whole CPUs.
                let c = match c.parse::<f64>() {
                    Ok(f) if f > 0.0 && f.fract() != 0.0 => {
                        notes.push(format!(
                            "app {name}: cpus {c} is rounded up to {}",
                            f.ceil()
                        ));
                        (f.ceil() as u64).to_string()
                    }
                    _ => c,
                };
                out.insert("cpus".into(), json!(c));
            }
            if let Some(m) = &res.memory {
                out.insert("memory".into(), json!(r.plain(m, &at("resources"))?));
            }
            spec["resources"] = Value::Object(out);
        }
        if !files.is_empty() {
            spec["files"] = json!(files);
        }
        if let Some(u) = &a.user {
            spec["user"] = json!(r.plain(u, &at("user"))?);
        }
        if let Some(w) = &a.working_dir {
            spec["working_dir"] = json!(r.plain(w, &at("working_dir"))?);
        }
        let spec: AppSpec = serde_json::from_value(spec)
            .map_err(|e| Error::invalid(format!("app {}: {e}", a.name)))?;
        spec.validate()
            .map_err(|e| Error::invalid(format!("app {}: {e}", a.name)))?;
        apps.push(spec);
    }
    let order = t
        .order()?
        .into_iter()
        .map(|k| r.names[&k].clone())
        .collect();
    let mut seen = BTreeSet::new();
    secrets.retain(|s| seen.insert(s.name.clone()));
    notes.extend(t.notes.iter().cloned());
    Ok(Plan {
        template: t.id.clone(),
        version: t.version.clone(),
        instance: p.instance.clone(),
        project: p.project.clone(),
        environment: p.environment.clone(),
        stack,
        apps,
        order,
        secrets,
        variables: planned_vars,
        urls,
        notes,
    })
}

fn generate_value(v: &Variable, r: &Renderer, ctx: &Context, at: &str) -> Result<String> {
    Ok(match v.kind {
        VarKind::Password => generate::password(v.length.unwrap_or(32).clamp(8, 256)),
        VarKind::Base64 => generate::base64(v.bytes.unwrap_or(32).clamp(8, 512)),
        VarKind::Hex => generate::hex(v.bytes.unwrap_or(32).clamp(4, 512)),
        VarKind::Uuid => generate::uuid(),
        VarKind::Username => generate::username(v.length.unwrap_or(8).clamp(3, 64)),
        VarKind::Port => (ctx.free_port)()
            .ok_or_else(|| Error::invalid(format!("{at}: no free port found")))?
            .to_string(),
        VarKind::Timestamp => {
            let secs = match &v.at {
                Some(d) => generate::parse_date(&r.plain(d, at)?).ok_or_else(|| {
                    Error::invalid(format!("{at}: at: a date like 2030-01-01T00:00:00Z"))
                })?,
                None => generate::now_secs(),
            };
            match v.unit.as_deref() {
                Some("ms") => (secs * 1000).to_string(),
                _ => secs.to_string(),
            }
        }
        VarKind::Jwt => {
            let j = v.jwt.as_ref().expect("validated: a jwt has jwt.secret");
            let secret = r
                .vars
                .get(&j.secret)
                .and_then(|x| x.value.clone())
                .ok_or_else(|| Error::invalid(format!("{at}: no value for {}", j.secret)))?;
            let payload = match &j.payload {
                Some(p) => {
                    let text = concat(&r.render(p, at)?);
                    let v: Value = serde_json::from_str(&text).map_err(|e| {
                        Error::invalid(format!("{at}: the payload is not JSON: {e}"))
                    })?;
                    let Value::Object(mut o) = v else {
                        return Err(Error::invalid(format!(
                            "{at}: the payload must be a JSON object"
                        )));
                    };
                    // A partial payload gets the default times.
                    let now = generate::now_secs();
                    o.entry("iat").or_insert(json!(now));
                    o.entry("exp").or_insert(json!(now + 10 * 365 * 86400));
                    Value::Object(o)
                }
                None => {
                    let now = generate::now_secs();
                    json!({"iss": "isb", "iat": now, "exp": now + 10 * 365 * 86400})
                }
            };
            generate::jwt(&secret, &payload)
        }
        VarKind::String | VarKind::Email | VarKind::Url | VarKind::Int | VarKind::Domain => {
            unreachable!("not a generated kind")
        }
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
