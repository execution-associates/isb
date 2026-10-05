//! Scheduled jobs: a command run on a cron schedule against an app or a
//! stack service of an org.
//!
//! Two ways to run:
//! - `exec` (default): in a running replica of the service, like
//!   `isb exec` (the instance's environment, secrets included).
//! - `run`: in a fresh one-off instance made from the service's deployed
//!   image, environment and secrets (no published ports, no volumes, no
//!   health check), deleted afterwards.
//!
//! Each run has a timeout and a record (status, exit code, duration, a
//! bounded log) kept under the state directory, newest `keep`. A run that
//! comes due while the previous one is still going is skipped (`concurrency:
//! skip`, the default) or started anyway (`allow`). A finished run emits
//! `job.succeeded` or `job.failed`.
//!
//! ```text
//! <org root>/jobs/<name>/job.json
//! <org root>/jobs/<name>/runs/<id>.json, <id>.log
//! ```

pub mod runs;
pub mod scheduler;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::app::Apps;
use crate::cron::Schedule;
use crate::error::{Error, Result};
use crate::exec::{ExecEvent, ExecOptions};
use crate::org::OrgId;
use crate::sandbox::Sandbox;
pub use runs::{Run, RunLog, RunStatus, RunStore, RunTrigger};
pub use scheduler::{Entry, Scheduled, Scheduler};

/// The longest a job may run.
pub const MAX_TIMEOUT: Duration = Duration::from_secs(24 * 3600);

/// What a job runs against.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    /// An app (its service in its project environment's stack).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// Or a stack and one of its services.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// In a running replica.
    #[default]
    Exec,
    /// In a fresh one-off instance from the service's image.
    Run,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Concurrency {
    /// A run that comes due while one is going is skipped (recorded).
    #[default]
    Skip,
    /// Runs may overlap.
    Allow,
}

fn yes() -> bool {
    true
}

fn default_timeout() -> String {
    "10m".into()
}

fn default_keep() -> u32 {
    20
}

/// What a user sets on a job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobSpec {
    pub name: String,
    /// Five cron fields or an alias (`@hourly`, `@daily`, ...).
    pub schedule: String,
    /// `UTC` (default) or a fixed offset such as `+02:00`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    pub target: Target,
    #[serde(default)]
    pub mode: Mode,
    /// argv (no shell unless you run one: `[sh, -c, ...]`).
    pub command: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout: String,
    #[serde(default)]
    pub concurrency: Concurrency,
    /// Runs kept.
    #[serde(default = "default_keep")]
    pub keep: u32,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Extra variables for the command.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// How late a slot missed while the daemon was down may still run
    /// (default 1h; `0s` never runs missed slots).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missed_grace: Option<String>,
}

/// A job as stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub spec: JobSpec,
    pub created_at: u64,
    pub updated_at: u64,
    /// The last slot it fired for (Unix seconds; creation time before).
    pub anchor: i64,
}

/// A job name: `[a-z0-9-]`, a path component and part of instance names.
pub fn validate_name(what: &str, s: &str) -> Result<()> {
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
            "{what} name {s:?}: up to 30 characters of [a-z0-9-], starting with a letter"
        )))
    }
}

/// A duration setting, bounded.
pub fn parse_timeout(s: &str) -> Result<Duration> {
    let d = crate::flex::parse_duration(s).map_err(Error::invalid)?;
    if d.is_zero() || d > MAX_TIMEOUT {
        return Err(Error::invalid(format!(
            "timeout {s:?}: more than 0 and at most 24h"
        )));
    }
    Ok(d)
}

/// A grace window in seconds (default [`scheduler::DEFAULT_GRACE`]).
pub fn parse_grace(s: &Option<String>) -> Result<i64> {
    match s {
        None => Ok(scheduler::DEFAULT_GRACE),
        Some(g) => {
            let d = crate::flex::parse_duration(g).map_err(Error::invalid)?;
            Ok(d.as_secs().min(31 * 86_400) as i64)
        }
    }
}

impl JobSpec {
    pub fn schedule(&self) -> Result<Schedule> {
        let off = crate::cron::parse_offset(self.timezone.as_deref().unwrap_or("UTC"))?;
        Schedule::parse_in(&self.schedule, off)
    }

    pub fn validate(&self) -> Result<()> {
        validate_name("job", &self.name)?;
        self.schedule()?;
        match (&self.target.app, &self.target.stack, &self.target.service) {
            (Some(a), None, None) => crate::app::validate_app_name(a)?,
            (None, Some(st), Some(_)) => crate::stack::validate_stack_name(st)?,
            _ => {
                return Err(Error::invalid(
                    "target: {app: NAME}, or {stack: NAME, service: NAME}",
                ));
            }
        }
        if self.command.is_empty() || self.command[0].is_empty() {
            return Err(Error::invalid("command: argv, at least the program"));
        }
        parse_timeout(&self.timeout)?;
        parse_grace(&self.missed_grace)?;
        if self.keep == 0 || self.keep > 1000 {
            return Err(Error::invalid("keep: 1 to 1000 runs"));
        }
        Ok(())
    }
}

/// Runs in progress, by (org, kind, name): the concurrency policy's view.
#[derive(Default)]
pub struct Running {
    set: Mutex<BTreeMap<(OrgId, String, String), usize>>,
}

/// Held while a run goes; dropping it marks the run over.
pub struct RunGuard {
    r: Arc<Running>,
    key: (OrgId, String, String),
}

impl Running {
    /// Start a run of `kind`/`name`: `None` when one is going and `skip`.
    pub fn enter(
        self: &Arc<Self>,
        org: &OrgId,
        kind: &str,
        name: &str,
        skip: bool,
    ) -> Option<RunGuard> {
        let key = (org.clone(), kind.to_string(), name.to_string());
        let mut s = self.set.lock().unwrap();
        let n = s.entry(key.clone()).or_default();
        if *n > 0 && skip {
            return None;
        }
        *n += 1;
        Some(RunGuard {
            r: self.clone(),
            key,
        })
    }

    pub fn is_running(&self, org: &OrgId, kind: &str, name: &str) -> bool {
        self.set
            .lock()
            .unwrap()
            .get(&(org.clone(), kind.to_string(), name.to_string()))
            .is_some_and(|n| *n > 0)
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        let mut s = self.r.set.lock().unwrap();
        if let Some(n) = s.get_mut(&self.key) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                s.remove(&self.key);
            }
        }
    }
}

struct Inner {
    state: PathBuf,
    apps: Apps,
    running: Arc<Running>,
    edit: Mutex<()>,
    scheduler: Mutex<Scheduler>,
}

/// Every org's jobs.
#[derive(Clone)]
pub struct Jobs {
    inner: Arc<Inner>,
}

/// Every org with a state directory (the default org always).
pub fn orgs(state: &Path) -> Vec<OrgId> {
    let mut out = vec![OrgId::default_org()];
    if let Ok(rd) = std::fs::read_dir(state.join("orgs")) {
        for e in rd.flatten() {
            if let Some(o) = e.file_name().to_str().and_then(|s| OrgId::new(s).ok()) {
                if !o.is_default() {
                    out.push(o);
                }
            }
        }
    }
    out
}

/// The stack and service a target names, qualified by org.
pub fn resolve_target(apps: &Apps, org: &OrgId, t: &Target) -> Result<(String, String)> {
    match (&t.app, &t.stack, &t.service) {
        (Some(a), _, _) => {
            let app = apps.get(org, a)?;
            Ok((crate::stack::qualified(org, &app.spec.stack()?), a.clone()))
        }
        (None, Some(st), Some(svc)) => Ok((crate::stack::qualified(org, st), svc.clone())),
        _ => Err(Error::invalid("target: an app, or a stack and a service")),
    }
}

/// The name of a running replica of a stack's service (the lowest slot).
pub fn running_instance(
    client: &crate::client::Client,
    org: &OrgId,
    stack: &str,
    service: &str,
) -> Result<String> {
    let oc = crate::org::client(client, org);
    let name = stack.rsplit('/').next().unwrap_or(stack);
    let insts = crate::stack::controller::list_instances(&oc, name, Some(service))?;
    insts
        .iter()
        .find(|i| i.is_running())
        .map(|i| i.name.clone())
        .ok_or_else(|| {
            Error::invalid(format!(
                "service {service} of stack {name} has no running replica"
            ))
        })
}

/// Run argv in `instance`, streaming output into `log`. Returns the exit
/// code.
pub fn exec_logged(
    client: &crate::client::Client,
    org: &OrgId,
    instance: &str,
    argv: &[String],
    opts: ExecOptions,
    log: &mut RunLog,
) -> Result<i32> {
    let oc = crate::org::client(client, org);
    let sb = Sandbox::get(&oc, instance)?;
    let timeout = opts.timeout;
    let mut s = sb.exec_stream(argv.to_vec(), opts)?;
    while let Some(ev) = s.next_event() {
        match ev {
            ExecEvent::Stdout(b) | ExecEvent::Stderr(b) => log.write(&b),
        }
    }
    match s.wait() {
        Err(Error::ExecTimeout { .. }) => Err(Error::invalid(format!(
            "timed out after {} and was killed",
            timeout.map(|t| format!("{t:?}")).unwrap_or_default()
        ))),
        r => r,
    }
}

impl Jobs {
    pub fn new(state: &Path, apps: Apps) -> Jobs {
        let j = Jobs {
            inner: Arc::new(Inner {
                state: state.to_path_buf(),
                apps,
                running: Arc::default(),
                edit: Mutex::new(()),
                scheduler: Mutex::new(Scheduler::idle()),
            }),
        };
        for org in orgs(state) {
            for job in j.list(&org).unwrap_or_default() {
                j.runs(&org, &job.spec.name).recover();
            }
        }
        j
    }

    /// The scheduler to wake when schedules change.
    pub fn set_scheduler(&self, s: Scheduler) {
        *self.inner.scheduler.lock().unwrap() = s;
    }

    fn dir(&self, org: &OrgId) -> PathBuf {
        crate::app::org_root(&self.inner.state, org).join("jobs")
    }

    fn job_path(&self, org: &OrgId, name: &str) -> PathBuf {
        self.dir(org).join(name).join("job.json")
    }

    pub fn runs(&self, org: &OrgId, name: &str) -> RunStore {
        RunStore::new(self.dir(org).join(name).join("runs"))
    }

    pub fn get(&self, org: &OrgId, name: &str) -> Result<Job> {
        validate_name("job", name)?;
        match std::fs::read(self.job_path(org, name)) {
            Ok(b) => Ok(serde_json::from_slice(&b)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(Error::NotFound(format!("job {name} in org {org}")))
            }
            Err(e) => Err(e.into()),
        }
    }

    pub fn list(&self, org: &OrgId) -> Result<Vec<Job>> {
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir(self.dir(org)) else {
            return Ok(out);
        };
        for e in rd.flatten() {
            let p = e.path().join("job.json");
            if !p.is_file() {
                continue;
            }
            match serde_json::from_slice::<Job>(&std::fs::read(&p)?) {
                Ok(j) => out.push(j),
                Err(e) => eprintln!("isb serve: skipping {}: {e}", p.display()),
            }
        }
        out.sort_by(|a, b| a.spec.name.cmp(&b.spec.name));
        Ok(out)
    }

    fn save(&self, org: &OrgId, j: &Job) -> Result<()> {
        crate::app::write_atomic(
            &self.job_path(org, &j.spec.name),
            &serde_json::to_vec_pretty(j)?,
        )
    }

    /// The target must exist now (a typo is better caught at create).
    fn check_target(&self, org: &OrgId, spec: &JobSpec) -> Result<()> {
        let (stack, service) = resolve_target(&self.inner.apps, org, &spec.target)?;
        if spec.target.stack.is_some() {
            self.inner
                .apps
                .controller()
                .definition(&stack)?
                .service(&service)?;
        }
        Ok(())
    }

    pub fn create(&self, org: &OrgId, spec: JobSpec) -> Result<Job> {
        spec.validate()?;
        self.check_target(org, &spec)?;
        let _g = self.inner.edit.lock().unwrap();
        if self.job_path(org, &spec.name).exists() {
            return Err(Error::AlreadyExists(format!("job {}", spec.name)));
        }
        let now = crate::stack::now_secs();
        let j = Job {
            spec,
            created_at: now,
            updated_at: now,
            anchor: now as i64,
        };
        self.save(org, &j)?;
        self.inner.scheduler.lock().unwrap().wake();
        Ok(j)
    }

    /// A JSON merge patch of the spec (the name is fixed). A changed
    /// schedule counts from now.
    pub fn update(&self, org: &OrgId, name: &str, patch: &Value) -> Result<Job> {
        let _g = self.inner.edit.lock().unwrap();
        let mut j = self.get(org, name)?;
        let mut v = serde_json::to_value(&j.spec)?;
        crate::app::merge_patch(&mut v, patch);
        let spec: JobSpec =
            serde_json::from_value(v).map_err(|e| Error::invalid(format!("job {name}: {e}")))?;
        if spec.name != j.spec.name {
            return Err(Error::invalid("a job's name is fixed"));
        }
        spec.validate()?;
        self.check_target(org, &spec)?;
        let now = crate::stack::now_secs();
        if spec.schedule != j.spec.schedule
            || spec.timezone != j.spec.timezone
            || (spec.enabled && !j.spec.enabled)
        {
            j.anchor = now as i64;
        }
        j.spec = spec;
        j.updated_at = now;
        self.save(org, &j)?;
        self.inner.scheduler.lock().unwrap().wake();
        Ok(j)
    }

    pub fn delete(&self, org: &OrgId, name: &str) -> Result<()> {
        let _g = self.inner.edit.lock().unwrap();
        self.get(org, name)?;
        if self.inner.running.is_running(org, "job", name) {
            return Err(Error::invalid(format!(
                "job {name} is running; delete it once that run finishes"
            )));
        }
        std::fs::remove_dir_all(self.dir(org).join(name))?;
        Ok(())
    }

    /// When the job next fires (Unix seconds), if enabled.
    pub fn next_run(&self, j: &Job) -> Option<i64> {
        if !j.spec.enabled {
            return None;
        }
        j.spec.schedule().ok()?.next_after(j.anchor)
    }

    /// Run the job now, in the background. The run's record (or, when one
    /// is going and the policy is skip, a refusal).
    pub fn run_now(&self, org: &OrgId, name: &str, by: &str) -> Result<Run> {
        let j = self.get(org, name)?;
        self.start(org, j, RunTrigger::Manual, by, None)?
            .ok_or_else(|| {
                Error::invalid(format!("job {name} is still running (concurrency: skip)"))
            })
    }

    /// Start a run on its own thread. `None`: skipped (recorded as such).
    fn start(
        &self,
        org: &OrgId,
        j: Job,
        trigger: RunTrigger,
        by: &str,
        slot: Option<i64>,
    ) -> Result<Option<Run>> {
        let name = j.spec.name.clone();
        let store = self.runs(org, &name);
        let skip = j.spec.concurrency == Concurrency::Skip;
        let Some(guard) = self.inner.running.enter(org, "job", &name, skip) else {
            if trigger != RunTrigger::Manual {
                let (mut r, mut log) =
                    store.start("job", trigger, by, slot, j.spec.keep as usize)?;
                r.error = Some("the previous run was still going (concurrency: skip)".into());
                r.finish(RunStatus::Skipped);
                store.finish(&mut r, &mut log)?;
            }
            return Ok(None);
        };
        let (r, log) = store.start("job", trigger, by, slot, j.spec.keep as usize)?;
        let me = self.clone();
        let org2 = org.clone();
        let run = r.clone();
        std::thread::spawn(move || {
            let _guard = guard;
            me.execute(&org2, &j, run, log);
        });
        Ok(Some(r))
    }

    fn execute(&self, org: &OrgId, j: &Job, mut r: Run, mut log: RunLog) {
        let store = self.runs(org, &j.spec.name);
        let res = self.attempt(org, j, &mut log);
        let target = resolve_target(&self.inner.apps, org, &j.spec.target).ok();
        let (stack, service) = target.unwrap_or_default();
        let (kind, level, msg) = match res {
            Ok(0) => {
                r.exit_code = Some(0);
                r.finish(RunStatus::Succeeded);
                (
                    "job.succeeded",
                    "info",
                    format!("job {}: run {} succeeded", j.spec.name, r.id),
                )
            }
            Ok(code) => {
                r.exit_code = Some(code);
                r.error = Some(format!("exit code {code}"));
                r.finish(RunStatus::Failed);
                (
                    "job.failed",
                    "error",
                    format!(
                        "job {}: run {} failed with exit code {code}",
                        j.spec.name, r.id
                    ),
                )
            }
            Err(e) => {
                log.line(&format!("isb: {e}"));
                r.error = Some(e.to_string());
                r.finish(RunStatus::Failed);
                (
                    "job.failed",
                    "error",
                    format!("job {}: run {} failed: {e}", j.spec.name, r.id),
                )
            }
        };
        if let Err(e) = store.finish(&mut r, &mut log) {
            eprintln!("isb serve: job {}: run {}: {e}", j.spec.name, r.id);
        }
        let stack = if stack.is_empty() {
            crate::stack::qualified(org, "")
        } else {
            stack
        };
        self.inner
            .apps
            .controller()
            .event(kind, level, &stack, &service, msg);
    }

    fn attempt(&self, org: &OrgId, j: &Job, log: &mut RunLog) -> Result<i32> {
        let s = &j.spec;
        let timeout = parse_timeout(&s.timeout)?;
        let (stack, service) = resolve_target(&self.inner.apps, org, &s.target)?;
        let mut opts = ExecOptions::default().timeout(timeout);
        opts.user = s.user.clone();
        opts.cwd = s.cwd.clone();
        opts.env = s.env.clone();
        let client = self.inner.apps.client().clone();
        match s.mode {
            Mode::Exec => {
                let inst = running_instance(&client, org, &stack, &service)?;
                log.line(&format!("isb: exec in {inst}: {}", s.command.join(" ")));
                exec_logged(&client, org, &inst, &s.command, opts, log)
            }
            Mode::Run => {
                let def = self.inner.apps.controller().definition(&stack)?;
                let (mut spec, _) = one_off_spec(&def, &service, &s.name, timeout)?;
                // Files (`secrets:`, `as: file`) too: they are pushed below.
                let keys: Vec<String> = spec.secret_keys().into_iter().map(String::from).collect();
                let values = crate::stack::secrets::values(
                    self.inner.apps.secrets(),
                    org,
                    &def.secrets,
                    keys.iter().map(String::as_str),
                )?;
                let secret_env = crate::supervise::secret_env(&spec, &values)?;
                spec.env.secrets.clear();
                spec.env.vars.extend(secret_env);
                let name = spec.name.clone().unwrap_or_default();
                log.line(&format!("isb: one-off instance {name} from {}", spec.image));
                let oc = crate::org::client(&client, org);
                let base = self.dir(org);
                std::fs::create_dir_all(&base)?;
                let made = Sandbox::connect_or_create_with_base(
                    &oc,
                    &spec,
                    &Default::default(),
                    &base,
                    crate::sandbox::EnsureOptions {
                        wait_ready: true,
                        ..Default::default()
                    },
                    &mut |m| log.line(&format!("isb: {m}")),
                );
                let r = match made {
                    Ok((sb, _)) => {
                        if spec.has_secret_files() {
                            crate::supervise::push_secrets(&sb, &spec, &values)?;
                        }
                        log.line(&format!("isb: run: {}", s.command.join(" ")));
                        exec_logged(&client, org, &name, &s.command, opts, log)
                    }
                    Err(e) => Err(e),
                };
                if let Err(e) = Sandbox::remove(&oc, &name, true) {
                    if !e.is_not_found() {
                        log.line(&format!("isb: removing {name}: {e}"));
                    }
                }
                r
            }
        }
    }
}

/// A one-off instance's spec from a deployed service: its image (as its
/// instances get it), environment, secrets, resources and user; no ports,
/// volumes, domains, health check or replicas. An OCI image's command is
/// replaced by `sleep`, so the job's command runs as an exec beside it.
pub fn one_off_spec(
    def: &crate::stack::StackDef,
    service: &str,
    job: &str,
    timeout: Duration,
) -> Result<(crate::spec::SandboxSpec, bool)> {
    let svc = def.service(service)?;
    let image = def.instance_image(service, &svc.image);
    let oci = crate::plan::ImageSource::parse(&image)?.is_oci();
    let mut v = json!({
        "image": image,
        "container_name": format!("job-{job}-{}", crate::app::git::random_hex(3)),
        "labels": {"isb.job": job},
    });
    let src = serde_json::to_value(svc)?;
    for k in [
        "environment",
        "secrets",
        "cpus",
        "mem_limit",
        "user",
        "working_dir",
        "type",
    ] {
        if let Some(x) = src.get(k) {
            v[k] = x.clone();
        }
    }
    if oci {
        let secs = timeout.as_secs() + 300;
        v["entrypoint"] = json!([]);
        v["command"] = json!(["sleep", secs.to_string()]);
    }
    let spec: crate::spec::SandboxSpec = serde_json::from_value(v)
        .map_err(|e| Error::invalid(format!("one-off instance for {service}: {e}")))?;
    Ok((spec, oci))
}

impl Scheduled for Jobs {
    fn entries(&self) -> Vec<Entry> {
        let mut out = Vec::new();
        for org in orgs(&self.inner.state) {
            for j in self.list(&org).unwrap_or_default() {
                if !j.spec.enabled {
                    continue;
                }
                let (Ok(schedule), Ok(grace)) =
                    (j.spec.schedule(), parse_grace(&j.spec.missed_grace))
                else {
                    continue;
                };
                out.push(Entry {
                    org: org.clone(),
                    name: j.spec.name.clone(),
                    schedule,
                    anchor: j.anchor,
                    grace,
                });
            }
        }
        out
    }

    fn fire(&self, e: &Entry, slot: i64, late: bool) {
        let j = {
            let _g = self.inner.edit.lock().unwrap();
            let Ok(mut j) = self.get(&e.org, &e.name) else {
                return;
            };
            if j.anchor >= slot {
                return;
            }
            j.anchor = slot;
            if let Err(err) = self.save(&e.org, &j) {
                eprintln!("isb serve: job {}: {err}", e.name);
                return;
            }
            j
        };
        let trigger = if late {
            RunTrigger::Missed
        } else {
            RunTrigger::Schedule
        };
        if let Err(err) = self.start(&e.org, j, trigger, "schedule", Some(slot)) {
            eprintln!("isb serve: job {}: {err}", e.name);
        }
    }

    fn advance(&self, e: &Entry, to: i64) {
        let _g = self.inner.edit.lock().unwrap();
        if let Ok(mut j) = self.get(&e.org, &e.name) {
            j.anchor = to;
            let _ = self.save(&e.org, &j);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(v: Value) -> JobSpec {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn spec_validation() {
        let ok = spec(json!({
            "name": "nightly", "schedule": "0 3 * * *",
            "target": {"app": "web"}, "command": ["sh", "-c", "echo hi"],
        }));
        ok.validate().unwrap();
        assert_eq!(ok.mode, Mode::Exec);
        assert_eq!(ok.concurrency, Concurrency::Skip);
        assert_eq!(ok.keep, 20);
        for bad in [
            json!({"schedule": "61 * * * *"}),
            json!({"target": {"app": null}}),
            json!({"target": {"app": "web", "stack": "s", "service": "x"}}),
            json!({"target": {"app": null, "stack": "s"}}),
            json!({"command": []}),
            json!({"timeout": "0s"}),
            json!({"timeout": "48h"}),
            json!({"timezone": "Europe/Paris"}),
            json!({"keep": 0}),
            json!({"name": "Bad_Name"}),
        ] {
            let mut v = serde_json::to_value(&ok).unwrap();
            crate::app::merge_patch(&mut v, &bad);
            let s: JobSpec = serde_json::from_value(v).unwrap();
            assert!(s.validate().is_err(), "{bad}");
        }
        assert!(serde_json::from_value::<JobSpec>(json!({"name": "x", "schedule": "@daily", "target": {"app": "a"}, "command": ["x"], "bogus": 1})).is_err());
        let st = spec(json!({
            "name": "x", "schedule": "@hourly", "timezone": "+02:00",
            "target": {"stack": "shop-production", "service": "web"}, "command": ["true"],
            "mode": "run", "concurrency": "allow",
        }));
        st.validate().unwrap();
        assert_eq!(st.schedule().unwrap().as_str(), "@hourly");
        assert_eq!(parse_grace(&None).unwrap(), 3600);
        assert_eq!(parse_grace(&Some("0s".into())).unwrap(), 0);
    }

    #[test]
    fn concurrency_policy() {
        let r = Arc::new(Running::default());
        let org = OrgId::default_org();
        let g1 = r.enter(&org, "job", "a", true).expect("first run starts");
        assert!(r.is_running(&org, "job", "a"));
        assert!(
            r.enter(&org, "job", "a", true).is_none(),
            "skip while running"
        );
        assert!(
            r.enter(&org, "job", "b", true).is_some(),
            "other jobs are not held"
        );
        assert!(
            r.enter(&org, "backup", "a", true).is_some(),
            "other kinds neither"
        );
        {
            let _g2 = r.enter(&org, "job", "a", false).expect("allow overlaps");
        }
        assert!(r.is_running(&org, "job", "a"), "the first still runs");
        drop(g1);
        assert!(!r.is_running(&org, "job", "a"));
        assert!(r.enter(&org, "job", "a", true).is_some());
    }

    #[test]
    fn one_off_from_a_deployed_service() {
        let file: crate::spec::ComposeFile = serde_yaml_ng::from_str(
            "services:\n  web:\n    image: docker:traefik/whoami@sha256:abc\n    environment: {A: '1', T: {secret: web.tok}}\n    ports: ['127.0.0.1:8080:80']\n    volumes: ['d:/data']\n    deploy: {replicas: 3}\n    healthcheck: {test: [CMD, /x]}\n    mem_limit: 256m\nvolumes: {d: {}}\nsecrets: {web.tok: {external: true, name: tok}}\n",
        )
        .unwrap();
        let def = crate::stack::StackDef {
            source: None,
            domains: Default::default(),
            name: "shop-production".into(),
            org: OrgId::default_org(),
            file,
            base_dir: "/".into(),
            secrets: Default::default(),
            force: Default::default(),
            images: Default::default(),
            deployed_at: 0,
            deployed_by: String::new(),
            previous: None,
        };
        let (s, oci) = one_off_spec(&def, "web", "nightly", Duration::from_secs(60)).unwrap();
        assert!(oci);
        assert!(s.name.as_deref().unwrap().starts_with("job-nightly-"));
        assert_eq!(s.image, "docker:traefik/whoami@sha256:abc");
        assert_eq!(s.env["A"], "1");
        assert_eq!(s.env.secrets["T"], "web.tok");
        assert!(s.ports.is_empty() && s.volumes.is_empty() && s.healthcheck.is_none());
        assert_eq!(s.replicas(), 1);
        assert_eq!(s.memory.as_deref(), Some("256m"));
        assert_eq!(s.command.as_deref().unwrap()[0], "sleep");
        assert_eq!(s.labels["isb.job"], "nightly");
        assert!(!s.labels.contains_key("isb.stack"));
        assert!(one_off_spec(&def, "nope", "j", Duration::from_secs(1)).is_err());
    }
}
