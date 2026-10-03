//! The app service: records on disk, the deploy queue and pipeline, and
//! webhooks.
//!
//! A deploy is a record ([`Deployment`]) that moves `queued` → `building` →
//! `deploying` → `done` | `failed`. One runs at a time per app; a deploy
//! asked for while one runs waits behind it, and a newer request
//! supersedes a waiting one that has not started (`superseded`), so a burst
//! of pushes builds the latest commit once.
//!
//! The pipeline: resolve the image (an image source's digest, or fetch the
//! git source and build it with [`crate::build::run`]), render the app into
//! its project environment's stack in place of its old service, hand the
//! stack to the controller, and wait for that service to converge.

use std::collections::{BTreeMap, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::git::{self, Credentials, GitAuth};
use super::{App, AppSpec, Project, Rendered, Source, webhook};
use crate::build::{BuildRequest, BuiltImage};
use crate::client::Client;
use crate::error::{Error, Result};
use crate::org::OrgId;
use crate::secrets::Secrets;
use crate::stack::{Controller, StackDef};

/// Turns a checkout into an image: [`crate::build::run`], or a stand-in.
pub type BuildFn =
    Arc<dyn Fn(&Client, &BuildRequest, &mut dyn FnMut(&str)) -> Result<BuiltImage> + Send + Sync>;

/// The manifest digest an image reference names now, if it can be found.
pub type DigestFn = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Deployment records kept per app (older ones and their logs are pruned).
const KEEP_DEPLOYMENTS: usize = 30;

/// How long a deploy waits for its service to converge.
const DEPLOY_TIMEOUT: Duration = Duration::from_secs(900);

/// Who asked for a deployment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Trigger {
    /// The local CLI.
    Manual,
    /// A tool call over MCP or REST.
    Api,
    Webhook,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Queued,
    Building,
    Deploying,
    Done,
    Failed,
    /// A newer deploy replaced it before it started.
    Superseded,
}

impl Status {
    pub fn finished(self) -> bool {
        matches!(self, Status::Done | Status::Failed | Status::Superseded)
    }

    /// The moves the pipeline makes; anything else is a bug.
    pub fn can_become(self, next: Status) -> bool {
        use Status::*;
        matches!(
            (self, next),
            (Queued, Building)
                | (Queued, Superseded)
                | (Queued, Failed)
                | (Building, Deploying)
                | (Building, Failed)
                | (Deploying, Done)
                | (Deploying, Failed)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commit {
    pub sha: String,
    pub message: String,
}

/// One deploy of an app.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Deployment {
    pub id: u64,
    pub app: String,
    pub trigger: Trigger,
    /// The caller (a user, an API token, `local`, `webhook:github`).
    pub by: String,
    pub status: Status,
    /// What a webhook said was pushed (`refs/heads/main abc123`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested: Option<String>,
    /// A rollback: the deployment whose image and settings it restores.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback_of: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<Commit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Unix milliseconds.
    pub created_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<u64>,
    /// The service as deployed: what a rollback puts back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rendered: Option<Rendered>,
}

impl Deployment {
    /// Move to `next`, refusing a move the state machine does not have.
    pub fn advance(&mut self, next: Status) -> Result<()> {
        if !self.status.can_become(next) {
            return Err(Error::invalid(format!(
                "deployment {}: cannot go from {:?} to {next:?}",
                self.id, self.status
            )));
        }
        self.status = next;
        let now = crate::stack::controller::now_ms();
        match next {
            Status::Building => self.started_at = Some(now),
            s if s.finished() => self.finished_at = Some(now),
            _ => {}
        }
        Ok(())
    }

    /// The record without the rendered service, for listings.
    pub fn summary(&self) -> Value {
        let mut v = serde_json::to_value(self).unwrap_or_default();
        if let Some(o) = v.as_object_mut() {
            o.remove("rendered");
        }
        v
    }
}

#[derive(Default)]
pub(super) struct Queue {
    pub(super) running: bool,
    pub(super) next: Option<u64>,
}

pub(super) struct Inner {
    pub(super) state: PathBuf,
    pub(super) client: Client,
    pub(super) ctl: Controller,
    pub(super) secrets: Arc<Secrets>,
    pub(super) build: BuildFn,
    digest: DigestFn,
    pub(super) timeout: Duration,
    /// Held across read-modify-write of app and project records.
    pub(super) edit: Mutex<()>,
    /// Held across read-splice-deploy of a stack, so two apps deploying
    /// into one environment never drop each other's service.
    pub(super) stacks: Mutex<()>,
    /// Keyed by app, or `<app>#pr-<n>` for a preview.
    pub(super) queues: Mutex<BTreeMap<(OrgId, String), Queue>>,
    /// Recent webhook delivery ids, to ignore a replayed delivery.
    deliveries: Mutex<VecDeque<String>>,
}

/// Projects, environments, apps and their deployments, for every org.
#[derive(Clone)]
pub struct Apps {
    pub(super) inner: Arc<Inner>,
}

impl Apps {
    /// Open the app store under `state`. Deployments a stopped daemon left
    /// unfinished are marked failed.
    pub fn new(state: &Path, client: Client, ctl: Controller, secrets: Arc<Secrets>) -> Apps {
        let a = Apps {
            inner: Arc::new(Inner {
                state: state.to_path_buf(),
                client,
                ctl,
                secrets,
                build: Arc::new(|c: &Client, r: &BuildRequest, l: &mut dyn FnMut(&str)| {
                    crate::build::run(c, r, l)
                }),
                digest: Arc::new(skopeo_digest),
                timeout: DEPLOY_TIMEOUT,
                edit: Mutex::new(()),
                stacks: Mutex::new(()),
                queues: Mutex::new(BTreeMap::new()),
                deliveries: Mutex::new(VecDeque::new()),
            }),
        };
        a.recover();
        a
    }

    fn with(self, f: impl FnOnce(&mut Inner)) -> Apps {
        let mut inner = Arc::try_unwrap(self.inner)
            .unwrap_or_else(|_| panic!("Apps::with_* before the Apps is shared"));
        f(&mut inner);
        Apps {
            inner: Arc::new(inner),
        }
    }

    /// Use `f` instead of [`crate::build::run`] (tests).
    pub fn with_build(self, f: BuildFn) -> Apps {
        self.with(|i| i.build = f)
    }

    /// Use `f` to find an image's digest instead of `skopeo inspect`.
    pub fn with_digest(self, f: DigestFn) -> Apps {
        self.with(|i| i.digest = f)
    }

    pub fn with_timeout(self, t: Duration) -> Apps {
        self.with(|i| i.timeout = t)
    }

    // --- paths -----------------------------------------------------------

    pub(super) fn apps_dir(&self, org: &OrgId) -> PathBuf {
        super::org_root(&self.inner.state, org).join("apps")
    }

    fn projects_dir(&self, org: &OrgId) -> PathBuf {
        self.apps_dir(org).join("projects")
    }

    pub(super) fn app_dir(&self, org: &OrgId, app: &str) -> PathBuf {
        self.apps_dir(org).join(app)
    }

    fn deployments_dir(&self, org: &OrgId, app: &str) -> PathBuf {
        self.app_dir(org, app).join("deployments")
    }

    /// Where an app's git checkout lives.
    pub fn source_dir(&self, org: &OrgId, app: &str) -> PathBuf {
        super::org_root(&self.inner.state, org)
            .join("sources")
            .join(app)
    }

    // --- projects --------------------------------------------------------

    pub fn project_create(
        &self,
        org: &OrgId,
        name: &str,
        description: &str,
        environments: &[String],
    ) -> Result<Project> {
        super::validate_part("project", name)?;
        let envs: Vec<String> = if environments.is_empty() {
            vec![super::DEFAULT_ENVIRONMENT.into()]
        } else {
            environments.to_vec()
        };
        for e in &envs {
            super::validate_part("environment", e)?;
            super::stack_name(name, e)?;
        }
        let _g = self.inner.edit.lock().unwrap();
        let p = self.projects_dir(org).join(format!("{name}.json"));
        if p.exists() {
            return Err(Error::AlreadyExists(format!("project {name}")));
        }
        let mut envs = envs;
        envs.dedup();
        let proj = Project {
            name: name.into(),
            description: description.into(),
            environments: envs,
            created_at: crate::stack::now_secs(),
        };
        super::write_atomic(&p, &serde_json::to_vec_pretty(&proj)?)?;
        Ok(proj)
    }

    pub fn project_get(&self, org: &OrgId, name: &str) -> Result<Project> {
        super::validate_part("project", name)?;
        let p = self.projects_dir(org).join(format!("{name}.json"));
        match std::fs::read(&p) {
            Ok(b) => Ok(serde_json::from_slice(&b)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(Error::NotFound(format!("project {name} in org {org}")))
            }
            Err(e) => Err(e.into()),
        }
    }

    pub fn project_list(&self, org: &OrgId) -> Result<Vec<Project>> {
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir(self.projects_dir(org)) else {
            return Ok(out);
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "json") {
                match serde_json::from_slice::<Project>(&std::fs::read(&p)?) {
                    Ok(pr) => out.push(pr),
                    Err(e) => eprintln!("isb serve: skipping {}: {e}", p.display()),
                }
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Delete an empty project (its apps go first).
    pub fn project_delete(&self, org: &OrgId, name: &str) -> Result<()> {
        let _g = self.inner.edit.lock().unwrap();
        self.project_get(org, name)?;
        let apps: Vec<String> = self
            .list(org)?
            .into_iter()
            .filter(|a| a.spec.project == name)
            .map(|a| a.spec.name)
            .collect();
        if !apps.is_empty() {
            return Err(Error::invalid(format!(
                "project {name} still has apps: {}; delete them first",
                apps.join(", ")
            )));
        }
        std::fs::remove_file(self.projects_dir(org).join(format!("{name}.json")))?;
        Ok(())
    }

    pub fn environment_create(&self, org: &OrgId, project: &str, env: &str) -> Result<Project> {
        super::validate_part("environment", env)?;
        super::stack_name(project, env)?;
        let _g = self.inner.edit.lock().unwrap();
        let mut p = self.project_get(org, project)?;
        if p.environments.iter().any(|e| e == env) {
            return Err(Error::AlreadyExists(format!(
                "environment {env} in project {project}"
            )));
        }
        p.environments.push(env.into());
        self.save_project(org, &p)?;
        Ok(p)
    }

    pub fn environment_delete(&self, org: &OrgId, project: &str, env: &str) -> Result<Project> {
        let _g = self.inner.edit.lock().unwrap();
        let mut p = self.project_get(org, project)?;
        if !p.environments.iter().any(|e| e == env) {
            return Err(Error::NotFound(format!(
                "environment {env} in project {project}"
            )));
        }
        let apps: Vec<String> = self
            .list(org)?
            .into_iter()
            .filter(|a| a.spec.project == project && a.spec.environment == env)
            .map(|a| a.spec.name)
            .collect();
        if !apps.is_empty() {
            return Err(Error::invalid(format!(
                "environment {env} still has apps: {}; delete them first",
                apps.join(", ")
            )));
        }
        p.environments.retain(|e| e != env);
        self.save_project(org, &p)?;
        Ok(p)
    }

    fn save_project(&self, org: &OrgId, p: &Project) -> Result<()> {
        super::write_atomic(
            &self.projects_dir(org).join(format!("{}.json", p.name)),
            &serde_json::to_vec_pretty(p)?,
        )
    }

    // --- apps ------------------------------------------------------------

    pub fn get(&self, org: &OrgId, name: &str) -> Result<App> {
        super::validate_app_name(name)?;
        match std::fs::read(self.app_dir(org, name).join("app.json")) {
            Ok(b) => Ok(serde_json::from_slice(&b)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(Error::NotFound(format!("app {name} in org {org}")))
            }
            Err(e) => Err(e.into()),
        }
    }

    pub fn list(&self, org: &OrgId) -> Result<Vec<App>> {
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir(self.apps_dir(org)) else {
            return Ok(out);
        };
        for e in rd.flatten() {
            let p = e.path().join("app.json");
            if !p.is_file() {
                continue;
            }
            match serde_json::from_slice::<App>(&std::fs::read(&p)?) {
                Ok(a) => out.push(a),
                Err(e) => eprintln!("isb serve: skipping {}: {e}", p.display()),
            }
        }
        out.sort_by(|a, b| a.spec.name.cmp(&b.spec.name));
        Ok(out)
    }

    fn save(&self, org: &OrgId, app: &App) -> Result<()> {
        super::write_atomic(
            &self.app_dir(org, &app.spec.name).join("app.json"),
            &serde_json::to_vec_pretty(app)?,
        )
    }

    /// Create an app (not deployed yet) and its webhook secret. Returns the
    /// app and that secret.
    pub fn create(&self, org: &OrgId, spec: AppSpec) -> Result<(App, String)> {
        spec.validate()?;
        let _g = self.inner.edit.lock().unwrap();
        let proj = self.project_get(org, &spec.project)?;
        if !proj.environments.contains(&spec.environment) {
            return Err(Error::NotFound(format!(
                "environment {} in project {} (it has {})",
                spec.environment,
                spec.project,
                proj.environments.join(", ")
            )));
        }
        if self.app_dir(org, &spec.name).join("app.json").exists() {
            return Err(Error::AlreadyExists(format!("app {}", spec.name)));
        }
        self.check_secrets(org, &spec)?;
        let now = crate::stack::now_secs();
        let app = App {
            spec,
            created_at: now,
            updated_at: now,
            next_deployment: 1,
            current: None,
        };
        let secret = git::random_hex(32);
        self.inner
            .secrets
            .set(org, &app.spec.webhook_secret(), secret.as_bytes())?;
        self.save(org, &app)?;
        Ok((app, secret))
    }

    /// Secrets an app names must exist (a typo is better caught now than
    /// at deploy).
    fn check_secrets(&self, org: &OrgId, spec: &AppSpec) -> Result<()> {
        let mut names = spec.env.secret_names();
        if let Some(p) = &spec.previews {
            names.extend(p.secret_names());
        }
        names.extend(spec.files.iter().map(|f| f.secret.clone()));
        if let Source::Git(g) = &spec.source {
            names.extend(g.auth.secret().map(String::from));
        }
        for n in names {
            self.inner.secrets.inspect(org, &n).map_err(|e| match e {
                Error::NotFound(_) => Error::invalid(format!(
                    "secret {n} does not exist in org {org}; create it with `isb secret create {n} --org {org}`"
                )),
                e => e,
            })?;
        }
        Ok(())
    }

    /// Change an app's settings with a JSON merge patch (`null` clears a
    /// setting). Name, project and environment are fixed. Takes effect at
    /// the next deploy.
    pub fn update(&self, org: &OrgId, name: &str, patch: &Value) -> Result<App> {
        let _g = self.inner.edit.lock().unwrap();
        let mut app = self.get(org, name)?;
        let mut v = serde_json::to_value(&app.spec)?;
        super::merge_patch(&mut v, patch);
        let spec: AppSpec =
            serde_json::from_value(v).map_err(|e| Error::invalid(format!("app {name}: {e}")))?;
        if spec.name != app.spec.name
            || spec.project != app.spec.project
            || spec.environment != app.spec.environment
        {
            return Err(Error::invalid(
                "an app's name, project and environment are fixed; create a new app instead",
            ));
        }
        spec.validate()?;
        self.check_secrets(org, &spec)?;
        app.spec = spec;
        app.updated_at = crate::stack::now_secs();
        self.save(org, &app)?;
        Ok(app)
    }

    pub fn env_get(&self, org: &OrgId, name: &str) -> Result<String> {
        Ok(self.get(org, name)?.spec.env.render())
    }

    /// Replace an app's environment with `.env` text.
    pub fn env_set(&self, org: &OrgId, name: &str, text: &str) -> Result<App> {
        let env = super::EnvFile::parse(text)?;
        self.update(org, name, &json!({"env": env.render()}))
    }

    /// The app's webhook secret; with `rotate`, a new one first.
    pub fn webhook_secret(&self, org: &OrgId, name: &str, rotate: bool) -> Result<String> {
        let app = self.get(org, name)?;
        let key = app.spec.webhook_secret();
        if !rotate {
            if let Ok((v, _)) = self.inner.secrets.get(org, &key) {
                return String::from_utf8(v)
                    .map_err(|_| Error::invalid("webhook secret is not text"));
            }
        }
        let s = git::random_hex(32);
        self.inner.secrets.set(org, &key, s.as_bytes())?;
        Ok(s)
    }

    /// A new ed25519 deploy key for a git app over SSH: stored as the org
    /// secret `app.<app>.deploy-key`, made the app's credential, and its
    /// public half returned to paste into the repository's deploy keys.
    pub fn deploy_key(&self, org: &OrgId, name: &str) -> Result<String> {
        let app = self.get(org, name)?;
        let Source::Git(g) = &app.spec.source else {
            return Err(Error::invalid(format!("app {name} has no git source")));
        };
        if git::transport(&g.url)? != git::Transport::Ssh {
            return Err(Error::invalid(format!(
                "a deploy key needs an SSH URL; {} is not one (git@host:owner/repo)",
                g.url
            )));
        }
        let (private, public) =
            git::generate_deploy_key(&self.apps_dir(org), &format!("isb-deploy-{org}-{name}"))?;
        let key = super::deploy_key_secret(name);
        self.inner.secrets.set(org, &key, &private)?;
        self.update(
            org,
            name,
            &json!({"source": {"git": {"auth": {"ssh_key_secret": key}}}}),
        )?;
        Ok(public)
    }

    /// Delete an app: its service leaves the stack (the stack goes when it
    /// was the last), then its records, checkout and the secrets isb made
    /// for it. Named volumes are kept.
    pub fn delete(&self, org: &OrgId, name: &str) -> Result<()> {
        let app = self.get(org, name)?;
        if self
            .inner
            .queues
            .lock()
            .unwrap()
            .contains_key(&(org.clone(), name.to_string()))
        {
            return Err(Error::invalid(format!(
                "app {name} is deploying; delete it once that finishes"
            )));
        }
        // Its previews go first, with their stacks, volumes and images.
        self.previews_remove_all(org, name)?;
        let stack = app.spec.stack()?;
        let q = crate::stack::qualified(org, &stack);
        {
            let _s = self.inner.stacks.lock().unwrap();
            if let Ok(cur) = self.inner.ctl.definition(&q) {
                if cur.file.services.contains_key(name) {
                    let file = super::splice(Some(&cur.file), &stack, name, None);
                    if file.services.is_empty() {
                        self.inner.ctl.remove(&q, false, Duration::from_secs(300))?;
                    } else {
                        let def = self.stack_def(org, &stack, file, "app delete")?;
                        self.inner.ctl.deploy(def)?;
                    }
                }
            }
        }
        let _g = self.inner.edit.lock().unwrap();
        for s in [super::webhook_secret(name), super::deploy_key_secret(name)] {
            match self.inner.secrets.delete(org, &s) {
                Ok(()) => {}
                Err(e) if e.is_not_found() => {}
                Err(e) => return Err(e),
            }
        }
        let _ = std::fs::remove_dir_all(self.source_dir(org, name));
        std::fs::remove_dir_all(self.app_dir(org, name))?;
        Ok(())
    }

    // --- deployments -----------------------------------------------------

    fn dep_path(&self, org: &OrgId, app: &str, id: u64) -> PathBuf {
        self.deployments_dir(org, app).join(format!("{id}.json"))
    }

    fn log_path(&self, org: &OrgId, app: &str, id: u64) -> PathBuf {
        self.deployments_dir(org, app).join(format!("{id}.log"))
    }

    fn save_dep(&self, org: &OrgId, d: &Deployment) -> Result<()> {
        super::write_atomic(
            &self.dep_path(org, &d.app, d.id),
            &serde_json::to_vec_pretty(d)?,
        )
    }

    pub fn deployment(&self, org: &OrgId, app: &str, id: u64) -> Result<Deployment> {
        super::validate_app_name(app)?;
        match std::fs::read(self.dep_path(org, app, id)) {
            Ok(b) => Ok(serde_json::from_slice(&b)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(Error::NotFound(format!("deployment {id} of app {app}")))
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Newest first.
    pub fn deployments(&self, org: &OrgId, app: &str) -> Result<Vec<Deployment>> {
        self.get(org, app)?;
        let mut ids: Vec<u64> = match std::fs::read_dir(self.deployments_dir(org, app)) {
            Ok(rd) => rd
                .flatten()
                .filter_map(|e| {
                    let n = e.file_name().into_string().ok()?;
                    n.strip_suffix(".json")?.parse().ok()
                })
                .collect(),
            Err(_) => vec![],
        };
        ids.sort_unstable_by(|a, b| b.cmp(a));
        ids.into_iter()
            .map(|id| self.deployment(org, app, id))
            .collect()
    }

    /// A deployment's log from byte `offset`: `(text, next offset, finished)`.
    pub fn log(&self, org: &OrgId, app: &str, id: u64, offset: u64) -> Result<(String, u64, bool)> {
        let d = self.deployment(org, app, id)?;
        let b = std::fs::read(self.log_path(org, app, id)).unwrap_or_default();
        let start = (offset as usize).min(b.len());
        Ok((
            String::from_utf8_lossy(&b[start..]).into_owned(),
            b.len() as u64,
            d.status.finished(),
        ))
    }

    /// Wait until a deployment finishes, or `timeout`.
    pub fn wait(&self, org: &OrgId, app: &str, id: u64, timeout: Duration) -> Result<Deployment> {
        let started = Instant::now();
        loop {
            let d = self.deployment(org, app, id)?;
            if d.status.finished() || started.elapsed() >= timeout {
                return Ok(d);
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    /// Queue a deploy of the app's current settings.
    pub fn deploy(
        &self,
        org: &OrgId,
        name: &str,
        trigger: Trigger,
        by: &str,
        requested: Option<String>,
    ) -> Result<Deployment> {
        self.enqueue(org, name, trigger, by, requested, None)
    }

    /// Queue a rollback to deployment `to` (default: the one before the
    /// current): its image and settings, without building.
    pub fn rollback(
        &self,
        org: &OrgId,
        name: &str,
        to: Option<u64>,
        trigger: Trigger,
        by: &str,
    ) -> Result<Deployment> {
        let app = self.get(org, name)?;
        let target = match to {
            Some(id) => self.deployment(org, name, id)?,
            None => self
                .deployments(org, name)?
                .into_iter()
                .find(|d| d.status == Status::Done && Some(d.id) != app.current)
                .ok_or_else(|| {
                    Error::invalid(format!(
                        "app {name} has no earlier successful deployment to roll back to"
                    ))
                })?,
        };
        if target.status != Status::Done || target.rendered.is_none() {
            return Err(Error::invalid(format!(
                "deployment {} did not finish done; only those can be rolled back to",
                target.id
            )));
        }
        self.enqueue(org, name, trigger, by, None, Some(target.id))
    }

    fn enqueue(
        &self,
        org: &OrgId,
        name: &str,
        trigger: Trigger,
        by: &str,
        requested: Option<String>,
        rollback_of: Option<u64>,
    ) -> Result<Deployment> {
        let dep = {
            let _g = self.inner.edit.lock().unwrap();
            let mut app = self.get(org, name)?;
            let id = app.next_deployment;
            app.next_deployment += 1;
            self.save(org, &app)?;
            let d = Deployment {
                id,
                app: name.into(),
                trigger,
                by: by.into(),
                status: Status::Queued,
                requested,
                rollback_of,
                commit: None,
                image: None,
                digest: None,
                error: None,
                created_at: crate::stack::controller::now_ms(),
                started_at: None,
                finished_at: None,
                rendered: None,
            };
            self.save_dep(org, &d)?;
            self.prune(org, &app);
            d
        };
        self.event(
            org,
            name,
            "info",
            format!("deployment {} queued by {by}", dep.id),
        );
        let start = {
            let mut qs = self.inner.queues.lock().unwrap();
            let q = qs.entry((org.clone(), name.to_string())).or_default();
            if let Some(old) = q.next.replace(dep.id) {
                if let Ok(mut d) = self.deployment(org, name, old) {
                    if d.advance(Status::Superseded).is_ok() {
                        d.error = Some(format!("superseded by deployment {}", dep.id));
                        let _ = self.save_dep(org, &d);
                    }
                }
            }
            !std::mem::replace(&mut q.running, true)
        };
        if start {
            let me = self.clone();
            let (org, name) = (org.clone(), name.to_string());
            std::thread::spawn(move || me.drain(&org, &name));
        }
        Ok(dep)
    }

    /// Run queued deployments of one app until none is left.
    fn drain(&self, org: &OrgId, name: &str) {
        loop {
            let next = {
                let mut qs = self.inner.queues.lock().unwrap();
                let key = (org.clone(), name.to_string());
                let q = qs.entry(key.clone()).or_default();
                match q.next.take() {
                    Some(id) => id,
                    None => {
                        qs.remove(&key);
                        return;
                    }
                }
            };
            self.run(org, name, next);
        }
    }

    fn run(&self, org: &OrgId, name: &str, id: u64) {
        let Ok(mut dep) = self.deployment(org, name, id) else {
            return;
        };
        let mut log = match DeployLog::open(self, org, name, id) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("isb serve: app {name}: deployment {id}: no log: {e}");
                return;
            }
        };
        let r = self.pipeline(org, name, &mut dep, &mut log);
        let (kind, level, msg) = match &r {
            Ok(()) => ("deploy.succeeded", "info", format!("deployment {id} done")),
            Err(e) => (
                "deploy.failed",
                "error",
                format!("deployment {id} failed: {e}"),
            ),
        };
        log.line(&msg);
        if let Err(e) = r {
            dep.error = Some(e.to_string());
            let _ = dep.advance(Status::Failed);
        }
        if let Err(e) = self.save_dep(org, &dep) {
            eprintln!("isb serve: app {name}: deployment {id}: {e}");
        }
        self.kind_event(org, name, kind, level, msg);
    }

    fn set_status(&self, org: &OrgId, dep: &mut Deployment, s: Status) -> Result<()> {
        dep.advance(s)?;
        self.save_dep(org, dep)?;
        self.event(
            org,
            &dep.app,
            "info",
            format!("deployment {}: {s:?}", dep.id).to_lowercase(),
        );
        Ok(())
    }

    fn pipeline(
        &self,
        org: &OrgId,
        name: &str,
        dep: &mut Deployment,
        log: &mut DeployLog,
    ) -> Result<()> {
        self.set_status(org, dep, Status::Building)?;
        let app = self.get(org, name)?;
        let mut notes = Vec::new();
        let rendered = match dep.rollback_of {
            Some(of) => {
                let t = self.deployment(org, name, of)?;
                log.line(&format!(
                    "rolling back to deployment {of} ({})",
                    t.image.as_deref().unwrap_or("?")
                ));
                dep.image = t.image;
                dep.digest = t.digest;
                dep.commit = t.commit;
                t.rendered
                    .ok_or_else(|| Error::invalid(format!("deployment {of} kept no settings")))?
            }
            None => {
                let (image, digest) = match &app.spec.source {
                    Source::Image(i) => {
                        log.line(&format!("image {i}"));
                        match (self.inner.digest)(i) {
                            Some(d) => {
                                let pinned = super::pin(i, &d).unwrap_or_else(|| i.clone());
                                log.line(&format!("resolved to {pinned}"));
                                (pinned, Some(d))
                            }
                            None => {
                                log.line("no digest found; deploying by tag");
                                (i.clone(), None)
                            }
                        }
                    }
                    Source::Git(g) => {
                        let creds = self.credentials(org, &g.auth)?;
                        let co = git::fetch(g, &creds, &self.source_dir(org, name), &mut |l| {
                            log.line(l)
                        })?;
                        dep.commit = Some(Commit {
                            sha: co.sha.clone(),
                            message: co.message.clone(),
                        });
                        self.save_dep(org, dep)?;
                        let b =
                            app.spec.build.as_ref().ok_or_else(|| {
                                Error::invalid("a git source needs build settings")
                            })?;
                        let req = BuildRequest {
                            org: org.clone(),
                            app: name.into(),
                            context: co.dir.clone(),
                            subdir: g.subdir.clone(),
                            builder: b.builder.clone(),
                            args: b.args.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
                            tag: co.sha.clone(),
                            untrusted: b.untrusted,
                            cache: None,
                        };
                        log.line(&format!("building {} with {:?}", co.sha, b.builder));
                        let built =
                            (self.inner.build)(&self.inner.client, &req, &mut |l| log.line(l))?;
                        log.line(&format!("built {} ({})", built.image, built.digest));
                        (built.image, Some(built.digest))
                    }
                };
                dep.image = Some(image.clone());
                dep.digest = digest;
                super::render(&app.spec, &image, &mut notes)?
            }
        };
        for n in &notes {
            log.line(&format!("note: {n}"));
        }
        dep.rendered = Some(rendered.clone());
        self.set_status(org, dep, Status::Deploying)?;
        let stack = app.spec.stack()?;
        let q = crate::stack::qualified(org, &stack);
        {
            let _s = self.inner.stacks.lock().unwrap();
            let cur = self.inner.ctl.definition(&q).ok();
            let file = super::splice(cur.as_ref().map(|d| &d.file), &stack, name, Some(&rendered));
            let def = self.stack_def(org, &stack, file, &dep.by)?;
            let changes = self.inner.ctl.deploy(def)?;
            let mine = changes.iter().find(|c| c.service == name);
            match mine {
                Some(c) if c.change == "unchanged" => {
                    // Deploy means fresh instances, as in Dokploy: a moved
                    // tag or a restart the settings do not show.
                    log.line("settings unchanged: replacing the instances anyway");
                    self.inner.ctl.redeploy(&q, name)?;
                }
                Some(c) => log.line(&format!(
                    "stack {stack}: {} {} (rev {}, {} replicas)",
                    c.service, c.change, c.rev, c.replicas
                )),
                None => {}
            }
        }
        let (ok, msg) = self.wait_service(&q, name)?;
        if !ok {
            return Err(Error::invalid(format!("service {name}: {msg}")));
        }
        log.line(&format!("service {name}.{stack} converged"));
        self.set_status(org, dep, Status::Done)?;
        let _g = self.inner.edit.lock().unwrap();
        let mut app = self.get(org, name)?;
        app.current = Some(dep.id);
        self.save(org, &app)?;
        Ok(())
    }

    /// The stack definition for `file`, with its secrets bound.
    pub(super) fn stack_def(
        &self,
        org: &OrgId,
        stack: &str,
        file: crate::spec::ComposeFile,
        by: &str,
    ) -> Result<StackDef> {
        let base = self.apps_dir(org);
        std::fs::create_dir_all(&base)?;
        let secrets = crate::stack::secrets::bind(
            &self.inner.secrets,
            org,
            stack,
            &file,
            &BTreeMap::new(),
            false,
        )?;
        Ok(StackDef {
            name: stack.into(),
            org: org.clone(),
            file,
            base_dir: base,
            secrets,
            force: BTreeMap::new(),
            images: BTreeMap::new(),
            deployed_at: crate::stack::now_secs(),
            deployed_by: by.into(),
            previous: None,
        })
    }

    /// Wait for one service of a stack to settle at its current revision:
    /// `(converged, why not)`.
    pub(super) fn wait_service(&self, q: &str, svc: &str) -> Result<(bool, String)> {
        let started = Instant::now();
        loop {
            let def = self.inner.ctl.definition(q)?;
            let rev = def.revision(svc)?;
            let replicas = def.service(svc)?.replicas();
            let st = self.inner.ctl.status(q)?;
            if let Some(s) = st.services.iter().find(|s| s.service == svc) {
                if s.rev == rev && s.replicas == replicas {
                    match s.state.as_str() {
                        "converged" => return Ok((true, String::new())),
                        "paused" | "failing" => {
                            return Ok((
                                false,
                                format!("{}: {}", s.state, s.message.clone().unwrap_or_default()),
                            ));
                        }
                        _ => {}
                    }
                }
            }
            if started.elapsed() >= self.inner.timeout {
                return Ok((
                    false,
                    format!("not converged after {:?}", self.inner.timeout),
                ));
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    pub(super) fn credentials(&self, org: &OrgId, auth: &GitAuth) -> Result<Credentials> {
        let read = |n: &str| {
            self.inner
                .secrets
                .get(org, n)
                .map(|(v, _)| v)
                .map_err(|e| Error::invalid(format!("git credential {n}: {e}")))
        };
        Ok(match auth {
            GitAuth::None => Credentials::None,
            GitAuth::Token {
                token_secret,
                username,
            } => Credentials::Token {
                username: username.clone().unwrap_or_else(|| "x-access-token".into()),
                token: String::from_utf8(read(token_secret)?)
                    .map_err(|_| Error::invalid("the git token is not text"))?
                    .trim()
                    .to_string(),
            },
            GitAuth::SshKey { ssh_key_secret } => Credentials::SshKey {
                private_key: read(ssh_key_secret)?,
            },
        })
    }

    /// Drop the oldest records beyond [`KEEP_DEPLOYMENTS`], never the
    /// current one.
    fn prune(&self, org: &OrgId, app: &App) {
        let Ok(rd) = std::fs::read_dir(self.deployments_dir(org, &app.spec.name)) else {
            return;
        };
        let mut ids: Vec<u64> = rd
            .flatten()
            .filter_map(|e| {
                e.file_name()
                    .into_string()
                    .ok()?
                    .strip_suffix(".json")?
                    .parse()
                    .ok()
            })
            .collect();
        ids.sort_unstable();
        let excess = ids.len().saturating_sub(KEEP_DEPLOYMENTS);
        for id in ids.into_iter().take(excess) {
            if Some(id) == app.current {
                continue;
            }
            let _ = std::fs::remove_file(self.dep_path(org, &app.spec.name, id));
            let _ = std::fs::remove_file(self.log_path(org, &app.spec.name, id));
        }
    }

    /// Mark deployments a stopped daemon left unfinished as failed.
    fn recover(&self) {
        let mut orgs = vec![OrgId::default_org()];
        if let Ok(rd) = std::fs::read_dir(self.inner.state.join("orgs")) {
            for e in rd.flatten() {
                if let Some(o) = e.file_name().to_str().and_then(|s| OrgId::new(s).ok()) {
                    orgs.push(o);
                }
            }
        }
        for org in orgs {
            for app in self.list(&org).unwrap_or_default() {
                for mut d in self.deployments(&org, &app.spec.name).unwrap_or_default() {
                    if d.status.finished() {
                        continue;
                    }
                    d.error = Some("interrupted: the daemon stopped during it".into());
                    d.status = Status::Failed;
                    d.finished_at = Some(crate::stack::controller::now_ms());
                    let _ = self.save_dep(&org, &d);
                }
                self.recover_previews(&org, &app.spec.name);
            }
        }
    }

    /// An event about an app on the daemon's feed, under its stack.
    fn event(&self, org: &OrgId, app: &str, level: &str, message: String) {
        let stack = self
            .get(org, app)
            .ok()
            .and_then(|a| a.spec.stack().ok())
            .unwrap_or_default();
        let q = crate::stack::qualified(org, &stack);
        self.inner
            .ctl
            .note_service(level, &q, app, format!("app {app}: {message}"));
    }

    /// [`Apps::event`] with a kind ([`crate::stack::controller::Event::kind`]).
    fn kind_event(&self, org: &OrgId, app: &str, kind: &str, level: &str, message: String) {
        let stack = self
            .get(org, app)
            .ok()
            .and_then(|a| a.spec.stack().ok())
            .unwrap_or_default();
        let q = crate::stack::qualified(org, &stack);
        self.inner
            .ctl
            .event(kind, level, &q, app, format!("app {app}: {message}"));
    }

    // --- webhooks --------------------------------------------------------

    /// Answer `POST /api/v1/webhooks/<org>/<app>`: `(status, body)`. Nothing
    /// happens without a valid signature or token; an unknown org or app
    /// answers as a bad signature does, so the endpoint maps nothing.
    pub fn webhook(
        &self,
        org: &str,
        app: &str,
        header: &dyn Fn(&str) -> Option<String>,
        token: Option<&str>,
        body: &[u8],
    ) -> (u16, Value) {
        let refuse = |why: &str| (401, json!({"error": "unauthorized", "message": why}));
        let (Ok(org), Ok(())) = (OrgId::new(org), super::validate_app_name(app)) else {
            return refuse("invalid signature");
        };
        let Ok(found) = self.get(&org, app) else {
            return refuse("invalid signature");
        };
        let Ok((secret, _)) = self.inner.secrets.get(&org, &found.spec.webhook_secret()) else {
            return refuse("invalid signature");
        };
        let provider = match webhook::verify(trim_ascii(&secret), header, token, body) {
            Ok(p) => p,
            Err(webhook::Refusal::Missing) => {
                return refuse(
                    "sign the request (X-Hub-Signature-256, X-Gitea-Signature, X-Gitlab-Token) or pass ?token=",
                );
            }
            Err(webhook::Refusal::Invalid) => return refuse("invalid signature"),
        };
        if let Some(id) = webhook::delivery_id(header) {
            let key = format!("{org}/{app}/{id}");
            let mut seen = self.inner.deliveries.lock().unwrap();
            if seen.contains(&key) {
                return (200, json!({"ignored": "delivery already received"}));
            }
            if seen.len() >= 1000 {
                seen.pop_front();
            }
            seen.push_back(key);
        }
        let by = format!("webhook:{provider:?}").to_lowercase();
        let requested = match webhook::event(provider, header, body) {
            webhook::Event::Ping => return (200, json!({"ok": true, "ping": true})),
            webhook::Event::Other(kind) => {
                return (200, json!({"ignored": format!("event {kind}")}));
            }
            webhook::Event::Trigger => None,
            webhook::Event::PullRequest(pr) => {
                return self.preview_webhook(&org, &found, provider, &by, &pr);
            }
            webhook::Event::Push {
                reference,
                after,
                message,
                deleted,
            } => {
                if deleted {
                    return (200, json!({"ignored": format!("{reference} was deleted")}));
                }
                if let Source::Git(g) = &found.spec.source {
                    if !g.matches_push(&reference) {
                        return (
                            200,
                            json!({"ignored": format!("{reference} is not {}", g.reference)}),
                        );
                    }
                }
                let mut r = reference;
                if let Some(a) = after {
                    r = format!("{r} {a}");
                }
                if let Some(m) = message {
                    r = format!("{r}: {m}");
                }
                Some(r)
            }
        };
        match self.deploy(&org, app, Trigger::Webhook, &by, requested) {
            Ok(d) => (202, json!({"deployment": d.id, "status": d.status})),
            Err(e) => (500, json!({"error": "deploy", "message": e.to_string()})),
        }
    }
}

fn trim_ascii(b: &[u8]) -> &[u8] {
    let s = b
        .iter()
        .position(|c| !c.is_ascii_whitespace())
        .unwrap_or(b.len());
    let e = b
        .iter()
        .rposition(|c| !c.is_ascii_whitespace())
        .map_or(s, |i| i + 1);
    &b[s..e]
}

/// A deployment's log: a file, and each line on the events feed.
struct DeployLog {
    file: std::fs::File,
    apps: Apps,
    org: OrgId,
    app: String,
    id: u64,
}

impl DeployLog {
    fn open(apps: &Apps, org: &OrgId, app: &str, id: u64) -> Result<DeployLog> {
        use std::os::unix::fs::OpenOptionsExt;
        let p = apps.log_path(org, app, id);
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(p)?;
        Ok(DeployLog {
            file,
            apps: apps.clone(),
            org: org.clone(),
            app: app.into(),
            id,
        })
    }

    fn line(&mut self, l: &str) {
        let _ = writeln!(self.file, "{l}");
        self.apps
            .event(&self.org, &self.app, "log", format!("#{}: {l}", self.id));
    }
}

/// The digest `skopeo inspect` reports for an OCI image, if skopeo is
/// installed and the registry answers within a minute.
pub fn skopeo_digest(image: &str) -> Option<String> {
    let src = crate::plan::ImageSource::parse(image).ok()?;
    if !src.is_oci() {
        return None;
    }
    let host = src.server.as_deref()?.strip_prefix("https://")?;
    let r = format!("docker://{host}/{}", src.alias);
    let mut child = std::process::Command::new("skopeo")
        .args(["inspect", "--no-tags", "--format", "{{.Digest}}", &r])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(s)) if s.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if started.elapsed() > Duration::from_secs(60) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    let mut out = String::new();
    use std::io::Read;
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    let d = out.trim();
    (d.starts_with("sha256:") && d.len() == 71).then(|| d.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::EnvValue;

    #[test]
    fn state_machine() {
        use Status::*;
        let mut d = Deployment {
            id: 1,
            app: "web".into(),
            trigger: Trigger::Api,
            by: "t".into(),
            status: Queued,
            requested: None,
            rollback_of: None,
            commit: None,
            image: None,
            digest: None,
            error: None,
            created_at: 0,
            started_at: None,
            finished_at: None,
            rendered: None,
        };
        assert!(d.advance(Done).is_err(), "queued cannot jump to done");
        d.advance(Building).unwrap();
        assert!(d.started_at.is_some());
        assert!(
            d.advance(Superseded).is_err(),
            "only a waiting one is superseded"
        );
        d.advance(Deploying).unwrap();
        d.advance(Done).unwrap();
        assert!(d.finished_at.is_some());
        for s in [Queued, Building, Deploying, Done, Failed, Superseded] {
            assert!(!Done.can_become(s) && !Failed.can_become(s) && !Superseded.can_become(s));
        }
        assert!(Queued.can_become(Superseded));
        assert!(Building.can_become(Failed));
        let v = d.summary();
        assert!(v.get("rendered").is_none());
        assert_eq!(v["status"], "done");
        assert_eq!(v["trigger"], "api");
    }

    /// An `Apps` over a controller with no incusd behind it: everything up
    /// to the stack deploy works, which then fails.
    fn apps(dir: &Path, gate: Arc<(Mutex<bool>, std::sync::Condvar)>) -> Apps {
        let k = crate::secrets::Keyring::new(age::x25519::Identity::generate(), vec![]);
        let secrets = Arc::new(Secrets::new(crate::secrets::LocalDriver::new(
            dir,
            Arc::new(k),
        )));
        let client = Client::with_socket("/nonexistent/isb-test/incus.sock");
        let store = crate::stack::Store::open(dir).unwrap();
        let ctl = Controller::start(
            client.clone(),
            store,
            Duration::from_secs(60),
            secrets.clone(),
        )
        .unwrap();
        // `docker:slow` holds its deploy in `building` until the gate opens.
        let digest: DigestFn = Arc::new(move |image: &str| {
            if image == "docker:slow" {
                let (m, cv) = &*gate;
                let mut open = m.lock().unwrap();
                while !*open {
                    open = cv.wait(open).unwrap();
                }
            }
            None
        });
        Apps::new(dir, client, ctl, secrets).with_digest(digest)
    }

    fn spec(v: Value) -> AppSpec {
        serde_json::from_value(v).unwrap()
    }

    fn hdrs(h: &[(&str, &str)]) -> Box<webhook::Headers<'static>> {
        let h: Vec<(String, String)> = h
            .iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v.to_string()))
            .collect();
        Box::new(move |k: &str| {
            h.iter()
                .find(|(n, _)| *n == k.to_ascii_lowercase())
                .map(|(_, v)| v.clone())
        })
    }

    #[test]
    fn apps_records_webhooks_and_queue() {
        let dir = tempfile::tempdir().unwrap();
        let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let ap = apps(dir.path(), gate.clone());
        let org = OrgId::new("acme").unwrap();

        // Projects and environments.
        let p = ap.project_create(&org, "shop", "", &[]).unwrap();
        assert_eq!(p.environments, ["production"]);
        assert!(ap.project_create(&org, "shop", "", &[]).is_err());
        ap.environment_create(&org, "shop", "staging").unwrap();
        assert!(
            dir.path()
                .join("orgs/acme/apps/projects/shop.json")
                .is_file()
        );

        // An app naming a secret that does not exist is refused.
        let web = json!({
            "name": "web", "project": "shop",
            "source": {"image": "docker:traefik/whoami"},
            "env": "# greeting\nA=1\nT=${{secret.tok}}\n",
        });
        assert!(ap.create(&org, spec(web.clone())).is_err());
        ap.inner.secrets.set(&org, "tok", b"v").unwrap();
        let (app, secret) = ap.create(&org, spec(web.clone())).unwrap();
        assert_eq!(secret.len(), 64);
        assert_eq!(app.spec.environment, "production");
        assert!(ap.create(&org, spec(web)).is_err(), "names are unique");
        let mut bad = spec(
            json!({"name": "x", "project": "shop", "environment": "qa", "source": {"image": "x"}}),
        );
        assert!(ap.create(&org, bad.clone()).is_err(), "no such environment");
        bad.project = "nope".into();
        assert!(ap.create(&org, bad).is_err(), "no such project");

        // The env editor keeps comments and never shows a secret's value.
        let text = ap.env_get(&org, "web").unwrap();
        assert_eq!(text, "# greeting\nA=1\nT=${{secret.tok}}\n");
        ap.env_set(
            &org,
            "web",
            "# greeting\nA=2\nT=${{secret.tok}}\nB=\"two words\"\n",
        )
        .unwrap();
        assert!(
            ap.env_get(&org, "web")
                .unwrap()
                .contains("A=2\nT=${{secret.tok}}\nB=\"two words\"")
        );
        assert!(ap.env_set(&org, "web", "T=${{secret.missing}}\n").is_err());
        let u = ap
            .update(&org, "web", &json!({"replicas": 3, "port": 80}))
            .unwrap();
        assert_eq!(u.spec.replicas, 3);
        assert_eq!(u.spec.env.get("A"), Some(&EnvValue::Plain("2".into())));
        assert!(
            ap.update(&org, "web", &json!({"project": "other"}))
                .is_err()
        );
        assert!(
            ap.update(&org, "web", &json!({"port": null}))
                .unwrap()
                .spec
                .port
                .is_none()
        );
        assert!(ap.rollback(&org, "web", None, Trigger::Api, "t").is_err());

        // Webhooks: refused without the secret, whatever the app.
        let push = br#"{"ref":"refs/heads/main","after":"abc","head_commit":{"message":"m"}}"#;
        let none = hdrs(&[]);
        assert_eq!(ap.webhook("acme", "web", &none, None, push).0, 401);
        assert_eq!(ap.webhook("acme", "web", &none, Some("wrong"), push).0, 401);
        assert_eq!(
            ap.webhook("acme", "nope", &none, Some(&secret), push).0,
            401
        );
        assert_eq!(
            ap.webhook("Bad Org", "web", &none, Some(&secret), push).0,
            401
        );
        let forged = hdrs(&[
            ("X-GitHub-Event", "push"),
            (
                "X-Hub-Signature-256",
                &format!("sha256={}", webhook::sign(b"other", push)),
            ),
        ]);
        assert_eq!(ap.webhook("acme", "web", &forged, None, push).0, 401);
        assert!(ap.deployments(&org, "web").unwrap().is_empty());
        let signed = |event: &str, delivery: &str, body: &[u8]| {
            hdrs(&[
                ("X-GitHub-Event", event),
                ("X-GitHub-Delivery", delivery),
                (
                    "X-Hub-Signature-256",
                    &format!("sha256={}", webhook::sign(secret.as_bytes(), body)),
                ),
            ])
        };
        let (st, v) = ap.webhook("acme", "web", &signed("ping", "d0", b"{}"), None, b"{}");
        assert_eq!((st, v["ping"].as_bool()), (200, Some(true)));
        let (st, v) = ap.webhook("acme", "web", &signed("issues", "d1", push), None, push);
        assert_eq!(st, 200);
        assert!(v["ignored"].as_str().unwrap().contains("issues"), "{v}");
        let (st, v) = ap.webhook("acme", "web", &signed("push", "d2", push), None, push);
        assert_eq!(st, 202, "{v}");
        assert_eq!(v["deployment"], 1);
        // The same delivery again is not a second deploy.
        let (st, v) = ap.webhook("acme", "web", &signed("push", "d2", push), None, push);
        assert_eq!(st, 200, "{v}");
        let d = ap.wait(&org, "web", 1, Duration::from_secs(30)).unwrap();
        assert_eq!(d.trigger, Trigger::Webhook);
        assert_eq!(d.by, "webhook:github");
        assert_eq!(d.requested.as_deref(), Some("refs/heads/main abc: m"));
        // No incusd here: the stack deploy fails, and says so.
        assert_eq!(d.status, Status::Failed, "{d:?}");
        assert!(d.image.is_some());
        let (log, _, done) = ap.log(&org, "web", 1, 0).unwrap();
        assert!(done && log.contains("image docker:traefik/whoami"), "{log}");
        let ds = ap.deployments(&org, "web").unwrap();
        assert_eq!(ds.len(), 1);

        // A git app deploys only on pushes to its branch.
        ap.inner.secrets.set(&org, "gh", b"t").unwrap();
        let (_, gsecret) = ap
            .create(
                &org,
                spec(json!({
                    "name": "api", "project": "shop", "environment": "staging",
                    "source": {"git": {"url": "https://example.invalid/o/r.git", "ref": "main", "auth": {"token_secret": "gh"}}},
                    "build": {"builder": {"type": "railpack"}},
                })),
            )
            .unwrap();
        let dev = br#"{"ref":"refs/heads/dev","after":"abc"}"#;
        let gl = |body: &[u8]| {
            let _ = body;
            hdrs(&[
                ("X-Gitlab-Event", "Push Hook"),
                ("X-Gitlab-Token", &gsecret),
            ])
        };
        let (st, v) = ap.webhook("acme", "api", &gl(dev), None, dev);
        assert_eq!(st, 200);
        assert!(
            v["ignored"].as_str().unwrap().contains("refs/heads/dev"),
            "{v}"
        );
        assert!(ap.deployments(&org, "api").unwrap().is_empty());
        let (st, _) = ap.webhook("acme", "api", &gl(push), None, push);
        assert_eq!(st, 202);
        let d = ap.wait(&org, "api", 1, Duration::from_secs(60)).unwrap();
        assert_eq!(d.status, Status::Failed);
        assert!(d.error.as_deref().unwrap_or("").contains("git"), "{d:?}");
        // A rotated secret: the old one stops working.
        let s2 = ap.webhook_secret(&org, "api", true).unwrap();
        assert_ne!(s2, gsecret);
        assert_eq!(ap.webhook("acme", "api", &gl(push), None, push).0, 401);

        // The queue: one at a time; a newer request replaces a waiting one.
        ap.create(
            &org,
            spec(json!({"name": "slow", "project": "shop", "source": {"image": "docker:slow"}})),
        )
        .unwrap();
        let d1 = ap.deploy(&org, "slow", Trigger::Api, "t", None).unwrap();
        let started = Instant::now();
        while ap.deployment(&org, "slow", d1.id).unwrap().status != Status::Building {
            assert!(started.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(20));
        }
        let d2 = ap.deploy(&org, "slow", Trigger::Api, "t", None).unwrap();
        let d3 = ap.deploy(&org, "slow", Trigger::Manual, "t", None).unwrap();
        assert_eq!(
            ap.deployment(&org, "slow", d2.id).unwrap().status,
            Status::Superseded
        );
        assert_eq!(
            ap.deployment(&org, "slow", d3.id).unwrap().status,
            Status::Queued
        );
        assert!(ap.delete(&org, "slow").is_err(), "not while deploying");
        {
            let (m, cv) = &*gate;
            *m.lock().unwrap() = true;
            cv.notify_all();
        }
        let d3 = ap
            .wait(&org, "slow", d3.id, Duration::from_secs(30))
            .unwrap();
        assert!(d3.status.finished());
        assert!(
            ap.wait(&org, "slow", d1.id, Duration::from_secs(1))
                .unwrap()
                .status
                .finished()
        );

        // Deleting: projects with apps stay; an app takes its secrets along.
        assert!(ap.project_delete(&org, "shop").is_err());
        assert!(ap.environment_delete(&org, "shop", "staging").is_err());
        let started = Instant::now();
        for a in ["web", "api", "slow"] {
            while let Err(e) = ap.delete(&org, a) {
                assert!(started.elapsed() < Duration::from_secs(10), "{a}: {e}");
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        assert!(ap.inner.secrets.inspect(&org, "app.web.webhook").is_err());
        assert!(
            ap.inner.secrets.inspect(&org, "tok").is_ok(),
            "not isb's to delete"
        );
        ap.environment_delete(&org, "shop", "staging").unwrap();
        ap.project_delete(&org, "shop").unwrap();
        assert!(ap.project_list(&org).unwrap().is_empty());
    }

    #[test]
    fn previews_from_webhooks() {
        let dir = tempfile::tempdir().unwrap();
        let gate = Arc::new((Mutex::new(true), std::sync::Condvar::new()));
        let ap = apps(dir.path(), gate);
        let org = OrgId::new("acme").unwrap();
        ap.project_create(&org, "shop", "", &[]).unwrap();
        assert!(
            ap.environment_create(&org, "shop", "prod-pr-1").is_err(),
            "preview stack names are kept"
        );
        let (_, secret) = ap
            .create(
                &org,
                spec(json!({
                    "name": "web", "project": "shop",
                    "source": {"git": {"url": "https://example.invalid/acme/web.git", "ref": "main"}},
                    "build": {"builder": {"type": "dockerfile"}},
                    "port": 8080,
                })),
            )
            .unwrap();
        let pr = |action: &str, n: u64, head_repo: &str, base: &str| {
            format!(
                r#"{{"action":"{action}","number":{n},"pull_request":{{"title":"t{n}","merged":false,
                "head":{{"ref":"f{n}","sha":"{}","repo":{{"full_name":"{head_repo}"}}}},
                "base":{{"ref":"{base}","repo":{{"full_name":"acme/web"}}}}}}}}"#,
                "a".repeat(40)
            )
            .into_bytes()
        };
        let mut delivery = 0;
        let mut send = |body: &[u8]| {
            delivery += 1;
            let h = hdrs(&[
                ("X-Gitea-Event", "pull_request"),
                ("X-Gitea-Delivery", &format!("p{delivery}")),
                ("X-Gitea-Signature", &webhook::sign(secret.as_bytes(), body)),
            ]);
            ap.webhook("acme", "web", &h, None, body)
        };
        // Previews are opt-in.
        let (st, v) = send(&pr("opened", 1, "acme/web", "main"));
        assert_eq!(st, 200);
        assert!(v["ignored"].as_str().unwrap().contains("off"), "{v}");
        ap.update(
            &org,
            "web",
            &json!({"previews": {"enabled": true, "max": 2, "env": "MODE=preview\n"}}),
        )
        .unwrap();
        // Another base branch, a fork: ignored.
        let (st, v) = send(&pr("opened", 1, "acme/web", "dev"));
        assert_eq!(st, 200);
        assert!(
            v["ignored"].as_str().unwrap().contains("targets dev"),
            "{v}"
        );
        let (st, v) = send(&pr("opened", 1, "mallory/web", "main"));
        assert_eq!(st, 200);
        assert!(v["ignored"].as_str().unwrap().contains("fork"), "{v}");
        assert!(ap.preview_list(&org, "web").unwrap().is_empty());
        // Opened: a preview and a deploy (which fails here: no git host).
        let (st, v) = send(&pr("opened", 1, "acme/web", "main"));
        assert_eq!(st, 202, "{v}");
        assert_eq!(
            (v["preview"].as_u64(), v["deployment"].as_u64()),
            (Some(1), Some(1))
        );
        let p = ap.preview_get(&org, "web", 1).unwrap();
        assert_eq!(p.stack, "shop-production-pr-1");
        assert_eq!((p.provider.as_str(), p.fork), ("gitea", false));
        let d = ap
            .preview_wait(&org, "web", 1, 1, Duration::from_secs(60))
            .unwrap();
        assert_eq!(d.status, Status::Failed, "{d:?}");
        let (log, _, done) = ap.preview_log(&org, "web", 1, 1, 0).unwrap();
        assert!(done && log.contains("refs/pull/1/head"), "{log}");
        // Synchronize to the commit it has (Gitea sends one after opening):
        // nothing; to a new one: the same preview, a second deployment.
        let (st, v) = send(&pr("synchronize", 1, "acme/web", "main"));
        assert_eq!(st, 200, "{v}");
        assert!(v["ignored"].as_str().unwrap().contains("already"), "{v}");
        let moved = String::from_utf8(pr("synchronize", 1, "acme/web", "main"))
            .unwrap()
            .replace(&"a".repeat(40), &"b".repeat(40));
        let (st, v) = send(moved.as_bytes());
        assert_eq!((st, v["deployment"].as_u64()), (202, Some(2)), "{v}");
        ap.preview_wait(&org, "web", 1, 2, Duration::from_secs(60))
            .unwrap();
        // The limit: two at once.
        assert_eq!(send(&pr("opened", 2, "acme/web", "main")).0, 202);
        let (st, v) = send(&pr("opened", 3, "acme/web", "main"));
        assert_eq!(st, 200);
        assert!(v["ignored"].as_str().unwrap().contains("2 previews"), "{v}");
        assert!(ap.preview_get(&org, "web", 3).is_err());
        ap.preview_wait(&org, "web", 2, 1, Duration::from_secs(60))
            .unwrap();
        // Forks when allowed: marked, so they build in a VM without the
        // app's secrets.
        ap.update(&org, "web", &json!({"previews": {"forks": true, "max": 5}}))
            .unwrap();
        assert_eq!(send(&pr("opened", 4, "mallory/web", "main")).0, 202);
        assert!(ap.preview_get(&org, "web", 4).unwrap().fork);
        ap.preview_wait(&org, "web", 4, 1, Duration::from_secs(60))
            .unwrap();
        // Closed: removed with its records.
        let (st, v) = send(&pr("closed", 1, "acme/web", "main"));
        assert_eq!(st, 202, "{v}");
        let started = Instant::now();
        while ap.preview_get(&org, "web", 1).is_ok() {
            assert!(started.elapsed() < Duration::from_secs(30));
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!dir.path().join("orgs/acme/apps/web/previews/1").exists());
        let (st, v) = send(&pr("closed", 1, "acme/web", "main"));
        assert_eq!(st, 200, "{v}");
        // The tools' path: redeploy, then delete.
        let d = ap
            .preview_redeploy(&org, "web", 2, Trigger::Api, "t")
            .unwrap();
        assert_eq!(d.id, 2);
        ap.preview_wait(&org, "web", 2, 2, Duration::from_secs(60))
            .unwrap();
        ap.preview_remove(&org, "web", 2, "test", true).unwrap();
        assert!(ap.preview_get(&org, "web", 2).is_err());
        // Deleting the app takes its previews along first; here the fork's
        // build cache cannot be checked without incusd, so it stops there.
        assert!(ap.delete(&org, "web").is_err());
        assert!(ap.get(&org, "web").is_ok());
        assert!(ap.preview_get(&org, "web", 4).unwrap().removing);
    }
}
