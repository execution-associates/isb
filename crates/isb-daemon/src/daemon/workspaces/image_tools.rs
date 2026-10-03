//! The `workspace_image_*` tools: build a workspace image from a recipe
//! script (crate::build::workspace_image), follow its log, list the images
//! isb built on this host, and remove one. Images are the host's, shared by
//! every org on it, so these are for platform admins and superadmins.

use std::collections::VecDeque;
use std::sync::Condvar;
use std::time::Instant;

use super::*;
use crate::build::workspace_image::{self as wi, ImageBuild};

/// Lines kept per build, and finished builds kept for their logs.
const MAX_LINES: usize = 20_000;
const KEEP_FINISHED: usize = 20;

struct Job {
    name: String,
    started: u64,
    by: String,
    state: Mutex<JobState>,
    changed: Condvar,
}

#[derive(Default)]
struct JobState {
    first: usize,
    lines: VecDeque<String>,
    result: Option<std::result::Result<wi::Built, String>>,
    finished: Option<u64>,
}

impl Job {
    fn push(&self, line: &str) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.lines.push_back(line.to_string());
        if s.lines.len() > MAX_LINES {
            s.lines.pop_front();
            s.first += 1;
        }
        self.changed.notify_all();
    }

    fn summary(&self, id: &str) -> Value {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut v = json!({
            "id": id, "name": self.name, "started_at": self.started, "by": self.by,
            "finished_at": s.finished,
            "state": match &s.result { None => "running", Some(Ok(_)) => "succeeded", Some(Err(_)) => "failed" },
        });
        match &s.result {
            Some(Ok(b)) => v["image"] = json!(b),
            Some(Err(e)) => v["error"] = json!(e),
            None => {}
        }
        v
    }
}

#[derive(Default)]
struct Jobs {
    by_id: Mutex<std::collections::BTreeMap<String, Arc<Job>>>,
}

impl Jobs {
    fn get(&self, id: &str) -> Result<Arc<Job>> {
        self.by_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("image build {id}")))
    }

    fn add(&self, id: String, j: Arc<Job>) {
        let mut m = self.by_id.lock().unwrap_or_else(|e| e.into_inner());
        m.insert(id, j);
        let finished: Vec<String> = m
            .iter()
            .filter(|(_, j)| {
                j.state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .result
                    .is_some()
            })
            .map(|(k, _)| k.clone())
            .collect();
        for k in finished
            .iter()
            .take(finished.len().saturating_sub(KEEP_FINISHED))
        {
            m.remove(k);
        }
    }

    fn running(&self) -> Vec<Value> {
        let m = self.by_id.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<Value> = m.iter().map(|(id, j)| j.summary(id)).collect();
        out.reverse();
        out
    }
}

/// Images are the host's: platform admins and superadmins only.
fn platform(c: &Caller, what: &str) -> Result<()> {
    let ok = match c {
        Caller::Local { .. } | Caller::Superadmin(_) => true,
        Caller::User { principal } => principal.platform_admin && !principal.is_workspace(),
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(Error::Forbidden(format!(
            "{what} is for platform admins (images are shared by every org on the host)"
        )))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildArgs {
    #[serde(default)]
    #[allow(dead_code)]
    org: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    recipe: Option<String>,
    #[serde(default)]
    base: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    timeout: Option<String>,
    #[serde(default)]
    force: bool,
}

/// The build a call asks for: the default recipe unless it gives one.
fn request(a: BuildArgs, by: String) -> Result<ImageBuild> {
    let mut b = ImageBuild::default_recipe(&by);
    if let Some(n) = a.name.filter(|n| !n.trim().is_empty()) {
        b.name = n.trim().to_string();
    }
    if let Some(r) = a.recipe {
        b.recipe = r;
    }
    if let Some(base) = a.base.filter(|x| !x.trim().is_empty()) {
        b.base = base.trim().to_string();
    }
    b.description = a.description;
    if let Some(t) = &a.timeout {
        b.timeout = crate::flex::parse_duration(t).map_err(Error::invalid)?;
    }
    b.force = a.force;
    wi::check(&b)?;
    Ok(b)
}

fn build(d: &Daemon, jobs: &Arc<Jobs>, a: Value, c: &Caller) -> Result<Value> {
    platform(c, "building workspace images")?;
    let b = request(args(a)?, c.to_string())?;
    let id = format!(
        "wi{}-{}{}",
        now(),
        crate::stack::new_id(),
        crate::stack::new_id()
    );
    let job = Arc::new(Job {
        name: b.name.clone(),
        started: now(),
        by: c.to_string(),
        state: Mutex::new(JobState::default()),
        changed: Condvar::new(),
    });
    jobs.add(id.clone(), job.clone());
    let client = d.client.clone();
    let recorder = d.workspaces.recorder.clone();
    let (name, base) = (b.name.clone(), b.base.clone());
    std::thread::Builder::new()
        .name(format!("isb-wsimage-{}", b.name))
        .spawn(move || {
            let r = wi::build(&client, &b, &mut |l| job.push(l));
            let (level, msg) = match &r {
                Ok(x) if x.up_to_date => ("info", format!("workspace image {} is up to date", x.name)),
                Ok(x) => ("info", format!("workspace image {} built in {}s", x.name, x.seconds)),
                Err(e) => ("error", format!("workspace image {} not built: {e}", b.name)),
            };
            if let Err(e) = &r {
                job.push(&format!("error: {e}"));
            }
            eprintln!("isb serve: {msg}");
            recorder.record(crate::history::NewRecord {
                source: "controller".into(),
                kind: "workspace_image.build".into(),
                object_type: Some("image".into()),
                object: Some(b.name.clone()),
                objects: vec![b.name.clone()],
                actor: Some(b.built_by.clone()),
                level: Some(level.into()),
                message: Some(msg),
                details: json!({"base": b.base, "recipe_sha256": b.recipe_sha256(), "result": r.as_ref().ok()}),
                ..Default::default()
            });
            let mut s = job.state.lock().unwrap_or_else(|e| e.into_inner());
            s.result = Some(r.map_err(|e| e.to_string()));
            s.finished = Some(now());
            job.changed.notify_all();
        })
        .map_err(Error::Io)?;
    Ok(
        json!({"id": id, "name": name, "base": base, "message": "Building: follow it with workspace_image_logs."}),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LogArgs {
    #[serde(default)]
    #[allow(dead_code)]
    org: Option<String>,
    id: String,
    #[serde(default)]
    since: usize,
    #[serde(default)]
    wait: u64,
}

fn logs(jobs: &Arc<Jobs>, a: Value, c: &Caller) -> Result<Value> {
    platform(c, "workspace image builds")?;
    let a: LogArgs = args(a)?;
    let job = jobs.get(&a.id)?;
    let until = Instant::now() + Duration::from_secs(a.wait.min(30));
    let mut s = job.state.lock().unwrap_or_else(|e| e.into_inner());
    while s.result.is_none() && s.first + s.lines.len() <= a.since {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        s = job
            .changed
            .wait_timeout(s, left)
            .unwrap_or_else(|e| e.into_inner())
            .0;
    }
    let from = a.since.max(s.first);
    let lines: Vec<&String> = s.lines.iter().skip(from - s.first).collect();
    let next = s.first + s.lines.len();
    let truncated = a.since < s.first;
    let lines = json!(lines);
    drop(s);
    let mut v = job.summary(&a.id);
    v["lines"] = lines;
    v["next"] = json!(next);
    if truncated {
        v["truncated"] = json!(true);
    }
    Ok(v)
}

pub(super) fn register(r: &mut Registry, d: Arc<Daemon>) -> Result<()> {
    let jobs = Arc::new(Jobs::default());
    let ro = json!({"readOnlyHint": true, "openWorldHint": false});
    let write = json!({"destructiveHint": false, "openWorldHint": true});
    let destructive = json!({"destructiveHint": true, "openWorldHint": false});
    let (d2, j2) = (d.clone(), jobs.clone());
    r.register(
        Tool::new(
            "workspace_image_build",
            "Build a workspace image from a recipe script (platform admins): launch a temporary container from `base` in isb's system project, run the recipe in it as root, stop it, publish it as the local image `name`, and delete the container. Without `recipe`, isb's default recipe (Ubuntu 24.04, dev at uid 1000, mise, Claude Code, Codex, herdr) as isb-workspace. Returns an id at once: follow it with workspace_image_logs. A failure publishes nothing; the same recipe and base again is a no-op unless force. Only images isb built can be replaced.",
            obj(
                json!({
                    "name": {"type": "string", "description": "The local image alias (default isb-workspace)."},
                    "recipe": {"type": "string", "description": "The recipe: a shell script run as root (a #! line picks its interpreter). Default: isb's own."},
                    "base": {"type": "string", "description": "The image it starts from (default images:ubuntu/24.04)."},
                    "description": {"type": "string", "description": "The image's description, shown in the workspace create form."},
                    "timeout": {"type": "string", "description": "Longest the recipe may run, e.g. 45m (default 30m, at most 2h)."},
                    "force": {"type": "boolean", "description": "Build even when the image is up to date."}
                }),
                &[],
            ),
            move |a, c| build(&d2, &j2, a, c),
        )
        .title("Build a workspace image")
        .annotations(write),
    )?;
    let j2 = jobs.clone();
    r.register(
        Tool::new(
            "workspace_image_logs",
            "A workspace image build's state and its log lines from `since` (0 for all); `wait` (seconds, at most 30) holds the call until there are new lines or the build ends. Done when `state` is succeeded (`image`) or failed (`error`).",
            obj(
                json!({
                    "id": {"type": "string"},
                    "since": {"type": "integer", "minimum": 0},
                    "wait": {"type": "integer", "minimum": 0, "maximum": 30}
                }),
                &["id"],
            ),
            move |a, c| logs(&j2, a, c),
        )
        .title("Follow a workspace image build")
        .annotations(ro.clone()),
    )?;
    let (d2, j2) = (d.clone(), jobs.clone());
    r.register(
        Tool::new(
            "workspace_image_list",
            "The workspace images isb built on this host (alias, description, size, base, recipe hash, who built it and when), the builds running or recently finished, and whether the default recipe's image (isb-workspace) exists and is current. Platform admins.",
            obj(json!({}), &[]),
            move |_a, c| {
                platform(c, "workspace images")?;
                let images = wi::list(&d2.client)?;
                let default = images
                    .iter()
                    .find(|i| i["aliases"].as_array().is_some_and(|a| a.iter().any(|x| x == wi::DEFAULT_NAME)));
                Ok(json!({
                    "images": images,
                    "builds": j2.running(),
                    "default": {
                        "name": wi::DEFAULT_NAME,
                        "exists": default.is_some(),
                        "current": default.is_some_and(|i| i["default_recipe"] == true),
                        "recipe_sha256": wi::recipe_sha256(wi::DEFAULT_RECIPE),
                    },
                }))
            },
        )
        .title("List workspace images")
        .annotations(ro),
    )?;
    let d2 = d.clone();
    r.register(
        Tool::new(
            "workspace_image_remove",
            "Remove a workspace image isb built (platform admins), by its alias: the alias, and the image when nothing else names it. Images isb did not build (dev-base, any other) are refused. Workspaces already made from it keep running; rebuilding one needs another image.",
            obj(json!({"name": {"type": "string"}}), &["name"]),
            move |a, c| {
                platform(c, "removing workspace images")?;
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct A {
                    #[serde(default)]
                    #[allow(dead_code)]
                    org: Option<String>,
                    name: String,
                }
                let a: A = args(a)?;
                let e = wi::remove(&d2.client, &a.name)?;
                eprintln!("isb serve: workspace image {} removed by {c}", a.name);
                Ok(json!({"ok": true, "name": a.name, "fingerprint": e.fingerprint, "image_deleted": e.aliases.iter().all(|x| *x == a.name)}))
            },
        )
        .title("Remove a workspace image")
        .annotations(destructive),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: Value) -> BuildArgs {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn a_call_without_a_recipe_builds_the_default_one() {
        let b = request(a(json!({})), "admin@x.io".into()).unwrap();
        assert_eq!(b.name, wi::DEFAULT_NAME);
        assert_eq!(b.recipe, wi::DEFAULT_RECIPE);
        assert_eq!(b.base, wi::DEFAULT_BASE);
        let b = request(
            a(json!({"name": "team", "recipe": "echo hi", "base": "images:debian/12", "timeout": "45m", "force": true})),
            "x".into(),
        )
        .unwrap();
        assert_eq!(
            (b.name.as_str(), b.recipe.as_str(), b.base.as_str()),
            ("team", "echo hi", "images:debian/12")
        );
        assert_eq!(b.timeout, Duration::from_secs(45 * 60));
        assert!(b.force);
        assert!(request(a(json!({"name": "Team"})), "x".into()).is_err());
        assert!(request(a(json!({"timeout": "3h"})), "x".into()).is_err());
    }

    #[test]
    fn only_platform_admins_build_or_remove_images() {
        assert!(platform(&Caller::Local { uid: None }, "x").is_ok());
        let anon = Caller::Unauthenticated {
            addr: "127.0.0.1:9".parse().unwrap(),
        };
        let e = platform(&anon, "building").unwrap_err();
        assert!(e.to_string().contains("platform admins"), "{e}");
    }
}
