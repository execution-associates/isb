//! `app_exec` and `instance_exec`: run argv in a replica or an instance, with
//! the output capped and the time bounded.

use super::*;
use crate::exec::ExecEvent;
use crate::stack::controller::InstanceStatus;

/// Each stream of an exec keeps at most this much (the end of it).
pub(super) const EXEC_OUTPUT_CAP: usize = 1024 * 1024;
/// The stdin an exec may be given.
pub(super) const EXEC_STDIN_CAP: usize = 1024 * 1024;
/// Default and largest exec timeouts.
pub(super) const EXEC_TIMEOUT_DEFAULT: Duration = Duration::from_secs(60);
pub(super) const EXEC_TIMEOUT_MAX: Duration = Duration::from_secs(15 * 60);

/// The last `cap` bytes of a stream, and how much went by.
pub(super) struct Tail {
    pub(super) buf: Vec<u8>,
    cap: usize,
    total: u64,
}

impl Tail {
    pub(super) fn new(cap: usize) -> Tail {
        Tail {
            buf: Vec::new(),
            cap,
            total: 0,
        }
    }

    pub(super) fn push(&mut self, b: &[u8]) {
        self.total += b.len() as u64;
        self.buf.extend_from_slice(b);
        // Trim in batches, not on every chunk.
        if self.buf.len() > self.cap.saturating_mul(2) {
            let cut = self.buf.len() - self.cap;
            self.buf.drain(..cut);
        }
    }

    /// The kept bytes, whether any were dropped, and the stream's length.
    pub(super) fn finish(mut self) -> (Vec<u8>, bool, u64) {
        if self.buf.len() > self.cap {
            let cut = self.buf.len() - self.cap;
            self.buf.drain(..cut);
        }
        let truncated = self.total > self.buf.len() as u64;
        (self.buf, truncated, self.total)
    }
}

/// An exec's timeout: 60s unless given, at most 15 minutes.
pub(super) fn exec_timeout(t: Option<&str>) -> Result<Duration> {
    let Some(t) = t else {
        return Ok(EXEC_TIMEOUT_DEFAULT);
    };
    let d = crate::flex::parse_duration(t).map_err(Error::invalid)?;
    if d.is_zero() || d > EXEC_TIMEOUT_MAX {
        return Err(Error::invalid(format!(
            "timeout must be between 1s and {}m (it was {t})",
            EXEC_TIMEOUT_MAX.as_secs() / 60
        )));
    }
    Ok(d)
}

/// What an exec call asks for, checked.
pub(super) struct Run {
    pub(super) argv: Vec<String>,
    pub(super) cwd: Option<String>,
    pub(super) user: Option<String>,
    pub(super) env: BTreeMap<String, String>,
    pub(super) stdin: Option<String>,
    pub(super) timeout: Duration,
}

impl Run {
    pub(super) fn new(
        argv: Vec<String>,
        cwd: Option<String>,
        user: Option<String>,
        env: BTreeMap<String, String>,
        stdin: Option<String>,
        timeout: Option<&str>,
    ) -> Result<Run> {
        if argv.is_empty() || argv[0].is_empty() {
            return Err(Error::invalid(
                "argv is the command and its arguments, e.g. [\"ls\", \"-l\"]; use [\"sh\", \"-c\", \"...\"] for a shell line",
            ));
        }
        if argv.len() > 1024 || argv.iter().any(|a| a.contains('\0')) {
            return Err(Error::invalid(
                "argv: at most 1024 arguments, none with a NUL",
            ));
        }
        if env.len() > 128 || env.keys().any(|k| k.is_empty() || k.contains('=')) {
            return Err(Error::invalid(
                "env: at most 128 variables, with names that are not empty or contain =",
            ));
        }
        if stdin.as_ref().is_some_and(|s| s.len() > EXEC_STDIN_CAP) {
            return Err(Error::invalid(format!(
                "stdin is at most {} KiB; write a file with instance_file_write and read it from the command",
                EXEC_STDIN_CAP / 1024
            )));
        }
        Ok(Run {
            argv,
            cwd,
            user,
            env,
            stdin,
            timeout: exec_timeout(timeout)?,
        })
    }
}

/// Which replica of an app a call means: the one at `slot`, the one named
/// `name`, else a running one, preferring those in rotation and healthy.
pub(super) fn pick_replica<'a>(
    app: &str,
    replicas: &'a [InstanceStatus],
    slot: Option<u32>,
    name: Option<&str>,
) -> Result<&'a InstanceStatus> {
    let list = || {
        replicas
            .iter()
            .map(|i| {
                format!(
                    "{} ({}, {})",
                    i.slot,
                    i.status.to_lowercase(),
                    if i.in_rotation {
                        "in rotation"
                    } else {
                        "out of rotation"
                    }
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let chosen = match (slot, name) {
        (Some(_), Some(_)) => {
            return Err(Error::invalid("give replica or instance, not both"));
        }
        (Some(n), None) => Some(replicas.iter().find(|i| i.slot == n).ok_or_else(|| {
            Error::NotFound(format!("replica {n} of {app} (it has: {})", list()))
        })?),
        (None, Some(n)) => Some(
            replicas
                .iter()
                .find(|i| i.name == n)
                .ok_or_else(|| Error::NotFound(format!("replica {n} of {app}")))?,
        ),
        (None, None) => None,
    };
    match chosen {
        Some(i) if i.status != "Running" => Err(Error::invalid(format!(
            "replica {} is {}, not running",
            i.slot,
            i.status.to_lowercase()
        ))),
        Some(i) => Ok(i),
        None => replicas
            .iter()
            .filter(|i| i.status == "Running")
            .min_by_key(|i| (!i.in_rotation, i.health != "healthy", i.slot))
            .ok_or_else(|| {
                Error::invalid(format!(
                    "{app} has no running replica{}",
                    if replicas.is_empty() {
                        String::new()
                    } else {
                        format!(" (replicas: {})", list())
                    }
                ))
            }),
    }
}

fn running_info(d: &Daemon, c: &Caller, oc: &Client, name: &str) -> Result<SandboxInfo> {
    let info = d.reach(c, oc, name)?;
    if info.status != "Running" {
        return Err(Error::invalid(format!(
            "{name} is {}, not running",
            info.status.to_lowercase()
        )));
    }
    Ok(info)
}

/// Run `r` in `instance` and capture its output, each stream capped.
pub(super) fn run_capped(oc: &Client, instance: &str, r: Run, cap: usize) -> Result<Value> {
    let started = Instant::now();
    let sb = Sandbox::get(oc, instance)?;
    let mut opts = ExecOptions::default().timeout(r.timeout);
    opts.cwd = r.cwd;
    opts.user = r.user;
    opts.env = r.env;
    if let Some(s) = r.stdin {
        opts.stdin = Stdin::Bytes(s.into_bytes());
    }
    let mut stream = sb
        .exec_stream(r.argv, opts)
        .map_err(|e| Error::invalid(format!("cannot run that in {instance}: {e}")))?;
    let (mut out, mut err) = (Tail::new(cap), Tail::new(cap));
    let code = loop {
        match stream.poll_event(Duration::from_millis(500)) {
            Ok(Some(ExecEvent::Stdout(b))) => out.push(&b),
            Ok(Some(ExecEvent::Stderr(b))) => err.push(&b),
            Ok(None) => {}
            Err(code) => break code,
        }
    };
    let (exit_code, timed_out) = match code {
        Ok(c) => (Some(c), false),
        Err(Error::ExecTimeout { .. }) => (None, true),
        Err(e) => return Err(e),
    };
    let (stdout, t1, n1) = out.finish();
    let (stderr, t2, n2) = err.finish();
    Ok(json!({
        "instance": instance,
        "exit_code": exit_code,
        "timed_out": timed_out,
        "stdout": String::from_utf8_lossy(&stdout),
        "stderr": String::from_utf8_lossy(&stderr),
        "stdout_bytes": n1,
        "stderr_bytes": n2,
        "stdout_truncated": t1,
        "stderr_truncated": t2,
        "truncated": t1 || t2,
        "duration_ms": started.elapsed().as_millis() as u64,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecArgs {
    name: String,
    #[serde(default)]
    #[allow(dead_code)]
    org: Option<String>,
    argv: Vec<String>,
    #[serde(default)]
    replica: Option<u32>,
    #[serde(default)]
    instance: Option<String>,
    cwd: Option<String>,
    user: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    stdin: Option<String>,
    timeout: Option<String>,
}

fn app_exec(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    let org = arg_org(&a)?;
    let a: ExecArgs = args(a)?;
    let run = Run::new(a.argv, a.cwd, a.user, a.env, a.stdin, a.timeout.as_deref())?;
    let (_, _, svc) = app_service(d, &org, &a.name)?;
    let inst = pick_replica(&a.name, &svc.instances, a.replica, a.instance.as_deref())?;
    let oc = crate::org::client(&d.client, &org);
    let mut out = run_capped(&oc, &inst.name, run, EXEC_OUTPUT_CAP)?;
    out["app"] = json!(a.name);
    out["replica"] = json!(inst.slot);
    let _ = c;
    Ok(out)
}

fn instance_exec(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    let org = arg_org(&a)?;
    let a: ExecArgs = args(a)?;
    if a.replica.is_some() || a.instance.is_some() {
        return Err(Error::invalid(
            "instance_exec runs in the instance `name`; app_exec takes replica",
        ));
    }
    let mut run = Run::new(a.argv, a.cwd, a.user, a.env, a.stdin, a.timeout.as_deref())?;
    let oc = d.oc(&Some(org.to_string()))?;
    let info = running_info(d, c, &oc, &a.name)?;
    d.workspaces.mark_active(&org.incus_project(), &a.name);
    // A workspace's commands run as its user, in its home, as its terminal does.
    if let Some(w) = info
        .config
        .get(crate::workspace::KEY_WORKSPACE)
        .and_then(|w| d.workspaces_def(&org, w).ok())
    {
        if run.user.is_none() {
            run.user = Some(w.user.clone());
            run.cwd.get_or_insert_with(|| w.home_dir());
        }
    }
    run_capped(&oc, &a.name, run, EXEC_OUTPUT_CAP)
}

/// Register the exec tools.
pub(super) fn register(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    let exec_props = |name_doc: &str| {
        json!({
            "name": {"type": "string", "description": name_doc},
            "argv": {"type": "array", "items": {"type": "string"}, "minItems": 1, "description": "The command and its arguments; no shell unless you run one: [\"sh\", \"-c\", \"...\"]. `command` is accepted as an alias (a string runs as `sh -c`)."},
            "cwd": {"type": "string"},
            "user": {"type": "string", "description": "A guest user name, uid or uid:gid (default: the instance's default, root)."},
            "env": {"type": "object", "additionalProperties": {"type": "string"}},
            "stdin": {"type": "string", "description": "Text fed to the command's stdin (at most 1 MiB)."},
            "timeout": {"type": "string", "description": "Kill the command after this long, e.g. 30s (default 60s, at most 15m). A command that runs out answers timed_out with the output so far."}
        })
    };
    let mut app_exec_props = exec_props("The app's name (`app` is accepted as an alias).");
    app_exec_props["replica"] = json!({"type": "integer", "minimum": 1, "description": "The replica's slot (default: a running one, preferring healthy replicas in rotation)."});
    app_exec_props["instance"] =
        json!({"type": "string", "description": "Or the replica's instance name."});
    tool!(
        r,
        d,
        "app_exec",
        "Run a command in an app",
        "Run argv in one of an app's replicas and return its exit code and output (kubectl exec, not interactive): by default a running replica, healthy and in rotation first; `replica` (slot) or `instance` choose one. stdout and stderr are each capped at 1 MiB (the end is kept; `truncated` says so). Images built FROM scratch have no shell or tools to run. Members and up.",
        obj(app_exec_props, &["name", "argv"]),
        annotations("app_exec", ann),
        app_exec
    );
    tool!(
        r,
        d,
        "instance_exec",
        "Run a command in an instance",
        "Run argv in any running instance of the org (a replica, a database, the workspace, a sandbox) and return its exit code and output: as sandbox_exec, with a 60s default timeout (at most 15m) and each stream capped at 1 MiB. A workspace's commands run as its user, in its home. Members and up.",
        obj(
            exec_props("The instance's name, from instance_list."),
            &["name", "argv"]
        ),
        annotations("instance_exec", ann),
        instance_exec
    );
    Ok(())
}
