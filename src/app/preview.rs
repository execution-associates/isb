//! Preview deployments: one per open pull request, built from its head and
//! served on its own URL until it closes.
//!
//! An app opts in with `previews: {enabled: true, ...}`. A pull request
//! event on the app's webhook (GitHub and Gitea/Forgejo `pull_request`,
//! GitLab `Merge Request Hook`) against a watched base branch then:
//! - opened, reopened, new commits: fetches the forge's ref for the
//!   request's head (`refs/pull/<n>/head`, `refs/merge-requests/<n>/head`)
//!   with the app's hardened git code, builds it (tag `pr-<n>-<sha>`) and
//!   deploys it as the app's service in the preview stack
//!   `<project>-<env>-pr-<n>`;
//! - closed or merged: removes that service, its volumes, its images and
//!   its records.
//!
//! A preview never touches production: its own stack, its own volumes
//! (`<stack>_<app>_<name>`), no published host ports, its own build cache,
//! and an environment of its own (the app's only with `inherit_env`).
//! Pull requests from forks carry code nobody with push access wrote: they
//! get no preview unless `forks: true`, and then build in a VM and receive
//! none of the app's secrets, only the preview secrets listed in
//! `fork_secrets`.
//!
//! ```text
//! apps/<app>/previews/<n>/preview.json
//! apps/<app>/previews/<n>/deployments/<id>.json, <id>.log
//! sources/<app>/previews/<n>/repo
//! ```

use std::collections::BTreeSet;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::deploy::{Commit, Deployment, Status, Trigger};
use super::forge::{self, ForgeKind, StatusSettings};
use super::webhook::{PrAction, Provider, PullRequest};
use super::{App, AppSpec, EnvFile, EnvValue, Rendered, Resources, Source, git};
use crate::build::BuildRequest;
use crate::error::{Error, Result};
use crate::org::OrgId;

/// Label (`user.isb.preview`) on a preview's instances: its pull request.
pub const LABEL_PREVIEW: &str = "isb.preview";

/// Preview deployment records kept per preview.
const KEEP_DEPLOYMENTS: usize = 10;

/// What an app sets for its previews.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewSettings {
    #[serde(default)]
    pub enabled: bool,
    /// Base branches whose pull requests get previews. Default: the app's
    /// git ref.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub branches: Vec<String>,
    /// At most this many previews at once (default 3); a pull request over
    /// the limit gets none until another closes.
    #[serde(default = "default_max")]
    pub max: u32,
    /// The preview's environment: `.env` text or a `{KEY: value | {secret:
    /// NAME}}` map, laid over the app's when `inherit_env`.
    #[serde(default, skip_serializing_if = "env_is_empty")]
    pub env: EnvFile,
    /// Start from the app's environment (default false: a preview gets only
    /// `env`, so it never points at production's database by accident).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub inherit_env: bool,
    /// `auto` (default: a generated sslip.io name) or `*.<suffix>` for
    /// `<app>-pr-<n>.<suffix>`, within the org's allowed domains.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    /// The port to route to; default the app's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default = "one")]
    pub replicas: u32,
    /// Per replica; default the app's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<Resources>,
    /// Remove a preview that has not been updated for this long (`7d`,
    /// `36h`). Default: kept until its pull request closes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl: Option<String>,
    /// Previews for pull requests from forks (default false). They build in
    /// a VM and get none of the app's secrets.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub forks: bool,
    /// Secrets in `env` a fork's preview may receive. Any other secret is
    /// left out of a fork's preview.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fork_secrets: Vec<String>,
    /// Post the preview's state and URL as a commit status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<StatusSettings>,
}

fn default_max() -> u32 {
    3
}

fn one() -> u32 {
    1
}

fn env_is_empty(e: &EnvFile) -> bool {
    e.vars().next().is_none()
}

impl PreviewSettings {
    /// Check the settings against their app.
    pub fn validate(&self, app: &AppSpec) -> Result<()> {
        let bad = |m: String| Err(Error::invalid(format!("previews: {m}")));
        if self.enabled && !matches!(app.source, Source::Git(_)) {
            return bad("only an app with a git source has pull requests".into());
        }
        for b in &self.branches {
            git::validate_ref(b)?;
            if git::is_sha(b) {
                return bad(format!("branch {b:?} is a commit, not a branch"));
            }
        }
        if !(1..=50).contains(&self.max) {
            return bad("max: 1 to 50".into());
        }
        if !(1..=10).contains(&self.replicas) {
            return bad("replicas: 1 to 10".into());
        }
        if let Some(d) = &self.domain {
            preview_host(d, "x", 1).map_err(|e| Error::invalid(format!("previews: {e}")))?;
        }
        if self.enabled && self.port.or(app.port).is_none() {
            return bad("set the app's port (or previews.port): a preview is served on it".into());
        }
        if let Some(t) = &self.ttl {
            let d = crate::flex::parse_duration(t)
                .map_err(|e| Error::invalid(format!("previews.ttl: {e}")))?;
            if d < Duration::from_secs(600) {
                return bad("ttl: at least 10m".into());
            }
        }
        for s in &self.fork_secrets {
            crate::secrets::validate_name(s)?;
        }
        if let Some(s) = &self.status {
            s.validate()?;
        }
        Ok(())
    }

    /// Secrets the settings name (they must exist).
    pub fn secret_names(&self) -> Vec<String> {
        let mut n = self.env.secret_names();
        if let Some(s) = &self.status {
            n.push(s.token_secret.clone());
        }
        n
    }

    /// The base branches watched: `branches`, or the app's ref.
    pub fn bases(&self, app: &AppSpec) -> Vec<String> {
        if !self.branches.is_empty() {
            return self.branches.clone();
        }
        match &app.source {
            Source::Git(g) => vec![
                g.reference
                    .strip_prefix("refs/heads/")
                    .unwrap_or(&g.reference)
                    .to_string(),
            ],
            Source::Image(_) => vec![],
        }
    }

    fn ttl(&self) -> Option<Duration> {
        self.ttl
            .as_deref()
            .and_then(|t| crate::flex::parse_duration(t).ok())
    }
}

/// `pr-<n>` or `...-pr-<n>`: a name previews own.
pub fn is_pr_suffix(s: &str) -> bool {
    let tail = s.rsplit("pr-").next().unwrap_or("");
    let n_ok = !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit());
    n_ok && (s == format!("pr-{tail}") || s.ends_with(&format!("-pr-{tail}")))
}

/// The stack a preview runs in: `<project>-<env>-pr-<n>`, or
/// `<project>-pr-<n>` when that is too long for a stack or its instances.
pub fn preview_stack(project: &str, environment: &str, app: &str, n: u64) -> Result<String> {
    for s in [
        format!("{project}-{environment}-pr-{n}"),
        format!("{project}-pr-{n}"),
    ] {
        if crate::stack::validate_stack_name(&s).is_ok()
            && crate::stack::instance_name(&s, app, 100, "0000").is_ok()
        {
            return Ok(s);
        }
    }
    Err(Error::invalid(format!(
        "app {app}: no preview stack name fits for pull request {n}; shorten the app or project name"
    )))
}

/// A preview's image tag.
pub fn preview_tag(n: u64, sha: &str) -> String {
    format!("pr-{n}-{sha}")
}

/// The build cache a preview uses: one per pull request for forks (removed
/// with the preview), one shared by the app's other previews. Never
/// production's.
pub fn cache_key(app: &str, n: u64, fork: bool) -> String {
    if fork {
        format!("{app}-pr-{n}")
    } else {
        format!("{app}-preview")
    }
}

/// The domain host for a preview: `auto`, or `<app>-pr-<n>.<suffix>` for
/// a `*.<suffix>` pattern.
pub fn preview_host(pattern: &str, app: &str, n: u64) -> std::result::Result<String, String> {
    let p = pattern.trim().to_ascii_lowercase();
    if p == "auto" {
        return Ok(p);
    }
    let Some(suffix) = p.strip_prefix("*.") else {
        return Err(format!(
            "domain {pattern:?}: `auto` or a wildcard suffix like *.preview.example.com"
        ));
    };
    let label_ok = |l: &str| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    };
    if suffix.split('.').count() < 2 || !suffix.split('.').all(label_ok) {
        return Err(format!("domain {pattern:?}: *.<a DNS name with a dot>"));
    }
    let host = format!("{app}-pr-{n}.{suffix}");
    if host.len() > 253 || !label_ok(&format!("{app}-pr-{n}")) {
        return Err(format!("domain {pattern:?}: {host} is not a valid name"));
    }
    Ok(host)
}

/// The app as its preview runs: the preview's environment, replicas,
/// resources and domain, no published host ports, no preview settings.
/// `notes` says what was left out and why.
pub fn preview_spec(
    app: &AppSpec,
    s: &PreviewSettings,
    n: u64,
    fork: bool,
    notes: &mut Vec<String>,
) -> AppSpec {
    let mut spec = app.clone();
    let mut env = EnvFile::default();
    if s.inherit_env {
        for (k, v) in app.env.vars() {
            match v {
                EnvValue::Secret { secret } if fork => {
                    notes.push(format!(
                        "{k}: the app's secret {secret} is withheld from a fork's pull request"
                    ));
                }
                v => env.set(k, v.clone()),
            }
        }
    }
    for (k, v) in s.env.vars() {
        match v {
            EnvValue::Secret { secret } if fork && !s.fork_secrets.contains(secret) => {
                env.remove(k);
                notes.push(format!(
                    "{k}: secret {secret} is not in fork_secrets; withheld from a fork's pull request"
                ));
            }
            v => env.set(k, v.clone()),
        }
    }
    spec.env = env;
    // Files are secret values too: they follow the env's rules, so a preview
    // gets the app's only with `inherit_env`, and a fork's only those in
    // `fork_secrets`.
    spec.files.retain(|f| {
        let keep = s.inherit_env && (!fork || s.fork_secrets.contains(&f.secret));
        if !keep {
            notes.push(format!(
                "{}: the app's file from secret {} is left out{}",
                f.path,
                f.secret,
                if s.inherit_env {
                    "; not in fork_secrets, withheld from a fork's pull request"
                } else {
                    " (previews take the app's files only with inherit_env)"
                }
            ));
        }
        keep
    });
    if !spec.ports.is_empty() {
        notes.push(format!(
            "published ports ({}) are production's; previews are served on their domain only",
            spec.ports.join(", ")
        ));
        spec.ports.clear();
    }
    spec.replicas = s.replicas;
    if s.resources.is_some() {
        spec.resources = s.resources.clone();
    }
    spec.port = s.port.or(app.port);
    spec.domains.clear();
    let pattern = s.domain.as_deref().unwrap_or("auto");
    match (preview_host(pattern, &app.name, n), spec.port) {
        (Ok(host), Some(port)) => {
            let mut d = serde_json::Map::new();
            d.insert("host".into(), json!(host));
            d.insert("port".into(), json!(port));
            spec.domains.push(d);
        }
        (Err(e), _) => notes.push(format!("no domain: {e}")),
        (_, None) => notes.push("no domain: the app has no port".into()),
    }
    spec.previews = None;
    spec
}

/// Render a preview's service: the app's rendering, labelled with its pull
/// request, its named volumes made its own (`<stack>_<app>_<name>`).
pub fn render_preview(
    spec: &AppSpec,
    image: &str,
    stack: &str,
    n: u64,
    notes: &mut Vec<String>,
) -> Result<Rendered> {
    let mut r = super::render(spec, image, notes)?;
    r.service.labels.insert(LABEL_PREVIEW.into(), n.to_string());
    for (key, v) in r.volumes.iter_mut() {
        v.name = Some(format!("{stack}_{key}"));
    }
    Ok(r)
}

/// One preview: a pull request of an app and what runs for it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preview {
    pub app: String,
    pub number: u64,
    pub stack: String,
    /// `github`, `gitea` or `gitlab`: which forge refs and API it uses.
    pub provider: String,
    pub fork: bool,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub head_ref: String,
    #[serde(default)]
    pub base_ref: String,
    /// The head commit the last event named.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    /// What runs: the commit, the image, the URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    pub next_deployment: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<u64>,
    /// Being removed: no more deploys.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub removing: bool,
}

fn provider_name(p: Provider) -> &'static str {
    match p {
        Provider::GitHub => "github",
        Provider::Gitea => "gitea",
        Provider::GitLab => "gitlab",
        Provider::Generic => "generic",
    }
}

fn provider_of(s: &str) -> Provider {
    match s {
        "gitlab" => Provider::GitLab,
        "gitea" => Provider::Gitea,
        "github" => Provider::GitHub,
        _ => Provider::Generic,
    }
}

/// What a pull request event led to.
pub enum Requested {
    Queued(Box<Preview>, Box<Deployment>),
    /// The app has `max` previews already.
    Limit(u32),
    Removing,
    /// A sync naming the commit the preview already has (Gitea sends one
    /// right after opening).
    Unchanged(String),
}

/// A teardown: state dir, org, app, pull request.
type TeardownKey = (PathBuf, String, String, u64);

/// Teardowns in progress, so the janitor does not start a second one.
fn tearing_down() -> &'static Mutex<BTreeSet<TeardownKey>> {
    static S: OnceLock<Mutex<BTreeSet<TeardownKey>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

fn queue_key(app: &str, n: u64) -> String {
    format!("{app}#pr-{n}")
}

impl super::Apps {
    // --- records ---------------------------------------------------------

    fn previews_dir(&self, org: &OrgId, app: &str) -> PathBuf {
        self.app_dir(org, app).join("previews")
    }

    fn preview_dir(&self, org: &OrgId, app: &str, n: u64) -> PathBuf {
        self.previews_dir(org, app).join(n.to_string())
    }

    fn preview_source_dir(&self, org: &OrgId, app: &str, n: u64) -> PathBuf {
        self.source_dir(org, app)
            .join("previews")
            .join(n.to_string())
    }

    fn pdep_path(&self, org: &OrgId, app: &str, n: u64, id: u64) -> PathBuf {
        self.preview_dir(org, app, n)
            .join("deployments")
            .join(format!("{id}.json"))
    }

    fn plog_path(&self, org: &OrgId, app: &str, n: u64, id: u64) -> PathBuf {
        self.preview_dir(org, app, n)
            .join("deployments")
            .join(format!("{id}.log"))
    }

    pub fn preview_get(&self, org: &OrgId, app: &str, n: u64) -> Result<Preview> {
        super::validate_app_name(app)?;
        match std::fs::read(self.preview_dir(org, app, n).join("preview.json")) {
            Ok(b) => Ok(serde_json::from_slice(&b)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::NotFound(format!(
                "preview of pull request {n} of app {app}"
            ))),
            Err(e) => Err(e.into()),
        }
    }

    /// An app's previews, by pull request number.
    pub fn preview_list(&self, org: &OrgId, app: &str) -> Result<Vec<Preview>> {
        super::validate_app_name(app)?;
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir(self.previews_dir(org, app)) else {
            return Ok(out);
        };
        for e in rd.flatten() {
            let Some(n) = e.file_name().to_str().and_then(|s| s.parse::<u64>().ok()) else {
                continue;
            };
            match self.preview_get(org, app, n) {
                Ok(p) => out.push(p),
                Err(e) if e.is_not_found() => {}
                Err(e) => eprintln!("isb serve: app {app}: preview {n}: {e}"),
            }
        }
        out.sort_by_key(|p| p.number);
        Ok(out)
    }

    fn preview_save(&self, org: &OrgId, p: &Preview) -> Result<()> {
        super::write_atomic(
            &self.preview_dir(org, &p.app, p.number).join("preview.json"),
            &serde_json::to_vec_pretty(p)?,
        )
    }

    fn pdep_save(&self, org: &OrgId, n: u64, d: &Deployment) -> Result<()> {
        super::write_atomic(
            &self.pdep_path(org, &d.app, n, d.id),
            &serde_json::to_vec_pretty(d)?,
        )
    }

    pub fn preview_deployment(
        &self,
        org: &OrgId,
        app: &str,
        n: u64,
        id: u64,
    ) -> Result<Deployment> {
        super::validate_app_name(app)?;
        match std::fs::read(self.pdep_path(org, app, n, id)) {
            Ok(b) => Ok(serde_json::from_slice(&b)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::NotFound(format!(
                "deployment {id} of the preview of pull request {n} of app {app}"
            ))),
            Err(e) => Err(e.into()),
        }
    }

    /// Newest first.
    pub fn preview_deployments(&self, org: &OrgId, app: &str, n: u64) -> Result<Vec<Deployment>> {
        let mut ids: Vec<u64> =
            match std::fs::read_dir(self.preview_dir(org, app, n).join("deployments")) {
                Ok(rd) => rd
                    .flatten()
                    .filter_map(|e| {
                        e.file_name()
                            .into_string()
                            .ok()?
                            .strip_suffix(".json")?
                            .parse()
                            .ok()
                    })
                    .collect(),
                Err(_) => vec![],
            };
        ids.sort_unstable_by(|a, b| b.cmp(a));
        Ok(ids
            .into_iter()
            .filter_map(|id| self.preview_deployment(org, app, n, id).ok())
            .collect())
    }

    /// A preview deployment's log from byte `offset`: `(text, next offset,
    /// finished)`.
    pub fn preview_log(
        &self,
        org: &OrgId,
        app: &str,
        n: u64,
        id: u64,
        offset: u64,
    ) -> Result<(String, u64, bool)> {
        let d = self.preview_deployment(org, app, n, id)?;
        let b = std::fs::read(self.plog_path(org, app, n, id)).unwrap_or_default();
        let start = (offset as usize).min(b.len());
        Ok((
            String::from_utf8_lossy(&b[start..]).into_owned(),
            b.len() as u64,
            d.status.finished(),
        ))
    }

    /// Wait until a preview deployment finishes, or `timeout`.
    pub fn preview_wait(
        &self,
        org: &OrgId,
        app: &str,
        n: u64,
        id: u64,
        timeout: Duration,
    ) -> Result<Deployment> {
        let started = Instant::now();
        loop {
            let d = self.preview_deployment(org, app, n, id)?;
            if d.status.finished() || started.elapsed() >= timeout {
                return Ok(d);
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    /// A preview as the tools show it: the record, plus its last
    /// deployment's status.
    pub fn preview_json(&self, org: &OrgId, p: &Preview) -> Value {
        let mut v = serde_json::to_value(p).unwrap_or_default();
        let last = self
            .preview_deployments(org, &p.app, p.number)
            .ok()
            .and_then(|d| d.into_iter().next());
        v["status"] = json!(if p.removing {
            "removing".to_string()
        } else {
            last.as_ref()
                .map(|d| format!("{:?}", d.status).to_lowercase())
                .unwrap_or_else(|| "new".into())
        });
        v["last_deployment"] = last.map(|d| d.summary()).unwrap_or(Value::Null);
        v
    }

    // --- events ----------------------------------------------------------

    fn pevent(&self, org: &OrgId, p: &Preview, level: &str, message: String) {
        let q = crate::stack::qualified(org, &p.stack);
        self.inner.ctl.note_service(
            level,
            &q,
            &p.app,
            format!("app {} preview #{}: {message}", p.app, p.number),
        );
    }

    fn pkind(&self, org: &OrgId, p: &Preview, kind: &str, level: &str, message: String) {
        let q = crate::stack::qualified(org, &p.stack);
        self.inner.ctl.event(
            kind,
            level,
            &q,
            &p.app,
            format!("app {} preview #{}: {message}", p.app, p.number),
        );
    }

    // --- webhooks --------------------------------------------------------

    /// A verified pull request event for `app`: `(status, body)`.
    pub(super) fn preview_webhook(
        &self,
        org: &OrgId,
        app: &App,
        provider: Provider,
        by: &str,
        pr: &PullRequest,
    ) -> (u16, Value) {
        let name = &app.spec.name;
        let n = pr.number;
        let ignored = |why: String| (200, json!({"ignored": why}));
        if provider == Provider::Generic {
            return ignored("pull requests need a forge's signature, not ?token=".into());
        }
        // A closed request's preview goes even if previews were turned off
        // since it was made.
        if let PrAction::Close { merged } = pr.action {
            if self.preview_get(org, name, n).is_err() {
                return ignored(format!("pull request {n} has no preview"));
            }
            let why = if merged { "merged" } else { "closed" };
            return match self.preview_remove(org, name, n, why, false) {
                Ok(()) => (202, json!({"preview": n, "removing": true})),
                Err(e) => (500, json!({"error": "preview", "message": e.to_string()})),
            };
        }
        let Some(s) = app.spec.previews.as_ref().filter(|s| s.enabled) else {
            return ignored(format!("previews are off for app {name}"));
        };
        match pr.action {
            PrAction::Other | PrAction::Close { .. } => {
                ignored(format!("pull request {n}: {}", pr.raw_action))
            }
            PrAction::Open | PrAction::Sync => {
                let bases = s.bases(&app.spec);
                if !bases.contains(&pr.base_ref) {
                    return ignored(format!(
                        "pull request {n} targets {}, not {}",
                        pr.base_ref,
                        bases.join(", ")
                    ));
                }
                if pr.fork && !s.forks {
                    return ignored(format!(
                        "pull request {n} is from a fork; previews for forks are off (previews.forks)"
                    ));
                }
                let requested = format!(
                    "pull request #{n} {} {}: {}",
                    pr.raw_action,
                    pr.head_sha.as_deref().unwrap_or("?"),
                    pr.title
                );
                match self.preview_request(org, name, provider, pr, Trigger::Webhook, by, requested)
                {
                    Ok(Requested::Queued(_, d)) => (
                        202,
                        json!({"preview": n, "deployment": d.id, "status": d.status}),
                    ),
                    Ok(Requested::Limit(max)) => {
                        if let Some(sha) = &pr.head_sha {
                            let p = Preview {
                                app: name.clone(),
                                number: n,
                                stack: String::new(),
                                provider: provider_name(provider).into(),
                                fork: pr.fork,
                                title: pr.title.clone(),
                                head_ref: pr.head_ref.clone(),
                                base_ref: pr.base_ref.clone(),
                                head_sha: Some(sha.clone()),
                                sha: None,
                                image: None,
                                url: None,
                                created_at: 0,
                                updated_at: 0,
                                next_deployment: 1,
                                current: None,
                                removing: false,
                            };
                            self.forge_status(
                                org,
                                app,
                                &p,
                                sha,
                                "error",
                                None,
                                &format!("no preview: the app has its {max} previews already"),
                                &mut |_| {},
                            );
                        }
                        ignored(format!(
                            "app {name} has its {max} previews already (previews.max)"
                        ))
                    }
                    Ok(Requested::Unchanged(sha)) => ignored(format!(
                        "the preview of pull request {n} is at {sha} already"
                    )),
                    Ok(Requested::Removing) => {
                        ignored(format!("the preview of pull request {n} is being removed"))
                    }
                    Err(e) => (500, json!({"error": "preview", "message": e.to_string()})),
                }
            }
        }
    }

    /// Create or update the preview of `pr` and queue a deploy of it.
    #[allow(clippy::too_many_arguments)]
    pub fn preview_request(
        &self,
        org: &OrgId,
        name: &str,
        provider: Provider,
        pr: &PullRequest,
        trigger: Trigger,
        by: &str,
        requested: String,
    ) -> Result<Requested> {
        let n = pr.number;
        let (p, created) = {
            let _g = self.inner.edit.lock().unwrap();
            let app = self.get(org, name)?;
            let s = app
                .spec
                .previews
                .clone()
                .filter(|s| s.enabled)
                .ok_or_else(|| Error::invalid(format!("previews are off for app {name}")))?;
            let now = crate::stack::now_secs();
            let (mut p, created) = match self.preview_get(org, name, n) {
                Ok(p) if p.removing => return Ok(Requested::Removing),
                Ok(p)
                    if pr.action == PrAction::Sync
                        && pr.head_sha.is_some()
                        && p.head_sha == pr.head_sha =>
                {
                    return Ok(Requested::Unchanged(p.head_sha.unwrap_or_default()));
                }
                Ok(p) => (p, false),
                Err(e) if e.is_not_found() => {
                    let live = self
                        .preview_list(org, name)?
                        .iter()
                        .filter(|p| !p.removing)
                        .count();
                    if live >= s.max as usize {
                        return Ok(Requested::Limit(s.max));
                    }
                    let stack = preview_stack(&app.spec.project, &app.spec.environment, name, n)?;
                    (
                        Preview {
                            app: name.into(),
                            number: n,
                            stack,
                            provider: provider_name(provider).into(),
                            fork: pr.fork,
                            title: String::new(),
                            head_ref: String::new(),
                            base_ref: String::new(),
                            head_sha: None,
                            sha: None,
                            image: None,
                            url: None,
                            created_at: now,
                            updated_at: now,
                            next_deployment: 1,
                            current: None,
                            removing: false,
                        },
                        true,
                    )
                }
                Err(e) => return Err(e),
            };
            // Once a fork, always a fork: the head cannot move repositories.
            p.fork |= pr.fork;
            p.title = pr.title.clone();
            p.head_ref = pr.head_ref.clone();
            p.base_ref = pr.base_ref.clone();
            p.head_sha = pr.head_sha.clone();
            p.updated_at = now;
            self.preview_save(org, &p)?;
            (p, created)
        };
        if created {
            self.pkind(
                org,
                &p,
                "preview.created",
                "info",
                format!("created for {} ({})", p.head_ref, p.title),
            );
        }
        let d = self.preview_enqueue(org, name, n, trigger, by, Some(requested))?;
        Ok(Requested::Queued(Box::new(p), Box::new(d)))
    }

    /// Build and deploy a preview's head again.
    pub fn preview_redeploy(
        &self,
        org: &OrgId,
        name: &str,
        n: u64,
        trigger: Trigger,
        by: &str,
    ) -> Result<Deployment> {
        let app = self.get(org, name)?;
        if !app.spec.previews.as_ref().is_some_and(|s| s.enabled) {
            return Err(Error::invalid(format!("previews are off for app {name}")));
        }
        let p = self.preview_get(org, name, n)?;
        if p.removing {
            return Err(Error::invalid(format!(
                "the preview of pull request {n} is being removed"
            )));
        }
        self.preview_enqueue(org, name, n, trigger, by, Some("redeploy".into()))
    }

    // --- the deploy queue ------------------------------------------------

    fn preview_enqueue(
        &self,
        org: &OrgId,
        name: &str,
        n: u64,
        trigger: Trigger,
        by: &str,
        requested: Option<String>,
    ) -> Result<Deployment> {
        let (p, dep) = {
            let _g = self.inner.edit.lock().unwrap();
            let mut p = self.preview_get(org, name, n)?;
            if p.removing {
                return Err(Error::invalid(format!(
                    "the preview of pull request {n} is being removed"
                )));
            }
            let id = p.next_deployment;
            p.next_deployment += 1;
            self.preview_save(org, &p)?;
            let d = Deployment {
                id,
                app: name.into(),
                trigger,
                by: by.into(),
                status: Status::Queued,
                requested,
                rollback_of: None,
                commit: None,
                image: None,
                digest: None,
                error: None,
                created_at: crate::stack::controller::now_ms(),
                started_at: None,
                finished_at: None,
                rendered: None,
            };
            self.pdep_save(org, n, &d)?;
            self.preview_prune(org, &p);
            (p, d)
        };
        self.pevent(
            org,
            &p,
            "info",
            format!("deployment {} queued by {by}", dep.id),
        );
        let key = (org.clone(), queue_key(name, n));
        let start = {
            let mut qs = self.inner.queues.lock().unwrap();
            let q = qs.entry(key).or_default();
            if let Some(old) = q.next.replace(dep.id) {
                self.supersede(org, name, n, old, dep.id);
            }
            !std::mem::replace(&mut q.running, true)
        };
        if start {
            let me = self.clone();
            let (org, name) = (org.clone(), name.to_string());
            std::thread::spawn(move || me.preview_drain(&org, &name, n));
        }
        Ok(dep)
    }

    fn supersede(&self, org: &OrgId, name: &str, n: u64, old: u64, by: u64) {
        if let Ok(mut d) = self.preview_deployment(org, name, n, old) {
            if d.advance(Status::Superseded).is_ok() {
                d.error = Some(if by == 0 {
                    "the preview is being removed".into()
                } else {
                    format!("superseded by deployment {by}")
                });
                let _ = self.pdep_save(org, n, &d);
            }
        }
    }

    fn preview_drain(&self, org: &OrgId, name: &str, n: u64) {
        let key = (org.clone(), queue_key(name, n));
        loop {
            let next = {
                let mut qs = self.inner.queues.lock().unwrap();
                let q = qs.entry(key.clone()).or_default();
                match q.next.take() {
                    Some(id) => id,
                    None => {
                        qs.remove(&key);
                        return;
                    }
                }
            };
            self.preview_run(org, name, n, next);
        }
    }

    fn preview_run(&self, org: &OrgId, name: &str, n: u64, id: u64) {
        let (Ok(mut dep), Ok(p)) = (
            self.preview_deployment(org, name, n, id),
            self.preview_get(org, name, n),
        ) else {
            return;
        };
        let mut log = match PreviewLog::open(self, org, &p, id) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("isb serve: app {name} preview #{n}: deployment {id}: no log: {e}");
                return;
            }
        };
        let r = self.preview_pipeline(org, name, n, &mut dep, &mut log);
        let p = self.preview_get(org, name, n).unwrap_or(p);
        match &r {
            Ok(()) => {
                let url = p.url.clone().unwrap_or_else(|| "no URL".into());
                log.line(&format!("deployment {id} done: {url}"));
                self.pkind(
                    org,
                    &p,
                    "deploy.succeeded",
                    "info",
                    format!("deployment {id} done: {url}"),
                );
            }
            Err(e) => {
                log.line(&format!("deployment {id} failed: {e}"));
                dep.error = Some(e.to_string());
                let _ = dep.advance(Status::Failed);
                self.pkind(
                    org,
                    &p,
                    "deploy.failed",
                    "error",
                    format!("deployment {id} failed: {e}"),
                );
                if let (Ok(app), Some(sha)) = (
                    self.get(org, name),
                    dep.commit
                        .as_ref()
                        .map(|c| c.sha.clone())
                        .or(p.head_sha.clone()),
                ) {
                    let first: String = e.to_string().chars().take(120).collect();
                    self.forge_status(
                        org,
                        &app,
                        &p,
                        &sha,
                        "failure",
                        None,
                        &format!("preview failed: {first}"),
                        &mut |l| log.line(l),
                    );
                }
            }
        }
        if let Err(e) = self.pdep_save(org, n, &dep) {
            eprintln!("isb serve: app {name} preview #{n}: deployment {id}: {e}");
        }
    }

    fn pstatus(&self, org: &OrgId, n: u64, dep: &mut Deployment, s: Status) -> Result<()> {
        dep.advance(s)?;
        self.pdep_save(org, n, dep)
    }

    fn preview_pipeline(
        &self,
        org: &OrgId,
        name: &str,
        n: u64,
        dep: &mut Deployment,
        log: &mut PreviewLog,
    ) -> Result<()> {
        self.pstatus(org, n, dep, Status::Building)?;
        let app = self.get(org, name)?;
        let s = app
            .spec
            .previews
            .clone()
            .filter(|s| s.enabled)
            .ok_or_else(|| Error::invalid(format!("previews are off for app {name}")))?;
        let p = self.preview_get(org, name, n)?;
        let Source::Git(g) = &app.spec.source else {
            return Err(Error::invalid(format!("app {name} has no git source")));
        };
        if let Some(sha) = &p.head_sha {
            self.forge_status(
                org,
                &app,
                &p,
                sha,
                "pending",
                None,
                "building the preview",
                &mut |l| log.line(l),
            );
        }
        let mut src = g.clone();
        src.reference = PullRequest::head_ref_in_base(provider_of(&p.provider), n);
        let creds = self.credentials(org, &g.auth)?;
        let co = git::fetch(
            &src,
            &creds,
            &self.preview_source_dir(org, name, n),
            &mut |l| log.line(l),
        )?;
        if let Some(want) = &p.head_sha {
            if !co.sha.eq_ignore_ascii_case(want) {
                log.line(&format!(
                    "note: the pull request's head is {} now, not {want} as the event said",
                    co.sha
                ));
            }
        }
        dep.commit = Some(Commit {
            sha: co.sha.clone(),
            message: co.message.clone(),
        });
        self.pdep_save(org, n, dep)?;
        let b = app
            .spec
            .build
            .as_ref()
            .ok_or_else(|| Error::invalid("a git source needs build settings"))?;
        let untrusted = b.untrusted || p.fork;
        if p.fork && !b.untrusted {
            log.line("a fork's pull request: building in a VM");
        }
        let req = BuildRequest {
            org: org.clone(),
            app: name.into(),
            context: co.dir.clone(),
            subdir: g.subdir.clone(),
            builder: b.builder.clone(),
            args: b.args.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            tag: preview_tag(n, &co.sha),
            untrusted,
            cache: Some(cache_key(name, n, p.fork)),
        };
        log.line(&format!("building {} with {:?}", co.sha, b.builder));
        let built = (self.inner.build)(&self.inner.client, &req, &mut |l| log.line(l))?;
        log.line(&format!("built {} ({})", built.image, built.digest));
        let mut notes = Vec::new();
        let spec = preview_spec(&app.spec, &s, n, p.fork, &mut notes);
        let rendered = render_preview(&spec, &built.image, &p.stack, n, &mut notes)?;
        for note in &notes {
            log.line(&format!("note: {note}"));
        }
        dep.image = Some(built.image.clone());
        dep.digest = Some(built.digest.clone());
        dep.rendered = Some(rendered.clone());
        self.pstatus(org, n, dep, Status::Deploying)?;
        let q = crate::stack::qualified(org, &p.stack);
        {
            let _s = self.inner.stacks.lock().unwrap();
            if self.preview_get(org, name, n)?.removing {
                return Err(Error::invalid("the preview is being removed"));
            }
            let cur = self.inner.ctl.definition(&q).ok();
            let file = super::splice(
                cur.as_ref().map(|d| &d.file),
                &p.stack,
                name,
                Some(&rendered),
            );
            let def = self.stack_def(org, &p.stack, file, &dep.by)?;
            let changes = self.inner.ctl.deploy(def)?;
            match changes.iter().find(|c| c.service == name) {
                Some(c) if c.change == "unchanged" => {
                    log.line("settings unchanged: replacing the instances anyway");
                    self.inner.ctl.redeploy(&q, name)?;
                }
                Some(c) => log.line(&format!(
                    "stack {}: {} {} (rev {}, {} replicas)",
                    p.stack, c.service, c.change, c.rev, c.replicas
                )),
                None => {}
            }
        }
        let (ok, msg) = self.wait_service(&q, name)?;
        if !ok {
            return Err(Error::invalid(format!("service {name}: {msg}")));
        }
        log.line(&format!("service {name}.{} converged", p.stack));
        let url = self.preview_url(&q, name);
        self.pstatus(org, n, dep, Status::Done)?;
        let p = {
            let _g = self.inner.edit.lock().unwrap();
            let mut p = self.preview_get(org, name, n)?;
            p.current = Some(dep.id);
            p.sha = Some(co.sha.clone());
            p.image = Some(built.image.clone());
            p.url = url.clone();
            self.preview_save(org, &p)?;
            p
        };
        // This preview's older images: only the running one is kept.
        let mine = format!("pr-{n}-");
        let keep = built.digest.clone();
        match crate::registry::Registry::shared(&self.inner.client).and_then(|reg| {
            reg.delete_tags(
                org,
                name,
                &|t: &str| t.starts_with(&mine),
                &[keep.clone()].into(),
            )
        }) {
            Ok(gone) => {
                for g in gone {
                    log.line(&format!("deleted the older image {g}"));
                }
            }
            Err(e) => log.line(&format!("note: older images kept: {e}")),
        }
        self.forge_status(
            org,
            &app,
            &p,
            &co.sha,
            "success",
            url.as_deref(),
            match &url {
                Some(_) => "preview ready",
                None => "preview deployed (no URL)",
            },
            &mut |l| log.line(l),
        );
        Ok(())
    }

    /// The URL the ingress serves a service on, once it shows (a few
    /// seconds at most after the service converges).
    fn preview_url(&self, q: &str, svc: &str) -> Option<String> {
        let started = Instant::now();
        loop {
            let url = self.inner.ctl.status(q).ok().and_then(|st| {
                st.services
                    .into_iter()
                    .find(|s| s.service == svc)?
                    .domains
                    .into_iter()
                    .find_map(|d| d.url)
            });
            if url.is_some() || started.elapsed() > Duration::from_secs(15) {
                return url;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    /// Post a commit status when the app's previews have `status` set.
    /// Never fails the deploy: problems go to the log.
    #[allow(clippy::too_many_arguments)]
    fn forge_status(
        &self,
        org: &OrgId,
        app: &App,
        p: &Preview,
        sha: &str,
        state: &str,
        url: Option<&str>,
        description: &str,
        log: &mut dyn FnMut(&str),
    ) {
        let Some(st) = app.spec.previews.as_ref().and_then(|s| s.status.as_ref()) else {
            return;
        };
        let Source::Git(g) = &app.spec.source else {
            return;
        };
        let r = (|| -> Result<()> {
            let kind = match (st.kind, provider_of(&p.provider)) {
                (Some(k), _) => k,
                (None, Provider::GitHub) => ForgeKind::Github,
                (None, Provider::Gitea) => ForgeKind::Gitea,
                (None, _) => {
                    return Err(Error::invalid(format!(
                        "commit statuses for {} are not supported; set previews.status.kind",
                        p.provider
                    )));
                }
            };
            let api = forge::api_base(kind, &g.url, st.api_url.as_deref())?;
            let (_, _, owner_repo) = forge::repo_of(&g.url).ok_or_else(|| {
                Error::invalid(format!("cannot tell the repository of {}", g.url))
            })?;
            let (token, _) = self.inner.secrets.get(org, &st.token_secret)?;
            let token = String::from_utf8(token)
                .map_err(|_| Error::invalid("the forge token is not text"))?;
            forge::post_status(
                kind,
                &api,
                &owner_repo,
                &token,
                sha,
                &forge::Status {
                    state,
                    target_url: url,
                    description,
                },
            )
        })();
        match r {
            Ok(()) => log(&format!("commit status {state} posted")),
            Err(e) => log(&format!("note: commit status not posted: {e}")),
        }
    }

    /// Drop the oldest deployment records beyond [`KEEP_DEPLOYMENTS`].
    fn preview_prune(&self, org: &OrgId, p: &Preview) {
        let ds = self
            .preview_deployments(org, &p.app, p.number)
            .unwrap_or_default();
        for d in ds.iter().skip(KEEP_DEPLOYMENTS) {
            if Some(d.id) == p.current {
                continue;
            }
            let _ = std::fs::remove_file(self.pdep_path(org, &p.app, p.number, d.id));
            let _ = std::fs::remove_file(self.plog_path(org, &p.app, p.number, d.id));
        }
    }

    // --- removal ---------------------------------------------------------

    /// Remove a preview: its service (and the stack with its last), its
    /// volumes, its build cache, its images and its records. With `wait`,
    /// returns when done; else in the background.
    pub fn preview_remove(
        &self,
        org: &OrgId,
        name: &str,
        n: u64,
        why: &str,
        wait: bool,
    ) -> Result<()> {
        {
            let _g = self.inner.edit.lock().unwrap();
            let mut p = self.preview_get(org, name, n)?;
            if !p.removing {
                p.removing = true;
                self.preview_save(org, &p)?;
                self.pevent(org, &p, "info", format!("removing ({why})"));
            }
        }
        // A deploy waiting to start never will.
        {
            let mut qs = self.inner.queues.lock().unwrap();
            if let Some(q) = qs.get_mut(&(org.clone(), queue_key(name, n))) {
                if let Some(old) = q.next.take() {
                    self.supersede(org, name, n, old, 0);
                }
            }
        }
        if wait {
            return self.preview_teardown(org, name, n, why);
        }
        let me = self.clone();
        let (org, name, why) = (org.clone(), name.to_string(), why.to_string());
        std::thread::spawn(move || {
            if let Err(e) = me.preview_teardown(&org, &name, n, &why) {
                eprintln!(
                    "isb serve: app {name} preview #{n}: removal failed: {e} (retried later)"
                );
            }
        });
        Ok(())
    }

    /// Remove every preview of an app (before the app goes).
    pub(super) fn previews_remove_all(&self, org: &OrgId, name: &str) -> Result<()> {
        for p in self.preview_list(org, name)? {
            self.preview_remove(org, name, p.number, "app deleted", true)?;
        }
        Ok(())
    }

    fn preview_teardown(&self, org: &OrgId, name: &str, n: u64, why: &str) -> Result<()> {
        let key = (
            self.inner.state.clone(),
            org.to_string(),
            name.to_string(),
            n,
        );
        if !tearing_down().lock().unwrap().insert(key.clone()) {
            return Ok(());
        }
        struct Done(TeardownKey);
        impl Drop for Done {
            fn drop(&mut self) {
                tearing_down().lock().unwrap().remove(&self.0);
            }
        }
        let _done = Done(key);
        // A deploy running now finishes first (it is bounded).
        let started = Instant::now();
        let qkey = (org.clone(), queue_key(name, n));
        while self.inner.queues.lock().unwrap().contains_key(&qkey) {
            if started.elapsed() > self.inner.timeout + Duration::from_secs(45 * 60) {
                return Err(Error::invalid(format!(
                    "preview #{n} of {name}: its deploy is still running"
                )));
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        let p = self.preview_get(org, name, n)?;
        let deps = self.preview_deployments(org, name, n)?;
        let q = crate::stack::qualified(org, &p.stack);
        {
            let _s = self.inner.stacks.lock().unwrap();
            if let Ok(cur) = self.inner.ctl.definition(&q) {
                if cur.file.services.contains_key(name) {
                    let file = super::splice(Some(&cur.file), &p.stack, name, None);
                    if file.services.is_empty() {
                        self.inner.ctl.remove(&q, true, Duration::from_secs(300))?;
                    } else {
                        let def = self.stack_def(org, &p.stack, file, "preview removal")?;
                        self.inner.ctl.deploy(def)?;
                    }
                }
            }
        }
        let oc = crate::org::client(&self.inner.client, org);
        // Its volumes: every name a deployment of it used. A volume is in
        // use until the instances are gone, so retry for a while.
        let volumes: BTreeSet<String> = deps
            .iter()
            .filter_map(|d| d.rendered.as_ref())
            .flat_map(|r| r.volumes.values().filter_map(|v| v.name.clone()))
            .collect();
        let caches: Vec<String> = [true, false]
            .iter()
            .map(|vm| crate::build::cache_volume(&cache_key(name, n, true), *vm))
            .collect();
        if !volumes.is_empty() || p.fork {
            let pool = crate::sandbox::host_facts(&oc)?.pick_pool(None)?;
            for v in volumes.iter().chain(caches.iter()) {
                let started = Instant::now();
                loop {
                    match crate::volume::remove(&oc, &pool, v) {
                        Ok(()) => {
                            self.pevent(org, &p, "info", format!("volume {v} deleted"));
                            break;
                        }
                        Err(e) if e.is_not_found() => break,
                        Err(e) if started.elapsed() > Duration::from_secs(180) => {
                            return Err(Error::invalid(format!("volume {v}: {e}")));
                        }
                        Err(_) => std::thread::sleep(Duration::from_secs(2)),
                    }
                }
            }
        }
        // Its images: tags `pr-<n>-*`, unless a deployed service runs the
        // same digest.
        if let Ok(reg) = crate::registry::Registry::shared(&self.inner.client) {
            let spare = self.deployed_digests(org, name);
            let gone = reg.delete_tags(
                org,
                name,
                &|t: &str| crate::registry::preview_tag_number(t) == Some(n),
                &spare,
            )?;
            for g in gone {
                self.pevent(org, &p, "info", format!("image {g} deleted"));
            }
        }
        {
            let _g = self.inner.edit.lock().unwrap();
            let _ = std::fs::remove_dir_all(self.preview_source_dir(org, name, n));
            std::fs::remove_dir_all(self.preview_dir(org, name, n))?;
        }
        self.pkind(
            org,
            &p,
            "preview.removed",
            "info",
            format!("removed ({why})"),
        );
        Ok(())
    }

    /// Digests of `<org>/<app>` that deployed stacks run now.
    fn deployed_digests(&self, org: &OrgId, app: &str) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for def in self.inner.ctl.definitions() {
            if &def.org != org {
                continue;
            }
            for (svc, spec) in &def.file.services {
                let Some(r) = spec.image.strip_prefix("registry:") else {
                    continue;
                };
                let Ok(r) = crate::registry::ImageRef::parse(r) else {
                    continue;
                };
                if r.app != app {
                    continue;
                }
                if let Some(d) = r.digest.clone().or_else(|| def.images.get(svc).cloned()) {
                    out.insert(d);
                }
            }
        }
        out
    }

    // --- upkeep ----------------------------------------------------------

    /// Mark preview deployments a stopped daemon left unfinished as failed.
    pub(super) fn recover_previews(&self, org: &OrgId, app: &str) {
        for p in self.preview_list(org, app).unwrap_or_default() {
            for mut d in self
                .preview_deployments(org, app, p.number)
                .unwrap_or_default()
            {
                if d.status.finished() {
                    continue;
                }
                d.error = Some("interrupted: the daemon stopped during it".into());
                d.status = Status::Failed;
                d.finished_at = Some(crate::stack::controller::now_ms());
                let _ = self.pdep_save(org, p.number, &d);
            }
        }
    }

    /// One pass of upkeep: finish removals that failed or were interrupted,
    /// and remove previews idle past their app's `ttl`.
    pub fn previews_upkeep(&self) {
        let mut orgs = vec![OrgId::default_org()];
        if let Ok(rd) = std::fs::read_dir(self.inner.state.join("orgs")) {
            for e in rd.flatten() {
                if let Some(o) = e.file_name().to_str().and_then(|s| OrgId::new(s).ok()) {
                    orgs.push(o);
                }
            }
        }
        let now = crate::stack::now_secs();
        for org in orgs {
            for app in self.list(&org).unwrap_or_default() {
                let ttl = app.spec.previews.as_ref().and_then(|s| s.ttl());
                for p in self.preview_list(&org, &app.spec.name).unwrap_or_default() {
                    let expired =
                        ttl.is_some_and(|t| now.saturating_sub(p.updated_at) > t.as_secs());
                    let why = if p.removing {
                        "retrying removal"
                    } else if expired {
                        "expired: not updated within previews.ttl"
                    } else {
                        continue;
                    };
                    if let Err(e) = self.preview_remove(&org, &app.spec.name, p.number, why, false)
                    {
                        eprintln!(
                            "isb serve: app {} preview #{}: {e}",
                            app.spec.name, p.number
                        );
                    }
                }
            }
        }
    }

    /// Run [`Self::previews_upkeep`] every five minutes (the daemon).
    pub fn start_preview_upkeep(&self) {
        let me = self.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(300));
                me.previews_upkeep();
            }
        });
    }
}

/// A preview deployment's log: a file, and each line on the events feed
/// under the preview's stack.
struct PreviewLog {
    file: std::fs::File,
    apps: super::Apps,
    org: OrgId,
    preview: Preview,
    id: u64,
}

impl PreviewLog {
    fn open(apps: &super::Apps, org: &OrgId, p: &Preview, id: u64) -> Result<PreviewLog> {
        use std::os::unix::fs::OpenOptionsExt;
        let path = apps.plog_path(org, &p.app, p.number, id);
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)?;
        Ok(PreviewLog {
            file,
            apps: apps.clone(),
            org: org.clone(),
            preview: p.clone(),
            id,
        })
    }

    fn line(&mut self, l: &str) {
        let _ = writeln!(self.file, "{l}");
        self.apps.pevent(
            &self.org,
            &self.preview,
            "log",
            format!("#{}: {l}", self.id),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(v: Value) -> AppSpec {
        serde_json::from_value(v).unwrap()
    }

    fn git_app() -> AppSpec {
        app(json!({
            "name": "web", "project": "shop",
            "source": {"git": {"url": "https://git.example.com/acme/web.git", "ref": "main"}},
            "build": {"builder": {"type": "dockerfile"}},
            "port": 8080,
            "env": {"DATABASE_URL": "postgres://db.shop-production/app", "TOKEN": {"secret": "api_token"}},
            "volumes": ["data:/data"],
            "ports": ["127.0.0.1:18080:8080"],
            "replicas": 3,
        }))
    }

    fn settings(v: Value) -> PreviewSettings {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn naming() {
        assert_eq!(
            preview_stack("shop", "production", "web", 12).unwrap(),
            "shop-production-pr-12"
        );
        // Too long for `<project>-<env>-pr-<n>`: the shorter form.
        assert_eq!(
            preview_stack("a-long-project-name", "production", "web", 12345).unwrap(),
            "a-long-project-name-pr-12345"
        );
        assert_eq!(preview_tag(7, "abc"), "pr-7-abc");
        assert!(crate::registry::valid_tag(&preview_tag(7, &"f".repeat(40))));
        assert_eq!(cache_key("web", 7, true), "web-pr-7");
        assert_eq!(cache_key("web", 7, false), "web-preview");
        assert_eq!(preview_host("auto", "web", 3).unwrap(), "auto");
        assert_eq!(
            preview_host("*.preview.example.com", "web", 3).unwrap(),
            "web-pr-3.preview.example.com"
        );
        for bad in ["preview.example.com", "*.com", "*.-x.com", "*.a b.com", ""] {
            assert!(preview_host(bad, "web", 3).is_err(), "{bad}");
        }
        assert!(is_pr_suffix("pr-3"));
        assert!(is_pr_suffix("production-pr-12"));
        assert!(!is_pr_suffix("pr-"));
        assert!(!is_pr_suffix("production"));
        assert!(!is_pr_suffix("sprint-pr-x"));
        assert!(!is_pr_suffix("expr-3"));
        assert!(super::super::validate_part("environment", "staging-pr-4").is_err());
        assert_eq!(
            PullRequest::head_ref_in_base(Provider::GitHub, 4),
            "refs/pull/4/head"
        );
        assert_eq!(
            PullRequest::head_ref_in_base(Provider::Gitea, 4),
            "refs/pull/4/head"
        );
        assert_eq!(
            PullRequest::head_ref_in_base(Provider::GitLab, 4),
            "refs/merge-requests/4/head"
        );
        crate::app::git::validate_ref("refs/merge-requests/4/head").unwrap();
    }

    #[test]
    fn settings_validate() {
        let mut a = git_app();
        a.previews = Some(settings(json!({"enabled": true})));
        a.validate().unwrap();
        let p = a.previews.clone().unwrap();
        assert_eq!(
            (p.max, p.replicas, p.forks, p.inherit_env),
            (3, 1, false, false)
        );
        assert_eq!(p.bases(&a), ["main"]);
        let bad = |v: Value| {
            let mut b = git_app();
            b.previews = Some(settings(v));
            b.validate().is_err()
        };
        assert!(bad(json!({"enabled": true, "max": 0})));
        assert!(bad(json!({"enabled": true, "replicas": 0})));
        assert!(bad(json!({"enabled": true, "ttl": "1m"})));
        assert!(bad(json!({"enabled": true, "ttl": "soon"})));
        assert!(bad(
            json!({"enabled": true, "domain": "preview.example.com"})
        ));
        assert!(bad(json!({"enabled": true, "branches": ["--x"]})));
        assert!(bad(
            json!({"enabled": true, "status": {"token_secret": "t", "api_url": "ftp://x"}})
        ));
        assert!(
            serde_json::from_value::<PreviewSettings>(json!({"enabled": true, "bogus": 1}))
                .is_err()
        );
        let mut noport = git_app();
        noport.port = None;
        noport.previews = Some(settings(json!({"enabled": true})));
        assert!(noport.validate().is_err());
        noport.previews = Some(settings(json!({"enabled": true, "port": 80})));
        noport.validate().unwrap();
        let mut img =
            app(json!({"name": "x", "project": "p", "source": {"image": "docker:nginx"}}));
        img.previews = Some(settings(json!({"enabled": true})));
        assert!(img.validate().is_err(), "an image app has no pull requests");
        let s = settings(json!({"env": {"K": {"secret": "s1"}}, "status": {"token_secret": "gt"}}));
        assert_eq!(s.secret_names(), ["s1", "gt"]);
        assert_eq!(
            settings(json!({"branches": ["main", "release"]})).bases(&git_app()),
            ["main", "release"]
        );
    }

    #[test]
    fn spec_is_isolated_from_production() {
        let a = git_app();
        let s = settings(json!({
            "enabled": true,
            "env": {"DATABASE_URL": "postgres://db.shop-production-pr-1/app", "PREVIEW_KEY": {"secret": "preview_key"}},
        }));
        let mut notes = vec![];
        let p = preview_spec(&a, &s, 5, false, &mut notes);
        // Only the preview's env: production's database URL and secret do
        // not come along.
        assert_eq!(
            p.env.get("DATABASE_URL"),
            Some(&EnvValue::Plain(
                "postgres://db.shop-production-pr-1/app".into()
            ))
        );
        assert!(p.env.get("TOKEN").is_none());
        assert!(p.env.get("PREVIEW_KEY").is_some());
        assert!(p.ports.is_empty(), "no published host ports");
        assert_eq!(p.replicas, 1);
        assert_eq!(p.domains.len(), 1);
        assert_eq!(p.domains[0]["host"], "auto");
        assert_eq!(p.domains[0]["port"], 8080);
        assert!(p.previews.is_none());
        let r = render_preview(
            &p,
            "registry:web:pr-5-abc@sha256:00",
            "shop-production-pr-5",
            5,
            &mut notes,
        )
        .unwrap();
        assert_eq!(r.service.labels[LABEL_PREVIEW], "5");
        assert_eq!(r.service.labels[super::super::LABEL_APP], "web");
        assert_eq!(
            r.volumes["web_data"].name.as_deref(),
            Some("shop-production-pr-5_web_data"),
            "never production's volume"
        );
        assert!(r.secrets.contains_key("web.preview_key"));
        assert!(!r.secrets.contains_key("web.api_token"));

        // inherit_env: the app's env under the preview's.
        let s2 =
            settings(json!({"enabled": true, "inherit_env": true, "env": {"DATABASE_URL": "x"}}));
        let p2 = preview_spec(&a, &s2, 5, false, &mut notes);
        assert_eq!(
            p2.env.get("DATABASE_URL"),
            Some(&EnvValue::Plain("x".into()))
        );
        assert!(p2.env.get("TOKEN").is_some());
    }

    #[test]
    fn app_files_follow_the_env_rules() {
        let mut a = git_app();
        a.files = vec![
            crate::app::AppFile {
                path: "/etc/app/prod.conf".into(),
                secret: "prod_conf".into(),
                mode: None,
            },
            crate::app::AppFile {
                path: "/etc/app/public.pem".into(),
                secret: "public_key".into(),
                mode: None,
            },
        ];
        let mut notes = vec![];
        let paths = |p: &AppSpec| p.files.iter().map(|f| f.path.clone()).collect::<Vec<_>>();
        // Without inherit_env a preview takes none of the app's files.
        let s = settings(json!({"enabled": true}));
        assert!(preview_spec(&a, &s, 1, false, &mut notes).files.is_empty());
        // With it, a same-repo preview takes them all, a fork only fork_secrets'.
        let s = settings(json!({
            "enabled": true, "forks": true, "inherit_env": true, "fork_secrets": ["public_key"],
        }));
        assert_eq!(paths(&preview_spec(&a, &s, 1, false, &mut notes)).len(), 2);
        assert_eq!(
            paths(&preview_spec(&a, &s, 1, true, &mut notes)),
            vec!["/etc/app/public.pem".to_string()]
        );
        assert!(notes.iter().any(|n| n.contains("prod_conf")), "{notes:?}");
    }

    #[test]
    fn forks_get_no_app_secrets() {
        let a = git_app();
        let s = settings(json!({
            "enabled": true, "forks": true, "inherit_env": true,
            "env": {"SAFE": {"secret": "public_key"}, "UNSAFE": {"secret": "preview_db"}, "PLAIN": "1"},
            "fork_secrets": ["public_key"],
            "domain": "*.preview.example.com",
        }));
        let mut notes = vec![];
        let p = preview_spec(&a, &s, 9, true, &mut notes);
        assert!(p.env.get("TOKEN").is_none(), "the app's secret is withheld");
        assert!(p.env.get("UNSAFE").is_none(), "not marked safe for forks");
        assert!(p.env.get("SAFE").is_some());
        assert_eq!(p.env.get("PLAIN"), Some(&EnvValue::Plain("1".into())));
        assert_eq!(
            p.env.get("DATABASE_URL"),
            Some(&EnvValue::Plain("postgres://db.shop-production/app".into())),
            "plain values are inherited as asked"
        );
        assert_eq!(
            notes.iter().filter(|n| n.contains("withheld")).count(),
            2,
            "{notes:?}"
        );
        assert_eq!(p.domains[0]["host"], "web-pr-9.preview.example.com");
        // The same settings for a branch of the repository itself: the
        // secrets it was given.
        let mut notes = vec![];
        let p = preview_spec(&a, &s, 9, false, &mut notes);
        assert!(p.env.get("TOKEN").is_some() && p.env.get("UNSAFE").is_some());
    }
}
