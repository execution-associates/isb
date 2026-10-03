//! The `build_*` and `registry_*` tools: builds run in the daemon (it alone
//! pushes to the local registry), and their logs are read back in pieces,
//! so a CLI or an agent follows a build that outlives any one request.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::build::{BuildRequest, Builder, BuiltImage};
use crate::client::Client;
use crate::error::{Error, Result};
use crate::org::OrgId;
use crate::server::{Caller, Registry, Tool};
use crate::stack::Controller;

use super::policy::RemotePolicy;

/// Lines kept per build; older ones are dropped (and counted).
const MAX_LINES: usize = 20_000;
/// Finished builds kept for their logs.
const KEEP_FINISHED: usize = 50;

struct Job {
    org: OrgId,
    app: String,
    tag: String,
    started: u64,
    state: Mutex<JobState>,
    changed: Condvar,
}

#[derive(Default)]
struct JobState {
    /// Line numbers start at `first`.
    first: usize,
    lines: VecDeque<String>,
    result: Option<std::result::Result<BuiltImage, String>>,
    finished: Option<u64>,
}

impl Job {
    fn push(&self, line: &str) {
        let mut s = self.state.lock().unwrap();
        s.lines.push_back(line.to_string());
        if s.lines.len() > MAX_LINES {
            s.lines.pop_front();
            s.first += 1;
        }
        self.changed.notify_all();
    }

    fn summary(&self, id: &str) -> Value {
        let s = self.state.lock().unwrap();
        let mut v = json!({
            "id": id, "org": self.org, "app": self.app, "tag": self.tag,
            "started_at": self.started, "finished_at": s.finished,
            "state": match &s.result { None => "running", Some(Ok(_)) => "succeeded", Some(Err(_)) => "failed" },
        });
        match &s.result {
            Some(Ok(b)) => {
                v["image"] = json!(b.image);
                v["digest"] = json!(b.digest);
            }
            Some(Err(e)) => v["error"] = json!(e),
            None => {}
        }
        v
    }
}

#[derive(Default)]
struct Jobs {
    by_id: Mutex<BTreeMap<String, Arc<Job>>>,
}

impl Jobs {
    fn get(&self, id: &str, org: &OrgId) -> Result<Arc<Job>> {
        self.by_id
            .lock()
            .unwrap()
            .get(id)
            .filter(|j| j.org == *org)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("build {id} in org {org}")))
    }

    fn add(&self, id: String, j: Arc<Job>) {
        let mut m = self.by_id.lock().unwrap();
        m.insert(id, j);
        // Drop the oldest finished builds beyond the limit.
        let finished: Vec<String> = m
            .iter()
            .filter(|(_, j)| j.state.lock().unwrap().result.is_some())
            .map(|(k, _)| k.clone())
            .collect();
        if finished.len() > KEEP_FINISHED {
            for k in finished.iter().take(finished.len() - KEEP_FINISHED) {
                m.remove(k);
            }
        }
    }
}

/// What the build tools need from the daemon.
pub struct Ctx {
    pub client: Client,
    pub policy: RemotePolicy,
    pub ctl: Controller,
}

fn args<T: serde::de::DeserializeOwned>(v: Value) -> Result<T> {
    serde_json::from_value(v).map_err(|e| Error::invalid(format!("bad arguments: {e}")))
}

fn org_of(o: &Option<String>) -> Result<OrgId> {
    match o {
        Some(o) => OrgId::new(o.clone()),
        None => Ok(OrgId::default_org()),
    }
}

fn obj(mut props: Value, required: &[&str]) -> Value {
    props["org"] =
        json!({"type": "string", "description": "The org to act in (default: default)."});
    json!({"type": "object", "properties": props, "required": required, "additionalProperties": false})
}

/// A build id: time-ordered and unguessable enough for a log handle (the
/// org check is what guards it).
fn new_build_id() -> String {
    format!(
        "b{}-{}{}",
        crate::stack::now_secs(),
        crate::stack::new_id(),
        crate::stack::new_id()
    )
}

/// The build tools and the registry tools.
pub fn register(r: &mut Registry, ctx: Ctx) -> Result<()> {
    let ctx = Arc::new(ctx);
    let jobs = Arc::new(Jobs::default());
    let ro = json!({"readOnlyHint": true, "openWorldHint": false});
    let write = json!({"destructiveHint": false, "openWorldHint": true});
    let destructive = json!({"destructiveHint": true, "openWorldHint": false});

    let (c2, j2) = (ctx.clone(), jobs.clone());
    r.register(
        Tool::new(
            "build_run",
            "Start a build of a source directory into an image in the org's local registry. It runs in a fresh sandbox in the org (a VM with untrusted=true), never on the host, and is pushed as <org>/<app>:<tag>. Returns an id at once: follow it with build_logs. The result's `image` (registry:APP:TAG@DIGEST) goes in a compose `image:`.",
            obj(
                json!({
                    "app": {"type": "string", "description": "The app: names the repository (<org>/<app>) and the build cache. [a-z0-9][a-z0-9._-]*, up to 40 characters."},
                    "context": {"type": "string", "description": "Absolute path of the source directory on the host (read, never written). Remote callers: under a --bind-root."},
                    "subdir": {"type": "string", "description": "Build from this subdirectory of the context."},
                    "builder": {"type": "string", "enum": ["railpack", "nixpacks", "dockerfile"], "description": "How to build (default railpack, or dockerfile when `dockerfile` is set)."},
                    "dockerfile": {"type": "string", "description": "Dockerfile path relative to the context (default Dockerfile)."},
                    "target": {"type": "string", "description": "Dockerfile stage to build."},
                    "args": {"type": "object", "additionalProperties": {"type": "string"}, "description": "Build arguments (Dockerfile ARGs; environment for railpack and nixpacks)."},
                    "tag": {"type": "string", "description": "The tag to push (default latest)."},
                    "untrusted": {"type": "boolean", "description": "Build in a VM (its own kernel)."},
                    "timeout": {"type": "string", "description": "Longest the build may take, e.g. 30m (the default)."}
                }),
                &["app", "context"],
            ),
            move |a, c| build_run(&c2, &j2, a, c),
        )
        .title("Start a build")
        .annotations(write.clone()),
    )?;

    let j2 = jobs.clone();
    r.register(
        Tool::new(
            "build_logs",
            "A build's state and its log lines from `since` (a line number; 0 for all). `wait` (seconds, at most 30) holds the call until there are new lines or the build ends. Done when `state` is succeeded (then `image` and `digest`) or failed (`error`).",
            obj(
                json!({
                    "id": {"type": "string"},
                    "since": {"type": "integer", "minimum": 0},
                    "wait": {"type": "integer", "minimum": 0, "maximum": 30}
                }),
                &["id"],
            ),
            move |a, _c| {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct A {
                    #[serde(default)]
                    org: Option<String>,
                    id: String,
                    #[serde(default)]
                    since: usize,
                    #[serde(default)]
                    wait: u64,
                }
                let a: A = args(a)?;
                let org = org_of(&a.org)?;
                let job = j2.get(&a.id, &org)?;
                let until = Instant::now() + Duration::from_secs(a.wait.min(30));
                let mut s = job.state.lock().unwrap();
                while s.result.is_none() && s.first + s.lines.len() <= a.since {
                    let left = until.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        break;
                    }
                    s = job.changed.wait_timeout(s, left).unwrap().0;
                }
                let from = a.since.max(s.first);
                let lines: Vec<&String> = s.lines.iter().skip(from - s.first).collect();
                let next = s.first + s.lines.len();
                let dropped = a.since < s.first;
                let lines = json!(lines);
                drop(s);
                let mut v = job.summary(&a.id);
                v["lines"] = lines;
                v["next"] = json!(next);
                if dropped {
                    v["truncated"] = json!(true);
                }
                Ok(v)
            },
        )
        .title("Follow a build")
        .annotations(ro.clone()),
    )?;

    let j2 = jobs.clone();
    r.register(
        Tool::new(
            "build_list",
            "The org's recent builds (running and finished), newest first.",
            obj(json!({}), &[]),
            move |a, _c| {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct A {
                    #[serde(default)]
                    org: Option<String>,
                }
                let a: A = args(a)?;
                let org = org_of(&a.org)?;
                let m = j2.by_id.lock().unwrap();
                let mut out: Vec<Value> = m
                    .iter()
                    .filter(|(_, j)| j.org == org)
                    .map(|(id, j)| j.summary(id))
                    .collect();
                out.reverse();
                Ok(json!({"builds": out}))
            },
        )
        .title("List builds")
        .annotations(ro.clone()),
    )?;

    let c2 = ctx.clone();
    r.register(
        Tool::new(
            "registry_list",
            "The org's images in the local registry: each app's tags with their digests and push times, newest first. Run one with image: registry:APP:TAG (or @DIGEST) in a compose file.",
            obj(json!({}), &[]),
            move |a, _c| {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct A {
                    #[serde(default)]
                    org: Option<String>,
                }
                let a: A = args(a)?;
                let org = org_of(&a.org)?;
                let reg = crate::registry::Registry::shared(&c2.client)?;
                Ok(json!({
                    "registry": crate::registry::status_json(&reg),
                    "repositories": reg.list(Some(&org))?,
                }))
            },
        )
        .title("List images")
        .annotations(ro.clone()),
    )?;

    let c2 = ctx.clone();
    r.register(
        Tool::new(
            "registry_gc",
            "Retention for the whole local registry (platform admins): per repository keep the newest `keep` tags (default 10) and every image a deployed stack runs or would roll back to, delete the rest, and reclaim their storage. dry_run reports only.",
            obj(
                json!({
                    "keep": {"type": "integer", "minimum": 0},
                    "dry_run": {"type": "boolean"}
                }),
                &[],
            ),
            move |a, _c| {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct A {
                    #[serde(default)]
                    #[allow(dead_code)]
                    org: Option<String>,
                    keep: Option<usize>,
                    #[serde(default)]
                    dry_run: bool,
                }
                let a: A = args(a)?;
                let reg = crate::registry::Registry::shared(&c2.client)?;
                let protected = crate::registry::protected_by(&c2.ctl.definitions());
                let mut lines = Vec::new();
                let rep = reg.gc(
                    a.keep.unwrap_or(crate::registry::DEFAULT_KEEP),
                    &protected,
                    a.dry_run,
                    &mut |l| lines.push(l.to_string()),
                )?;
                let mut v = serde_json::to_value(rep)?;
                v["log"] = json!(lines);
                Ok(v)
            },
        )
        .title("Registry retention")
        .annotations(destructive),
    )?;
    Ok(())
}

fn build_run(ctx: &Ctx, jobs: &Arc<Jobs>, a: Value, c: &Caller) -> Result<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        #[serde(default)]
        org: Option<String>,
        app: String,
        context: PathBuf,
        #[serde(default)]
        subdir: Option<String>,
        #[serde(default)]
        builder: Option<String>,
        #[serde(default)]
        dockerfile: Option<String>,
        #[serde(default)]
        target: Option<String>,
        #[serde(default)]
        args: BTreeMap<String, String>,
        #[serde(default)]
        tag: Option<String>,
        #[serde(default)]
        untrusted: bool,
        #[serde(default)]
        timeout: Option<String>,
    }
    let a: A = args(a)?;
    let org = org_of(&a.org)?;
    if !a.context.is_absolute() {
        return Err(Error::invalid("context must be an absolute path"));
    }
    if !c.is_trusted() {
        ctx.policy.check_base_dir(&a.context)?;
    }
    let builder = match (a.builder.as_deref(), &a.dockerfile) {
        (None, Some(_)) | (Some("dockerfile"), _) => Builder::Dockerfile {
            path: a.dockerfile.clone().unwrap_or_else(|| "Dockerfile".into()),
            target: a.target.clone(),
        },
        (None, None) | (Some("railpack"), None) => Builder::Railpack,
        (Some("nixpacks"), None) => Builder::Nixpacks,
        (Some("buildpacks"), None) => Builder::Buildpacks { builder: None },
        (Some(b), _) => {
            return Err(Error::invalid(format!(
                "builder {b:?}: railpack, nixpacks or dockerfile (a dockerfile path needs the dockerfile builder)"
            )));
        }
    };
    let req = BuildRequest {
        org: org.clone(),
        app: a.app.clone(),
        context: a.context.clone(),
        subdir: a.subdir.clone(),
        builder,
        args: a.args.into_iter().collect(),
        tag: a.tag.clone().unwrap_or_else(|| "latest".into()),
        untrusted: a.untrusted,
        cache: None,
    };
    let mut opts = crate::build::BuildOptions::default();
    if let Some(t) = &a.timeout {
        opts.timeout = crate::flex::parse_duration(t).map_err(Error::invalid)?;
    }
    // Refuse what cannot start before answering with an id.
    crate::registry::Registry::shared(&ctx.client)?;
    let id = new_build_id();
    let job = Arc::new(Job {
        org: org.clone(),
        app: req.app.clone(),
        tag: req.tag.clone(),
        started: crate::stack::now_secs(),
        state: Mutex::new(JobState::default()),
        changed: Condvar::new(),
    });
    jobs.add(id.clone(), job.clone());
    let client = ctx.client.clone();
    let ctl = ctx.ctl.clone();
    let who = c.to_string();
    let stack = crate::stack::qualified(&org, &format!("build:{}", req.app));
    ctl.note(
        "info",
        &stack,
        format!("build {id} of {}:{} started by {who}", req.app, req.tag),
    );
    std::thread::Builder::new()
        .name(format!("isb-build-{id}"))
        .spawn(move || {
            let r = crate::build::run_with(&client, &req, &opts, &mut |l| job.push(l));
            let (level, msg) = match &r {
                Ok(b) => (
                    "info",
                    format!("build of {}:{} succeeded: {}", req.app, req.tag, b.image),
                ),
                Err(e) => (
                    "error",
                    format!("build of {}:{} failed: {e}", req.app, req.tag),
                ),
            };
            if let Err(e) = &r {
                job.push(&format!("error: {e}"));
            }
            ctl.note(level, &stack, msg);
            let mut s = job.state.lock().unwrap();
            s.result = Some(r.map_err(|e| e.to_string()));
            s.finished = Some(crate::stack::now_secs());
            job.changed.notify_all();
        })
        .map_err(Error::Io)?;
    Ok(json!({"id": id, "org": org, "app": a.app, "tag": a.tag.unwrap_or_else(|| "latest".into())}))
}
