//! Applications: the Dokploy-style object over stacks.
//!
//! An org holds projects; a project holds environments (`production` by
//! default); an environment holds apps. An app is a source (an image, or a
//! git repository plus a [`crate::build::Builder`]) and the settings it runs
//! with: environment, domains, volumes, replicas, port, health check,
//! resources, command.
//!
//! A project's environment renders to ONE ordinary stack named
//! `<project>-<env>`, each app one service in it, so apps reach each other
//! as `<app>.<project>-<env>` and the stack controller does the rolling
//! deploys. Deploying an app replaces only its own service in that stack
//! (revisions are per service), so only that app rolls.
//!
//! Everything lives under the daemon's state directory, next to the org's
//! stacks: `apps/` in the default org, `orgs/<org>/apps/` in the others.
//!
//! ```text
//! apps/projects/<project>.json
//! apps/<app>/app.json
//! apps/<app>/deployments/<n>.json, <n>.log
//! sources/<app>/repo, known_hosts           (git checkouts)
//! ```

pub mod deploy;
pub mod env;
pub mod git;
pub mod webhook;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::build::Builder;
use crate::error::{Error, Result};
use crate::org::OrgId;
use crate::spec::{NamedVolumeSpec, SandboxSpec, SecretDef};

pub use deploy::{Apps, BuildFn, DigestFn};
pub use env::{EnvFile, EnvValue};
pub use git::{GitAuth, GitSource};

/// The environment a project starts with.
pub const DEFAULT_ENVIRONMENT: &str = "production";

/// Label (`user.isb.app`) on every instance of an app.
pub const LABEL_APP: &str = "isb.app";

/// Where an org's apps, projects and sources live: next to its stacks.
pub fn org_root(state: &Path, org: &OrgId) -> PathBuf {
    if org.is_default() {
        state.to_path_buf()
    } else {
        org.dir(state)
    }
}

/// A project: a named group of environments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub environments: Vec<String>,
    pub created_at: u64,
}

/// A project or environment name: `<project>-<env>` must be a stack name.
pub fn validate_part(kind: &str, s: &str) -> Result<()> {
    let ok = !s.is_empty()
        && s.len() <= 24
        && s.starts_with(|c: char| c.is_ascii_lowercase())
        && !s.ends_with('-')
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "{kind} name {s:?}: up to 24 characters of [a-z0-9-], starting with a letter"
        )))
    }
}

/// The stack a project's environment renders to.
pub fn stack_name(project: &str, environment: &str) -> Result<String> {
    let n = format!("{project}-{environment}");
    crate::stack::validate_stack_name(&n).map_err(|_| {
        Error::invalid(format!(
            "{project} + {environment}: the stack name {n:?} is over 30 characters; shorten one"
        ))
    })?;
    Ok(n)
}

/// Where an app's code or image comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum Source {
    /// An image as a compose `image:` takes it (`docker:nginx:1.27`,
    /// `ghcr:org/app:tag`, a local alias).
    Image(String),
    Git(GitSource),
}

/// How a git source becomes an image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildSettings {
    pub builder: Builder,
    /// Build-time variables (Dockerfile `ARG`s, buildpack env).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub args: BTreeMap<String, String>,
    /// Build in a VM (default) rather than a container.
    #[serde(default = "yes")]
    pub untrusted: bool,
}

fn yes() -> bool {
    true
}

/// CPU and memory limits per replica.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resources {
    /// `limits.cpu`: a count, e.g. `2`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpus: Option<String>,
    /// `512m`, `2g`, `2GiB`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
}

/// What a user sets on an app.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSpec {
    pub name: String,
    pub project: String,
    #[serde(default = "default_env")]
    pub environment: String,
    pub source: Source,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<BuildSettings>,
    /// `.env` text, or a `{KEY: value | {secret: NAME}}` map.
    #[serde(default)]
    pub env: EnvFile,
    /// The ingress' `domains:` list (`{host, path?, port?, https?,
    /// redirect?}`), passed to the rendered service as is. `port`
    /// defaults to the app's `port`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domains: Vec<serde_json::Map<String, Value>>,
    /// Named volumes, `NAME:/path[:ro]`. Each is the app's own
    /// (`<stack>_<app>_<name>`), shared by its replicas. Host paths are
    /// not allowed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<String>,
    /// Published host ports, compose syntax (`127.0.0.1:8080:80`),
    /// load-balanced over healthy replicas.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<String>,
    #[serde(default = "one")]
    pub replicas: u32,
    /// The port the app listens on inside its instances.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// A compose `healthcheck`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub healthcheck: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<Resources>,
    /// A compose `command`: argv, or a line split like a shell would.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Value>,
}

fn default_env() -> String {
    DEFAULT_ENVIRONMENT.into()
}

fn one() -> u32 {
    1
}

/// An app as stored: what the user set, plus bookkeeping.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct App {
    pub spec: AppSpec,
    pub created_at: u64,
    pub updated_at: u64,
    /// The number the next deployment gets.
    #[serde(default = "one_u64")]
    pub next_deployment: u64,
    /// The deployment running now (the last one that finished `done`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<u64>,
}

fn one_u64() -> u64 {
    1
}

impl AppSpec {
    pub fn stack(&self) -> Result<String> {
        stack_name(&self.project, &self.environment)
    }

    /// Check everything that does not need the host.
    pub fn validate(&self) -> Result<()> {
        validate_app_name(&self.name)?;
        validate_part("project", &self.project)?;
        validate_part("environment", &self.environment)?;
        let stack = self.stack()?;
        crate::stack::instance_name(&stack, &self.name, 100, "0000").map_err(|_| {
            Error::invalid(format!(
                "app {}: instance names in stack {stack} would be too long; shorten the app, project or environment name",
                self.name
            ))
        })?;
        match (&self.source, &self.build) {
            (Source::Image(i), None) => {
                crate::plan::ImageSource::parse(i)?;
            }
            (Source::Image(_), Some(_)) => {
                return Err(Error::invalid("an image source is not built; drop `build`"));
            }
            (Source::Git(g), Some(_)) => {
                g.validate()?;
            }
            (Source::Git(_), None) => {
                return Err(Error::invalid(
                    "a git source needs `build` (e.g. {builder: {type: railpack}})",
                ));
            }
        }
        if self.replicas > 100 {
            return Err(Error::invalid("replicas: at most 100"));
        }
        for v in &self.volumes {
            parse_volume(v)?;
        }
        for d in &self.domains {
            let host = d.get("host").and_then(Value::as_str).unwrap_or("");
            if host.is_empty() {
                return Err(Error::invalid("every domain needs a host"));
            }
            if !d.contains_key("port") && self.port.is_none() {
                return Err(Error::invalid(format!(
                    "domain {host}: give it a port, or set the app's port"
                )));
            }
        }
        Ok(())
    }

    /// The webhook secret's name in the org's store.
    pub fn webhook_secret(&self) -> String {
        webhook_secret(&self.name)
    }
}

pub fn webhook_secret(app: &str) -> String {
    format!("app.{app}.webhook")
}

pub fn deploy_key_secret(app: &str) -> String {
    format!("app.{app}.deploy-key")
}

/// An app name: a service name in its stack and a DNS label.
pub fn validate_app_name(s: &str) -> Result<()> {
    let ok = !s.is_empty()
        && s.len() <= 30
        && s.starts_with(|c: char| c.is_ascii_lowercase())
        && !s.ends_with('-')
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "app name {s:?}: up to 30 characters of [a-z0-9-], starting with a letter"
        )))
    }
}

/// `NAME:/path[:ro|rw]`: a named volume.
fn parse_volume(v: &str) -> Result<(String, String, Option<String>)> {
    let mut parts = v.splitn(3, ':');
    let name = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");
    let opts = parts.next().map(String::from);
    let name_ok = !name.is_empty()
        && name.len() <= 30
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !name_ok {
        return Err(Error::invalid(format!(
            "volume {v:?}: NAME:/path with NAME of [a-z0-9-] (apps take named volumes only, never host paths)"
        )));
    }
    if !target.starts_with('/') {
        return Err(Error::invalid(format!(
            "volume {v:?}: the target must be an absolute path"
        )));
    }
    if let Some(o) = &opts {
        if !matches!(o.as_str(), "ro" | "rw") {
            return Err(Error::invalid(format!(
                "volume {v:?}: options are ro or rw"
            )));
        }
    }
    Ok((name.to_string(), target.to_string(), opts))
}

/// One app rendered for its stack: the service plus the top-level secrets
/// and volumes it uses. Stored with every deployment, so a rollback puts
/// back exactly what ran.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rendered {
    pub service: SandboxSpec,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secrets: BTreeMap<String, SecretDef>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub volumes: BTreeMap<String, NamedVolumeSpec>,
}

/// The top-level secret key an app's env reference renders to.
fn secret_key(app: &str, name: &str) -> String {
    format!("{app}.{name}")
}

fn volume_key(app: &str, name: &str) -> String {
    format!("{app}_{name}")
}

/// Whether the compose parser takes a service's `domains:` (the ingress
/// adds it). Until it does, an app's domains stay in the app record and
/// out of the rendered service.
pub fn compose_takes_domains() -> bool {
    serde_json::from_value::<SandboxSpec>(json!({"image": "x", "domains": []})).is_ok()
}

/// Render `spec` running `image` as its stack service. `notes` gets what
/// was left out and why.
pub fn render(spec: &AppSpec, image: &str, notes: &mut Vec<String>) -> Result<Rendered> {
    let mut environment = serde_json::Map::new();
    let mut secrets = BTreeMap::new();
    for (k, v) in spec.env.vars() {
        match v {
            EnvValue::Plain(s) => {
                environment.insert(k.to_string(), json!(s));
            }
            EnvValue::Secret { secret } => {
                let key = secret_key(&spec.name, secret);
                environment.insert(k.to_string(), json!({"secret": key}));
                secrets.insert(
                    key,
                    SecretDef {
                        external: true,
                        name: Some(secret.clone()),
                        ..Default::default()
                    },
                );
            }
        }
    }
    let mut volumes = BTreeMap::new();
    let mut mounts = Vec::new();
    for v in &spec.volumes {
        let (name, target, opts) = parse_volume(v)?;
        let key = volume_key(&spec.name, &name);
        mounts.push(match opts {
            Some(o) => format!("{key}:{target}:{o}"),
            None => format!("{key}:{target}"),
        });
        volumes.insert(key, NamedVolumeSpec::default());
    }
    let mut labels = serde_json::Map::new();
    labels.insert(LABEL_APP.into(), json!(spec.name));
    // A shared volume and two live replicas of a database do not mix:
    // apps with volumes replace stop-first, the rest start-first.
    let order = if spec.volumes.is_empty() {
        "start-first"
    } else {
        "stop-first"
    };
    let mut svc = json!({
        "image": image,
        "labels": labels,
        "deploy": {"replicas": spec.replicas, "update_config": {"order": order}},
    });
    if !environment.is_empty() {
        svc["environment"] = Value::Object(environment);
    }
    if !mounts.is_empty() {
        svc["volumes"] = json!(mounts);
    }
    if !spec.ports.is_empty() {
        svc["ports"] = json!(spec.ports);
    }
    if let Some(c) = &spec.command {
        svc["command"] = c.clone();
    }
    if let Some(h) = &spec.healthcheck {
        svc["healthcheck"] = h.clone();
    }
    if let Some(r) = &spec.resources {
        if let Some(c) = &r.cpus {
            svc["cpus"] = json!(c);
        }
        if let Some(m) = &r.memory {
            svc["mem_limit"] = json!(m);
        }
    }
    let parse = |v: Value| {
        serde_json::from_value::<SandboxSpec>(v)
            .map_err(|e| Error::invalid(format!("app {}: {e}", spec.name)))
    };
    let service = if spec.domains.is_empty() {
        parse(svc)?
    } else {
        let domains: Vec<Value> = spec
            .domains
            .iter()
            .map(|d| {
                let mut d = d.clone();
                if let (false, Some(p)) = (d.contains_key("port"), spec.port) {
                    d.insert("port".into(), json!(p));
                }
                Value::Object(d)
            })
            .collect();
        let mut with = svc.clone();
        with["domains"] = json!(domains);
        if compose_takes_domains() {
            parse(with)?
        } else {
            notes.push(format!(
                "domains ({}) are kept with the app; this isb has no ingress to serve them yet",
                spec.domains
                    .iter()
                    .filter_map(|d| d.get("host").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            parse(svc)?
        }
    };
    Ok(Rendered {
        service,
        secrets,
        volumes,
    })
}

/// The stack file with `app`'s service replaced by `r` (or removed, with
/// `None`), the rest untouched, and top-level secrets and volumes no
/// service uses any more dropped.
pub fn splice(
    current: Option<&crate::spec::ComposeFile>,
    stack: &str,
    app: &str,
    r: Option<&Rendered>,
) -> crate::spec::ComposeFile {
    let mut f = current.cloned().unwrap_or_default();
    f.name = Some(stack.to_string());
    f.services.remove(app);
    if let Some(r) = r {
        f.services.insert(app.to_string(), r.service.clone());
        for (k, v) in &r.secrets {
            f.secrets.insert(k.clone(), v.clone());
        }
        for (k, v) in &r.volumes {
            f.volumes.insert(k.clone(), v.clone());
        }
    }
    let used_secrets = crate::stack::secrets::used_keys(&f);
    f.secrets.retain(|k, _| used_secrets.contains(k));
    let used_volumes: std::collections::BTreeSet<String> = f
        .services
        .values()
        .flat_map(|s| s.volumes.iter().map(|v| v.source.clone()))
        .collect();
    f.volumes.retain(|k, _| used_volumes.contains(k));
    f
}

/// Merge `patch` into `base` (RFC 7396): `null` removes a key.
pub fn merge_patch(base: &mut Value, patch: &Value) {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            for (k, v) in p {
                if v.is_null() {
                    b.remove(k);
                } else {
                    merge_patch(b.entry(k.clone()).or_insert(Value::Null), v);
                }
            }
        }
        (b, p) => *b = p.clone(),
    }
}

/// The OCI reference `image` pinned to `digest`
/// (`docker:traefik/whoami:v1` -> `docker:traefik/whoami@sha256:...`), or
/// `None` for an image that is not from an OCI registry.
pub fn pin(image: &str, digest: &str) -> Option<String> {
    let (prefix, rest) = image.split_once(':')?;
    if !matches!(prefix, "docker" | "ghcr" | "quay" | "oci") || !digest.starts_with("sha256:") {
        return None;
    }
    let rest = rest.split('@').next().unwrap_or(rest);
    let (dir, last) = match rest.rsplit_once('/') {
        Some((d, l)) => (Some(d), l),
        None => (None, rest),
    };
    let last = last.split(':').next().unwrap_or(last);
    Some(match dir {
        Some(d) => format!("{prefix}:{d}/{last}@{digest}"),
        None => format!("{prefix}:{last}@{digest}"),
    })
}

/// Atomic write (temp file, fsync, rename), 0600.
pub(crate) fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension(format!("tmp-{}", git::random_hex(4)));
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(data)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tools take JSON; YAML here is only for brevity.
    fn spec(y: &str) -> AppSpec {
        try_spec(y).unwrap()
    }

    fn try_spec(y: &str) -> std::result::Result<AppSpec, serde_json::Error> {
        serde_json::from_value(serde_yaml_ng::from_str::<Value>(y).unwrap())
    }

    #[test]
    fn spec_forms_and_validation() {
        let a = spec(
            "name: web\nproject: shop\nsource: {image: 'docker:traefik/whoami'}\nenv: {A: '1', T: {secret: tok}}\n",
        );
        assert_eq!(a.environment, "production");
        assert_eq!(a.replicas, 1);
        assert_eq!(a.stack().unwrap(), "shop-production");
        a.validate().unwrap();
        let g = spec(
            "name: api\nproject: shop\nsource: {git: {url: 'https://h/o/r', ref: dev}}\nbuild: {builder: {type: railpack}}\n",
        );
        g.validate().unwrap();
        assert!(g.build.as_ref().unwrap().untrusted);
        let mut bad = g.clone();
        bad.build = None;
        assert!(bad.validate().is_err());
        let mut bad = a.clone();
        bad.volumes = vec!["/etc:/x".into()];
        assert!(bad.validate().is_err());
        bad.volumes = vec!["data:/var/lib/x".into()];
        bad.validate().unwrap();
        bad.domains = vec![serde_json::from_str(r#"{"host":"a.example.com"}"#).unwrap()];
        assert!(bad.validate().is_err(), "a domain needs a port");
        bad.port = Some(80);
        bad.validate().unwrap();
        let mut bad = a.clone();
        bad.project = "a-very-long-project-name".into();
        bad.environment = "staging-environment".into();
        assert!(bad.validate().is_err());
        assert!(try_spec("name: x\nproject: p\nsource: {image: x}\nbogus: 1\n").is_err());
        assert!(try_spec("name: x\nproject: p\nsource: {image: x, git: {url: u}}\n").is_err());
    }

    #[test]
    fn renders_one_service() {
        let a = spec(concat!(
            "name: web\nproject: shop\nsource: {image: 'docker:traefik/whoami'}\n",
            "env: \"# c\\nA=1\\nT=${{secret.tok}}\\n\"\n",
            "volumes: ['data:/data']\nports: ['127.0.0.1:18080:80']\nreplicas: 2\nport: 80\n",
            "command: [/whoami, --port, '80']\n",
            "healthcheck: {test: [CMD, /whoami, --help], interval: 5s}\n",
            "resources: {cpus: '1', memory: 256m}\n",
        ));
        let mut notes = vec![];
        let r = render(&a, "docker:traefik/whoami@sha256:ab", &mut notes).unwrap();
        let s = &r.service;
        assert_eq!(s.image, "docker:traefik/whoami@sha256:ab");
        assert_eq!(s.env["A"], "1");
        assert_eq!(s.env.secrets["T"], "web.tok");
        assert_eq!(r.secrets["web.tok"].name.as_deref(), Some("tok"));
        assert!(r.secrets["web.tok"].external);
        assert_eq!(s.volumes[0].source, "web_data");
        assert!(r.volumes.contains_key("web_data"));
        assert_eq!(s.replicas(), 2);
        assert_eq!(s.labels[LABEL_APP], "web");
        assert_eq!(s.cpus.as_deref(), Some("1"));
        assert_eq!(s.memory.as_deref(), Some("256m"));
        assert!(s.healthcheck.is_some());
        assert_eq!(s.ports.len(), 1);
        assert!(notes.is_empty());
    }

    #[test]
    fn domains_follow_the_parser() {
        let mut a = spec("name: web\nproject: shop\nsource: {image: x}\nport: 8080\n");
        a.domains = vec![serde_json::from_str(r#"{"host":"shop.example.com"}"#).unwrap()];
        let mut notes = vec![];
        let r = render(&a, "x", &mut notes).unwrap();
        if compose_takes_domains() {
            let v = serde_json::to_value(&r.service).unwrap();
            assert_eq!(v["domains"][0]["port"], 8080);
            assert!(notes.is_empty());
        } else {
            assert_eq!(notes.len(), 1, "{notes:?}");
        }
    }

    #[test]
    fn splice_touches_only_the_app() {
        let mut notes = vec![];
        let web = spec(
            "name: web\nproject: shop\nsource: {image: x}\nenv: {T: {secret: tok}}\nvolumes: ['d:/d']\n",
        );
        let api = spec("name: api\nproject: shop\nsource: {image: y}\nenv: {T: {secret: tok}}\n");
        let rw = render(&web, "x", &mut notes).unwrap();
        let ra = render(&api, "y", &mut notes).unwrap();
        let f1 = splice(None, "shop-production", "web", Some(&rw));
        let f2 = splice(Some(&f1), "shop-production", "api", Some(&ra));
        assert_eq!(f2.services.len(), 2);
        assert_eq!(f2.services["web"], f1.services["web"]);
        assert_eq!(
            f2.secrets.keys().collect::<Vec<_>>(),
            ["api.tok", "web.tok"]
        );
        // The revision of the app not deployed stays the same.
        let def = |f: &crate::spec::ComposeFile| crate::stack::StackDef {
            name: "shop-production".into(),
            org: OrgId::default_org(),
            file: f.clone(),
            base_dir: "/".into(),
            secrets: Default::default(),
            force: Default::default(),
            images: Default::default(),
            deployed_at: 0,
            deployed_by: String::new(),
            previous: None,
        };
        let mut web2 = web.clone();
        web2.env.set("B", EnvValue::Plain("2".into()));
        let rw2 = render(&web2, "x", &mut notes).unwrap();
        let f3 = splice(Some(&f2), "shop-production", "web", Some(&rw2));
        assert_eq!(
            def(&f2).revision("api").unwrap(),
            def(&f3).revision("api").unwrap()
        );
        assert_ne!(
            def(&f2).revision("web").unwrap(),
            def(&f3).revision("web").unwrap()
        );
        // Removing web drops its secret key and volume, keeps api's.
        let f4 = splice(Some(&f3), "shop-production", "web", None);
        assert_eq!(f4.services.keys().collect::<Vec<_>>(), ["api"]);
        assert_eq!(f4.secrets.keys().collect::<Vec<_>>(), ["api.tok"]);
        assert!(f4.volumes.is_empty());
    }

    #[test]
    fn pins_digests() {
        let d = "sha256:abc";
        assert_eq!(
            pin("docker:traefik/whoami", d).unwrap(),
            "docker:traefik/whoami@sha256:abc"
        );
        assert_eq!(
            pin("docker:nginx:1.27", d).unwrap(),
            "docker:nginx@sha256:abc"
        );
        assert_eq!(
            pin("oci:reg.example.com:5000/team/app:v2", d).unwrap(),
            "oci:reg.example.com:5000/team/app@sha256:abc"
        );
        assert_eq!(
            pin("ghcr:o/a@sha256:old", d).unwrap(),
            "ghcr:o/a@sha256:abc"
        );
        assert!(pin("dev-base", d).is_none());
        assert!(pin("images:debian/12", d).is_none());
    }

    #[test]
    fn merge_patch_rfc7396() {
        let mut b = json!({"a": 1, "b": {"c": 2, "d": 3}});
        merge_patch(&mut b, &json!({"a": null, "b": {"c": 9}, "e": [1]}));
        assert_eq!(b, json!({"b": {"c": 9, "d": 3}, "e": [1]}));
    }
}
