//! `kubectl` for an isb org: the tools that look into and act on the
//! instances isb manages (docs/guides/kubectl.md).
//!
//! `instance_list` and `instance_get` are `get pods` and `describe pod`;
//! `app_exec` and `instance_exec` are `exec`; `app_logs` is `logs`;
//! `app_restart` and `instance_restart` are `rollout restart` and `delete pod`;
//! `app_scale`, `app_top` and `app_events` are `scale`, `top` and `get
//! events`; `instance_file_read` and `instance_file_write` are `cp`.
//!
//! They are ordinary registry tools, so the one authorizer judges them as it
//! does every other (members and up change things, viewers read, a token's
//! scopes narrow that), `--deny-tools` applies, the audit hook records them,
//! and a control plane forwards them to the server an org lives on.

use std::collections::BTreeSet;

use super::*;
use crate::audit::Visibility;
use crate::exec::ExecEvent;
use crate::org::OrgId;
use crate::stack::controller::{InstanceStatus, ServiceStatus};

/// Each stream of an exec keeps at most this much (the end of it).
pub(super) const EXEC_OUTPUT_CAP: usize = 1024 * 1024;
/// The stdin an exec may be given.
pub(super) const EXEC_STDIN_CAP: usize = 1024 * 1024;
/// Default and largest exec timeouts.
pub(super) const EXEC_TIMEOUT_DEFAULT: Duration = Duration::from_secs(60);
pub(super) const EXEC_TIMEOUT_MAX: Duration = Duration::from_secs(15 * 60);
/// The largest file `instance_file_read` returns and `instance_file_write` takes.
pub(super) const FILE_CAP: usize = 4 * 1024 * 1024;

/// Tools that only read.
pub(super) const READS: &[&str] = &[
    "instance_list",
    "instance_get",
    "app_logs",
    "app_top",
    "app_events",
];
/// Tools that read what may be secret: refused to viewers and to `read` and
/// `deploy` tokens, and always recorded.
pub(super) const SECRET_READS: &[&str] = &["instance_file_read"];
/// Tools that change things or run code.
pub(super) const WRITES: &[&str] = &[
    "app_exec",
    "instance_exec",
    "app_restart",
    "instance_restart",
    "app_scale",
    "instance_file_write",
];

/// Where isb keeps what it delivers into instances: the workspace token,
/// stack and app secrets, the supervised service's environment and the
/// egress CA. Writes there are refused.
const MANAGED_DIRS: &[&str] = &["/run/isb", "/run/secrets", "/etc/isb"];
/// Kernel and device trees: neither read nor written through the file tools.
const VIRTUAL_DIRS: &[&str] = &["/proc", "/sys", "/dev"];

/// How a tool is judged, as the registry annotates it.
pub(super) fn class_of(tool: &str) -> Option<audit::Class> {
    if SECRET_READS.contains(&tool) {
        Some(audit::Class {
            read_only: false,
            secret_read: true,
        })
    } else if READS.contains(&tool) {
        Some(audit::Class {
            read_only: true,
            secret_read: false,
        })
    } else if WRITES.contains(&tool) {
        Some(audit::Class::default())
    } else {
        None
    }
}

fn annotations(tool: &str, ann: &Ann) -> Value {
    match class_of(tool) {
        Some(c) if c.secret_read => {
            json!({"readOnlyHint": true, "isbSecretRead": true, "openWorldHint": false})
        }
        Some(c) if c.read_only => ann.ro.clone(),
        _ => ann.write.clone(),
    }
}

// --- rules the tests hold -------------------------------------------------

/// The last `cap` bytes of a stream, and how much went by.
pub(super) struct Tail {
    buf: Vec<u8>,
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
            Error::NotFound(format!("{app} has no replica {n} (replicas: {})", list()))
        })?),
        (None, Some(n)) => Some(
            replicas
                .iter()
                .find(|i| i.name == n)
                .ok_or_else(|| Error::NotFound(format!("{app} has no replica {n}")))?,
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
                Error::NotFound(format!(
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

/// An absolute path with `.` and empty parts resolved; `..` and NUL refused.
pub(super) fn clean_path(p: &str) -> Result<String> {
    if !p.starts_with('/') {
        return Err(Error::invalid(format!("path {p:?} must be absolute")));
    }
    if p.contains('\0') || p.len() > 4096 {
        return Err(Error::invalid("path: no NUL, at most 4096 characters"));
    }
    let mut parts = Vec::new();
    for part in p.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err(Error::invalid(format!("path {p:?} must not contain .."))),
            s => parts.push(s),
        }
    }
    if parts.is_empty() {
        return Err(Error::invalid("path names the root directory, not a file"));
    }
    Ok(format!("/{}", parts.join("/")))
}

fn under(path: &str, dir: &str) -> bool {
    path == dir || path.strip_prefix(dir).is_some_and(|r| r.starts_with('/'))
}

/// Why a file may not be read or written through the file tools, if so.
/// `managed` are the files isb itself delivers into this instance.
pub(super) fn refuse_path(path: &str, write: bool, managed: &[String]) -> Option<String> {
    if VIRTUAL_DIRS.iter().any(|d| under(path, d)) {
        return Some(format!(
            "{path} is a kernel or device file, not a regular one"
        ));
    }
    if path == crate::workspace::TOKEN_PATH {
        return Some(format!(
            "{path} is the workspace's token: isb delivers and rotates it (workspace_token_rotate)"
        ));
    }
    if write {
        if let Some(d) = MANAGED_DIRS.iter().find(|d| under(path, d)) {
            return Some(format!(
                "{d} is where isb delivers secrets and its own files; change them through secrets, apps or stacks"
            ));
        }
        if managed.iter().any(|m| m == path) {
            return Some(format!(
                "{path} is delivered by isb from an org secret; change the secret instead"
            ));
        }
    }
    None
}

/// An octal mode: `0644`, `"644"` or the number 420.
fn parse_mode(v: &Value) -> Result<u32> {
    let bad = || Error::invalid("mode is an octal string like \"0644\"");
    let m = match v {
        Value::String(s) => {
            u32::from_str_radix(s.trim().trim_start_matches("0o"), 8).map_err(|_| bad())?
        }
        // JSON has no octal: 644 means 0644, as it does in YAML.
        Value::Number(n) => {
            let n = n.as_u64().ok_or_else(bad)?;
            u32::from_str_radix(&n.to_string(), 8).map_err(|_| bad())?
        }
        _ => return Err(bad()),
    };
    if m > 0o7777 {
        return Err(bad());
    }
    Ok(m)
}

/// Lines of `short-iso` journal output at or after `cutoff_ms`. A line with
/// no timestamp follows the one before it. The flag says whether any
/// timestamp was found (an OCI app's console log has none).
pub(super) fn since_lines(text: &str, cutoff_ms: i64) -> (String, bool) {
    let mut keep = true;
    let mut seen = false;
    let mut out = Vec::new();
    for line in text.lines() {
        if let Some(ms) = line_time(line) {
            seen = true;
            keep = ms >= cutoff_ms;
        }
        if keep || !seen {
            out.push(line);
        }
    }
    (out.join("\n"), seen)
}

fn line_time(line: &str) -> Option<i64> {
    let ts = line.split_whitespace().next()?;
    if ts.len() < 24 || !ts.is_char_boundary(19) {
        return None;
    }
    // short-iso writes the zone as +0000: rfc3339 wants +00:00.
    let mut s = ts.to_string();
    let n = s.len();
    if (s.as_bytes()[n - 5] == b'+' || s.as_bytes()[n - 5] == b'-') && !s.ends_with('Z') {
        s.insert(n - 2, ':');
    }
    crate::history::rfc3339_ms(&s)
}

/// What an instance is to isb: `app`, `database`, `stack` (a compose
/// service's replica), `tunnel`, `workspace`, `build` or `sandbox`.
/// `app_db` is `Some(is_database)` when an app owns it.
pub(super) fn kind(
    labels: &BTreeMap<String, String>,
    tunnel: bool,
    app_db: Option<bool>,
) -> &'static str {
    match crate::workspace::kind_of(labels) {
        "replica" if tunnel => "tunnel",
        "replica" => match app_db {
            Some(true) => "database",
            Some(false) => "app",
            None => "stack",
        },
        k => k,
    }
}

/// What the audit row of one of these calls keeps besides the usual: the
/// argv, a file's path and size, the names (not values) of the environment,
/// the size (not the text) of stdin. Contents never.
pub(super) fn audit_details(action: &str, args: &Value) -> Option<Value> {
    if !WRITES.contains(&action) && !SECRET_READS.contains(&action) {
        return None;
    }
    let mut out = serde_json::Map::new();
    let clip = |s: &str| -> String { s.chars().take(256).collect() };
    if let Some(argv) = args.get("argv").and_then(Value::as_array) {
        let v: Vec<String> = argv
            .iter()
            .filter_map(Value::as_str)
            .take(64)
            .map(clip)
            .collect();
        out.insert("argv".into(), json!(v));
    }
    if let Some(p) = args.get("path").and_then(Value::as_str) {
        out.insert("path".into(), json!(clip(p)));
    }
    if let Some(s) = args.get("stdin").and_then(Value::as_str) {
        out.insert("stdin_bytes".into(), json!(s.len()));
    }
    if let Some(c) = args.get("content").and_then(Value::as_str) {
        out.insert("bytes".into(), json!(content_len(c, args)));
    }
    if let Some(env) = args.get("env").and_then(Value::as_object) {
        let keys: Vec<&String> = env.keys().take(64).collect();
        out.insert("env_keys".into(), json!(keys));
    }
    Some(Value::Object(out))
}

fn content_len(content: &str, args: &Value) -> usize {
    if args.get("encoding").and_then(Value::as_str) == Some("base64") {
        crate::rpc::b64_decode(content)
            .map(|b| b.len())
            .unwrap_or(content.len())
    } else {
        content.len()
    }
}

// --- running things -----------------------------------------------------

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

/// An app's stack, qualified, and its service's status.
fn app_service(
    d: &Daemon,
    org: &OrgId,
    app: &str,
) -> Result<(crate::app::App, String, ServiceStatus)> {
    let a = d.apps.get(org, app)?;
    let stack = crate::stack::qualified(org, &a.spec.stack()?);
    let st = d
        .ctl
        .status(&stack)
        .map_err(|_| Error::NotFound(format!("{app} is not deployed")))?;
    let svc = st
        .services
        .into_iter()
        .find(|s| s.service == app)
        .ok_or_else(|| Error::NotFound(format!("{app} is not deployed")))?;
    Ok((a, stack, svc))
}

/// The files isb delivers into an app's replicas: app files and secrets.
fn managed_files(
    d: &Daemon,
    stack: &str,
    service: &str,
    app: Option<&crate::app::App>,
) -> Vec<String> {
    let mut out: Vec<String> = app
        .map(|a| a.spec.files.iter().map(|f| f.path.clone()).collect())
        .unwrap_or_default();
    if let Ok(def) = d.ctl.definition(stack) {
        if let Ok(spec) = def.service(service) {
            out.extend(spec.secrets.iter().map(|s| s.guest_path()));
        }
    }
    out
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

// --- looking -----------------------------------------------------------

fn labels_of(info: &SandboxInfo) -> BTreeMap<String, String> {
    info.config
        .iter()
        .filter_map(|(k, v)| k.strip_prefix("user.").map(|k| (k.to_string(), v.clone())))
        .collect()
}

/// An app's replica, as the stack's status has it, with who owns it.
struct Replica {
    stack: String,
    service: String,
    status: InstanceStatus,
}

/// Every instance of the org as one row each.
fn rows(d: &Daemon, c: &Caller, org: &OrgId) -> Vec<Value> {
    let snap = d.ctl.snapshot();
    let mut replicas: BTreeMap<String, Replica> = BTreeMap::new();
    for s in d.ctl.list().into_iter().filter(|s| s.org == org.as_str()) {
        for svc in &s.services {
            for i in &svc.instances {
                replicas.insert(
                    i.name.clone(),
                    Replica {
                        stack: s.name.clone(),
                        service: svc.service.clone(),
                        status: i.clone(),
                    },
                );
            }
        }
    }
    let apps = d.apps.list(org).unwrap_or_default();
    let owner = |r: &Replica| -> Option<(&crate::app::App, bool)> {
        apps.iter().find_map(|a| {
            if a.spec.name != r.service {
                return None;
            }
            let stack = a.spec.stack().ok()?;
            let own = stack == r.stack;
            let preview = r.stack.starts_with(&format!("{}-", a.spec.project))
                && crate::app::preview::is_pr_suffix(&r.stack);
            (own || preview).then_some((a, !own))
        })
    };
    let now = now_secs() as i64;
    let mut out = Vec::new();
    let mut push = |name: &str,
                    status: &str,
                    labels: &BTreeMap<String, String>,
                    sample: Option<&crate::metrics::InstanceSample>| {
        let rep = replicas.get(name);
        let app = rep.and_then(&owner);
        let tunnel = rep.is_some_and(|r| r.stack == crate::ingress::cloudflare::TUNNEL_STACK);
        let is_db = app.map(|(a, _)| matches!(a.spec.source, crate::app::Source::Database(_)));
        let kind = kind(labels, tunnel, is_db);
        let created = sample.map(|s| s.created_at.clone()).unwrap_or_default();
        let age = crate::history::rfc3339_ms(&created).map(|ms| (now - ms / 1000).max(0));
        let st = rep.map(|r| &r.status);
        out.push(json!({
            "name": name,
            "org": org,
            "kind": kind,
            "type": sample.map(|s| s.kind.as_str()),
            "app": app.map(|(a, _)| a.spec.name.as_str()),
            "project": app.map(|(a, _)| a.spec.project.as_str()),
            "environment": app.map(|(a, _)| a.spec.environment.as_str()),
            "preview": app.map(|(_, p)| p).filter(|p| *p),
            "stack": rep.map(|r| r.stack.as_str()),
            "service": rep.map(|r| r.service.as_str()),
            "slot": st.map(|s| s.slot),
            "revision": st.map(|s| s.rev.as_str()),
            "status": status,
            "health": st.map(|s| s.health.as_str()).unwrap_or("none"),
            "in_rotation": st.map(|s| s.in_rotation),
            "ip": st.and_then(|s| s.ip.clone()).or_else(|| sample.and_then(|s| s.ip.clone())),
            "restarts": st.map(|s| s.restarts),
            "created_at": created,
            "age_s": age,
            "cpu_pct": st.and_then(|s| s.cpu_pct).or_else(|| sample.and_then(|s| s.cpu_pct)),
            "mem_bytes": st.and_then(|s| s.mem_bytes).or_else(|| sample.and_then(|s| s.mem_bytes)),
            "image": sample.map(|s| s.image.as_str()),
            "owner": labels.get("isb.owner"),
            "expires_at": labels.get("isb.expires_at").and_then(|v| v.parse::<u64>().ok()),
        }));
    };
    for s in snap.instances.values() {
        if OrgId::from_incus_project(&s.project).as_ref() != Some(org) {
            continue;
        }
        let reachable = c.is_trusted()
            || c.principal().is_some()
            || d.policy.any_instance
            || s.labels.contains_key("isb.stack")
            || s.labels.contains_key(LABEL_OWNER);
        if reachable {
            push(&s.name, &s.status, &s.labels, Some(s));
        }
    }
    // A replica just created may not be in the sample yet.
    let present: BTreeSet<&str> = snap
        .instances
        .values()
        .filter(|s| OrgId::from_incus_project(&s.project).as_ref() == Some(org))
        .map(|s| s.name.as_str())
        .collect();
    let missing: Vec<(String, InstanceStatus)> = replicas
        .iter()
        .filter(|(n, _)| !present.contains(n.as_str()))
        .map(|(n, r)| (n.clone(), r.status.clone()))
        .collect();
    for (name, st) in missing {
        let labels = BTreeMap::from([("isb.stack".to_string(), String::new())]);
        push(&name, &st.status, &labels, None);
    }
    out.sort_by(|a, b| {
        let key = |v: &Value| {
            (
                v["stack"].as_str().unwrap_or("~").to_string(),
                v["service"].as_str().unwrap_or("").to_string(),
                v["slot"].as_u64().unwrap_or(0),
                v["name"].as_str().unwrap_or("").to_string(),
            )
        };
        key(a).cmp(&key(b))
    });
    out
}

fn instance_list(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        #[serde(default)]
        #[allow(dead_code)]
        org: Option<String>,
        app: Option<String>,
        stack: Option<String>,
        service: Option<String>,
        kind: Option<String>,
        status: Option<String>,
    }
    let org = arg_org(&a)?;
    let a: A = args(a)?;
    d.oc(&a.org)?;
    if let Some(k) = &a.kind {
        let ok = [
            "app",
            "database",
            "stack",
            "tunnel",
            "workspace",
            "build",
            "sandbox",
        ];
        if !ok.contains(&k.as_str()) {
            return Err(Error::invalid(format!("kind is one of {}", ok.join(", "))));
        }
    }
    let want = |v: &Value, k: &str, f: &Option<String>| {
        f.as_deref().is_none_or(|f| v[k].as_str() == Some(f))
    };
    let instances: Vec<Value> = rows(d, c, &org)
        .into_iter()
        .filter(|v| {
            want(v, "app", &a.app)
                && want(v, "stack", &a.stack)
                && want(v, "service", &a.service)
                && want(v, "kind", &a.kind)
                && a.status.as_deref().is_none_or(|s| {
                    v["status"]
                        .as_str()
                        .is_some_and(|x| x.eq_ignore_ascii_case(s))
                })
        })
        .collect();
    Ok(json!({"org": org, "count": instances.len(), "instances": instances}))
}

fn instance_get(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        name: String,
        #[serde(default)]
        #[allow(dead_code)]
        org: Option<String>,
        history: Option<usize>,
    }
    let org = arg_org(&a)?;
    let a: A = args(a)?;
    let oc = d.oc(&a.org)?;
    let info = d.reach(c, &oc, &a.name)?;
    let all = rows(d, c, &org);
    let mut out = all
        .into_iter()
        .find(|v| v["name"].as_str() == Some(a.name.as_str()))
        .unwrap_or_else(|| json!({"name": a.name, "org": org, "status": info.status}));
    let labels = labels_of(&info);
    // Limits as incus has them.
    let limits: BTreeMap<&str, &String> = info
        .config
        .iter()
        .filter_map(|(k, v)| k.strip_prefix("limits.").map(|k| (k, v)))
        .collect();
    // The environment's names, never its values.
    let mut env_names: BTreeSet<String> = info
        .config
        .keys()
        .filter_map(|k| k.strip_prefix("environment.").map(String::from))
        .collect();
    let mut volumes = Vec::new();
    let mut ports = Value::Null;
    let mut managed: Vec<String> = Vec::new();
    let mut domains = json!([]);
    if let (Some(stack), Some(svc)) = (labels.get("isb.stack"), labels.get("isb.service")) {
        let q = crate::stack::qualified(&org, stack);
        if let Ok(def) = d.ctl.definition(&q) {
            if let Ok(spec) = def.service(svc) {
                env_names.extend(spec.env.vars.keys().cloned());
                env_names.extend(spec.env.secrets.keys().cloned());
                volumes = spec
                    .volumes
                    .iter()
                    .filter_map(|v| serde_json::to_value(v).ok())
                    .collect();
                ports = serde_json::to_value(&spec.ports).unwrap_or_default();
                out["image"] = json!(def.instance_image(svc, &spec.image));
                out["resources"] = json!({"cpus": spec.cpus, "memory": spec.memory});
                managed = spec.secrets.iter().map(|s| s.guest_path()).collect();
            }
            if let Ok(app) = d.apps.get(&org, svc) {
                managed.extend(app.spec.files.iter().map(|f| f.path.clone()));
            }
        }
        if let Ok(st) = d.ctl.status(&q) {
            if let Some(s) = st.services.iter().find(|s| &s.service == svc) {
                let routed = out["in_rotation"].as_bool().unwrap_or(false);
                domains = json!(
                    s.domains
                        .iter()
                        .map(|dm| {
                            let mut v = serde_json::to_value(dm).unwrap_or_default();
                            v["routed_to_this"] = json!(routed);
                            v
                        })
                        .collect::<Vec<_>>()
                );
                out["published_ports"] = json!(
                    s.ports
                        .iter()
                        .map(|p| json!({"listen": p.listen, "target": p.target, "backends": p.backends}))
                        .collect::<Vec<_>>()
                );
                if let Some(i) = s.instances.iter().find(|i| i.name == a.name) {
                    out["last_probe"] = json!(i.last_probe);
                    out["cpu_history"] = json!(i.cpu_history);
                    out["disk_bytes"] = json!(i.disk_bytes);
                }
            }
        }
    }
    // Devices of the instance itself: mounts and proxies.
    let devices: Vec<Value> = info
        .devices
        .iter()
        .map(|(n, p)| {
            let mut v = json!({"name": n});
            for k in [
                "type", "path", "source", "pool", "listen", "connect", "size", "readonly",
            ] {
                if let Some(x) = p.get(k) {
                    v[k] = json!(x);
                }
            }
            v
        })
        .collect();
    out["labels"] = json!(
        labels
            .iter()
            .filter(|(k, _)| !k.starts_with("isb.create-token"))
            .collect::<BTreeMap<_, _>>()
    );
    out["config_limits"] = json!(limits);
    out["env_names"] = json!(env_names);
    out["volumes"] = json!(volumes);
    out["ports"] = ports;
    out["devices"] = json!(devices);
    out["domains"] = domains;
    out["managed_files"] = json!(managed);
    out["profiles"] = json!(info.profiles);
    out["instance_type"] = json!(info.instance_type);
    // What happened to it: the controller's events, incus lifecycle events
    // and markers. Audit rows are the audit log's.
    let q = crate::history::HistoryQuery {
        org: Some(org.to_string()),
        object: Some(a.name.clone()),
        exact: true,
        source: Some("controller,incus,marker".into()),
        limit: Some(a.history.unwrap_or(25).clamp(1, 200)),
        ..Default::default()
    };
    let hist = d
        .audit
        .timeline(&q, &Visibility::Orgs(vec![org.to_string()]), None)
        .map(|p| serde_json::to_value(p.items).unwrap_or_default())
        .unwrap_or_else(|e| json!({"error": e.to_string()}));
    out["history"] = hist;
    Ok(out)
}

// --- app-level tools --------------------------------------------------

fn app_logs(d: &Daemon, a: Value, _c: &Caller) -> Result<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        name: String,
        #[serde(default)]
        #[allow(dead_code)]
        org: Option<String>,
        replica: Option<u32>,
        tail: Option<usize>,
        since: Option<String>,
    }
    let org = arg_org(&a)?;
    let a: A = args(a)?;
    let (_, stack, svc) = app_service(d, &org, &a.name)?;
    if let Some(n) = a.replica {
        if !svc.instances.iter().any(|i| i.slot == n) {
            return Err(Error::NotFound(format!("{} has no replica {n}", a.name)));
        }
    }
    let cutoff = match &a.since {
        Some(s) => {
            let d = crate::flex::parse_duration(s).map_err(Error::invalid)?;
            Some(now_ms_i64() - d.as_millis() as i64)
        }
        None => None,
    };
    // A `since` filters what was read, so read more than the tail then.
    let lines = a.tail.unwrap_or(200).clamp(1, 5000);
    let read = if cutoff.is_some() { 5000 } else { lines };
    let mut logs = d.ctl.logs(&stack, &a.name, a.replica, read)?;
    let mut since_applied = true;
    if let Some(cut) = cutoff {
        for text in logs.values_mut() {
            let (t, seen) = since_lines(text, cut);
            since_applied &= seen || text.is_empty();
            *text = t;
        }
    }
    for text in logs.values_mut() {
        let all: Vec<&str> = text.lines().collect();
        if all.len() > lines {
            *text = all[all.len() - lines..].join("\n");
        }
    }
    let slots: BTreeMap<&str, u32> = svc
        .instances
        .iter()
        .map(|i| (i.name.as_str(), i.slot))
        .collect();
    let mut out = json!({"app": a.name, "logs": logs, "replicas": slots});
    if cutoff.is_some() && !since_applied {
        out["note"] = json!(
            "since could not be applied to every replica: an OCI image's console log has no timestamps, so all of its tail is shown"
        );
    }
    Ok(out)
}

fn now_ms_i64() -> i64 {
    crate::stack::controller::now_ms() as i64
}

fn app_top(d: &Daemon, a: Value, _c: &Caller) -> Result<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        name: String,
        #[serde(default)]
        #[allow(dead_code)]
        org: Option<String>,
        #[serde(default)]
        history: bool,
    }
    let org = arg_org(&a)?;
    let a: A = args(a)?;
    let (app, _, svc) = app_service(d, &org, &a.name)?;
    let snap = d.ctl.snapshot();
    let project = org.incus_project();
    let mut cpu = 0.0f32;
    let mut mem = 0u64;
    let replicas: Vec<Value> = svc
        .instances
        .iter()
        .map(|i| {
            cpu += i.cpu_pct.unwrap_or(0.0);
            mem += i.mem_bytes.unwrap_or(0);
            let net = snap.instances.get(&format!("{project}/{}", i.name));
            let mut v = json!({
                "replica": i.slot,
                "instance": i.name,
                "status": i.status,
                "health": i.health,
                "in_rotation": i.in_rotation,
                "cpu_pct": i.cpu_pct,
                "mem_bytes": i.mem_bytes,
                "disk_bytes": i.disk_bytes,
                "net_rx_bytes": net.and_then(|n| n.net_rx_bytes),
                "net_tx_bytes": net.and_then(|n| n.net_tx_bytes),
            });
            if a.history {
                v["cpu_history"] = json!(i.cpu_history);
            }
            v
        })
        .collect();
    Ok(json!({
        "app": a.name,
        "sampled_at": snap.at,
        "limits": app.spec.resources,
        "total": {"replicas": replicas.len(), "cpu_pct": cpu, "mem_bytes": mem},
        "replicas": replicas,
    }))
}

fn app_events(d: &Daemon, a: Value, _c: &Caller) -> Result<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        name: String,
        #[serde(default)]
        #[allow(dead_code)]
        org: Option<String>,
        #[serde(default)]
        since: u64,
        limit: Option<usize>,
        /// Also the stack's own events (deploys of the whole stack).
        #[serde(default)]
        stack_wide: bool,
    }
    let org = arg_org(&a)?;
    let a: A = args(a)?;
    let app = d.apps.get(&org, &a.name)?;
    let stack = crate::stack::qualified(&org, &app.spec.stack()?);
    let limit = a.limit.unwrap_or(100).clamp(1, 1000);
    let (seq, events) = d.ctl.events(a.since, 1000);
    let mut events: Vec<_> = events
        .into_iter()
        .filter(|e| {
            e.stack == stack && (e.service == a.name || (a.stack_wide && e.service.is_empty()))
        })
        .collect();
    let skip = events.len().saturating_sub(limit);
    events.drain(..skip);
    Ok(json!({"app": a.name, "seq": seq, "events": events}))
}

fn app_restart(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        name: String,
        #[serde(default)]
        #[allow(dead_code)]
        org: Option<String>,
        #[serde(default)]
        wait: bool,
        timeout: Option<String>,
    }
    let org = arg_org(&a)?;
    let a: A = args(a)?;
    let (_, stack, svc) = app_service(d, &org, &a.name)?;
    if svc.replicas == 0 {
        return Err(Error::invalid(format!(
            "{} is scaled to 0: there is nothing to restart (app_scale starts it)",
            a.name
        )));
    }
    let timeout = match &a.timeout {
        Some(t) => crate::flex::parse_duration(t).map_err(Error::invalid)?,
        None => Duration::from_secs(600),
    };
    d.ctl.redeploy(&stack, &a.name)?;
    d.ctl.service_event(
        "info",
        &stack,
        &a.name,
        format!(
            "restarted by {} (rolling replace of its replicas)",
            caller_name(c)
        ),
    );
    let mut out = json!({
        "app": a.name,
        "restarting": svc.replicas,
        "rollout": "replicas are replaced one by one as the app's update_config says (stop-first by default; start-first keeps it serving); follow it with app_events or instance_list",
    });
    if a.wait {
        let st = super::wait_settled(&d.ctl, &stack, timeout)?;
        if let Some(s) = st.services.into_iter().find(|s| s.service == a.name) {
            out["status"] = json!(s);
        }
    }
    Ok(out)
}

fn app_scale(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        name: String,
        #[serde(default)]
        #[allow(dead_code)]
        org: Option<String>,
        replicas: u32,
    }
    let org = arg_org(&a)?;
    let a: A = args(a)?;
    if a.replicas > 100 {
        return Err(Error::invalid("replicas is at most 100"));
    }
    let app = d.apps.get(&org, &a.name)?;
    let stack = crate::stack::qualified(&org, &app.spec.stack()?);
    let before = app.spec.replicas;
    // The app's own setting changes too, so a later deploy keeps the count.
    d.apps
        .update(&org, &a.name, &json!({"replicas": a.replicas}))?;
    let deployed = d
        .ctl
        .status(&stack)
        .is_ok_and(|s| s.services.iter().any(|s| s.service == a.name));
    if deployed {
        d.ctl.scale(&stack, &a.name, a.replicas)?;
        d.ctl.service_event(
            "info",
            &stack,
            &a.name,
            format!("scaled to {} by {}", a.replicas, caller_name(c)),
        );
    }
    Ok(json!({
        "app": a.name,
        "replicas": a.replicas,
        "was": before,
        "applied": deployed,
        "message": if deployed {
            format!("{} now runs {} replica(s); the app's replicas setting is {} too, so deploys keep it", a.name, a.replicas, a.replicas)
        } else {
            format!("{} is not deployed: its replicas setting is {} and the next deploy runs that many", a.name, a.replicas)
        },
    }))
}

// --- instance-level acts ----------------------------------------------

fn instance_restart(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        name: String,
        #[serde(default)]
        #[allow(dead_code)]
        org: Option<String>,
        #[serde(default)]
        wait: bool,
        timeout: Option<String>,
    }
    let org = arg_org(&a)?;
    let a: A = args(a)?;
    let oc = d.oc(&a.org)?;
    let info = d.reach(c, &oc, &a.name)?;
    let labels = labels_of(&info);
    let timeout = match &a.timeout {
        Some(t) => crate::flex::parse_duration(t).map_err(Error::invalid)?,
        None => Duration::from_secs(120),
    };
    match crate::workspace::kind_of(&labels) {
        "workspace" => Err(Error::invalid(format!(
            "{} is the org's workspace; workspace_restart restarts it",
            a.name
        ))),
        "build" => Err(Error::invalid(format!(
            "{} is a build's machine; it goes when the build ends",
            a.name
        ))),
        "replica" => {
            let (Some(stack), Some(svc)) = (labels.get("isb.stack"), labels.get("isb.service"))
            else {
                return Err(Error::invalid(format!("{} has no stack labels", a.name)));
            };
            let slot = labels.get("isb.slot").and_then(|s| s.parse::<u32>().ok());
            let q = crate::stack::qualified(&org, stack);
            // The controller keeps the service at its replica count: it
            // makes a new instance for the slot.
            Sandbox::remove(&oc, &a.name, true)?;
            d.ctl.service_event(
                "info",
                &q,
                svc,
                format!(
                    "replica {} ({}) deleted by {}; the controller replaces it",
                    slot.unwrap_or(0),
                    a.name,
                    caller_name(c)
                ),
            );
            let mut out = json!({
                "deleted": a.name, "stack": stack, "service": svc, "slot": slot,
                "message": "the controller creates a replacement for the slot; instance_list shows it come up",
            });
            if a.wait {
                let started = Instant::now();
                loop {
                    let st = d.ctl.status(&q).ok();
                    let fresh = st
                        .as_ref()
                        .and_then(|s| s.services.iter().find(|s| &s.service == svc))
                        .and_then(|s| s.instances.iter().find(|i| Some(i.slot) == slot))
                        .filter(|i| {
                            i.status == "Running" && matches!(i.health.as_str(), "healthy" | "none")
                        })
                        .filter(|i| {
                            Sandbox::get(&oc, &i.name)
                                .and_then(|sb| sb.info())
                                .is_ok_and(|n| n.created_at != info.created_at)
                        })
                        .cloned();
                    if let Some(i) = fresh {
                        out["replacement"] = json!({"name": i.name, "status": i.status, "health": i.health, "in_rotation": i.in_rotation});
                        break;
                    }
                    if started.elapsed() >= timeout {
                        out["replacement"] = Value::Null;
                        out["timed_out"] = json!(true);
                        break;
                    }
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
            Ok(out)
        }
        _ => {
            d.workspaces.mark_active(&org.incus_project(), &a.name);
            Sandbox::get(&oc, &a.name)?.restart()?;
            Ok(json!({"restarted": a.name}))
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileArgs {
    name: String,
    #[serde(default)]
    org: Option<String>,
    path: String,
    #[serde(default)]
    encoding: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    mode: Option<Value>,
    #[serde(default)]
    uid: Option<u32>,
    #[serde(default)]
    gid: Option<u32>,
    #[serde(default = "yes")]
    parents: bool,
}

fn yes() -> bool {
    true
}

/// The files isb delivers into `name`, when it is a stack replica.
fn managed_for(d: &Daemon, org: &OrgId, info: &SandboxInfo) -> Vec<String> {
    let labels = labels_of(info);
    match (labels.get("isb.stack"), labels.get("isb.service")) {
        (Some(stack), Some(svc)) => {
            let app = d.apps.get(org, svc).ok();
            managed_files(d, &crate::stack::qualified(org, stack), svc, app.as_ref())
        }
        _ => Vec::new(),
    }
}

fn instance_file_read(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    let org = arg_org(&a)?;
    let a: FileArgs = args(a)?;
    let oc = d.oc(&a.org)?;
    let info = d.reach(c, &oc, &a.name)?;
    let path = clean_path(&a.path)?;
    if let Some(why) = refuse_path(&path, false, &[]) {
        return Err(Error::Forbidden(why));
    }
    let want = a.encoding.as_deref().unwrap_or("auto");
    if !["auto", "utf8", "base64"].contains(&want) {
        return Err(Error::invalid("encoding is auto, utf8 or base64"));
    }
    d.workspaces.mark_active(&org.incus_project(), &a.name);
    let body = oc
        .read_file(&a.name, &path)?
        .ok_or_else(|| Error::NotFound(format!("{path} in {}", a.name)))?;
    if body.len() > FILE_CAP {
        return Err(Error::invalid(format!(
            "{path} is {} bytes; instance_file_read returns at most {} MiB (instance_exec with head, tail or split reads a part)",
            body.len(),
            FILE_CAP / 1024 / 1024
        )));
    }
    let text = std::str::from_utf8(&body)
        .ok()
        .filter(|t| !t.contains('\0'));
    let (encoding, content) = match (want, text) {
        ("utf8", None) => {
            return Err(Error::invalid(format!(
                "{path} is not UTF-8 text; ask for encoding base64"
            )));
        }
        ("auto" | "utf8", Some(t)) => ("utf8", t.to_string()),
        _ => ("base64", crate::rpc::b64_encode(&body)),
    };
    let _ = info;
    Ok(
        json!({"instance": a.name, "path": path, "size": body.len(), "encoding": encoding, "content": content}),
    )
}

fn instance_file_write(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    let org = arg_org(&a)?;
    let a: FileArgs = args(a)?;
    let oc = d.oc(&a.org)?;
    let info = d.reach(c, &oc, &a.name)?;
    let path = clean_path(&a.path)?;
    let content = a.content.as_deref().ok_or_else(|| {
        Error::invalid("content is required (an empty string writes an empty file)")
    })?;
    let data = match a.encoding.as_deref().unwrap_or("utf8") {
        "utf8" => content.as_bytes().to_vec(),
        "base64" => crate::rpc::b64_decode(content)
            .map_err(|e| Error::invalid(format!("content is not base64: {e}")))?,
        _ => return Err(Error::invalid("encoding is utf8 or base64")),
    };
    if data.len() > FILE_CAP {
        return Err(Error::invalid(format!(
            "{} bytes is over the {} MiB limit of instance_file_write",
            data.len(),
            FILE_CAP / 1024 / 1024
        )));
    }
    if let Some(why) = refuse_path(&path, true, &managed_for(d, &org, &info)) {
        return Err(Error::Forbidden(why));
    }
    let mode = match &a.mode {
        Some(m) => parse_mode(m)?,
        None => 0o644,
    };
    let (uid, gid) = (a.uid.unwrap_or(0), a.gid.unwrap_or(0));
    d.workspaces.mark_active(&org.incus_project(), &a.name);
    if a.parents {
        make_parents(&oc, &a.name, &path, uid, gid)?;
    }
    oc.push_file(&a.name, &path, &data, uid, gid, mode)?;
    Ok(
        json!({"instance": a.name, "path": path, "bytes": data.len(), "mode": format!("{mode:04o}"), "uid": uid, "gid": gid}),
    )
}

/// Create the directories above `path` that are missing (those that exist
/// keep their owner and mode).
fn make_parents(oc: &Client, instance: &str, path: &str, uid: u32, gid: u32) -> Result<()> {
    let mut dirs: Vec<String> = Vec::new();
    let mut cur = path
        .rsplit_once('/')
        .map(|(p, _)| p.to_string())
        .unwrap_or_default();
    while !cur.is_empty() {
        if oc.read_file(instance, &cur)?.is_some() {
            break;
        }
        dirs.push(cur.clone());
        cur = cur
            .rsplit_once('/')
            .map(|(p, _)| p.to_string())
            .unwrap_or_default();
    }
    for d in dirs.iter().rev() {
        oc.make_dir(instance, d, uid, gid, 0o755)?;
    }
    Ok(())
}

// --- registration -----------------------------------------------------

/// The `kubectl`-shaped tools.
#[expect(clippy::too_many_lines, reason = "one table of schemas")]
pub(super) fn register(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    tool!(
        r,
        d,
        "instance_list",
        "List instances",
        "Every instance isb manages in the org, one row each (kubectl get pods): kind (app replica `app`, `database`, a compose service's `stack`, `tunnel`, `workspace`, `sandbox`, `build`), the owning app or stack and service, replica slot, revision, status, health (healthy, unhealthy, starting, none) and whether it is in rotation (receiving traffic), IP, restarts, age, CPU and memory now. Filter by app, stack, service, kind or status. instance_get describes one.",
        obj(
            json!({
                "app": {"type": "string"},
                "stack": {"type": "string"},
                "service": {"type": "string"},
                "kind": {"type": "string", "enum": ["app", "database", "stack", "tunnel", "workspace", "build", "sandbox"]},
                "status": {"type": "string", "description": "Running, Stopped, ..."}
            }),
            &[]
        ),
        annotations("instance_list", ann),
        instance_list
    );
    tool!(
        r,
        d,
        "instance_get",
        "Describe an instance",
        "One instance in full (kubectl describe pod): everything instance_list shows plus image and revision, resource limits, the names of its environment variables (never values), volumes and devices, ports, the domains its service serves and whether traffic reaches this replica, the last health probe, the files isb delivers into it, labels, and its recent history (controller events, incus lifecycle events, restarts).",
        obj(
            json!({
                "name": {"type": "string", "description": "The instance's name, from instance_list."},
                "history": {"type": "integer", "minimum": 1, "maximum": 200, "description": "History rows to return (default 25)."}
            }),
            &["name"]
        ),
        annotations("instance_get", ann),
        instance_get
    );
    let exec_props = |name_doc: &str| {
        json!({
            "name": {"type": "string", "description": name_doc},
            "argv": {"type": "array", "items": {"type": "string"}, "minItems": 1, "description": "The command and its arguments; no shell unless you run one: [\"sh\", \"-c\", \"...\"]."},
            "cwd": {"type": "string"},
            "user": {"type": "string", "description": "A guest user name, uid or uid:gid (default: the instance's default, root)."},
            "env": {"type": "object", "additionalProperties": {"type": "string"}},
            "stdin": {"type": "string", "description": "Text fed to the command's stdin (at most 1 MiB)."},
            "timeout": {"type": "string", "description": "Kill the command after this long, e.g. 30s (default 60s, at most 15m). A command that runs out answers timed_out with the output so far."}
        })
    };
    let mut app_exec_props = exec_props("The app.");
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
    tool!(
        r,
        d,
        "app_logs",
        "An app's logs",
        "Recent output of an app's replicas, by app name (kubectl logs deploy/NAME): all replicas, or one with `replica`. `tail` lines (default 200, at most 5000); `since` keeps lines newer than a duration like 10m (for system images; an OCI image's console log has no timestamps). A replaced replica's logs go with it, so there is no `previous`: app_events and history_query say what happened.",
        obj(
            json!({
                "name": {"type": "string", "description": "The app."},
                "replica": {"type": "integer", "minimum": 1, "description": "One replica's slot."},
                "tail": {"type": "integer", "minimum": 1, "maximum": 5000},
                "since": {"type": "string", "description": "e.g. 10m, 2h."}
            }),
            &["name"]
        ),
        annotations("app_logs", ann),
        app_logs
    );
    tool!(
        r,
        d,
        "app_top",
        "An app's resource use",
        "Per replica CPU (percent of one core), memory, disk and network counters now, and the totals, next to the app's limits (kubectl top pods). history=true adds each replica's recent CPU samples; metrics_query has the long history.",
        obj(
            json!({"name": {"type": "string"}, "history": {"type": "boolean"}}),
            &["name"]
        ),
        annotations("app_top", ann),
        app_top
    );
    tool!(
        r,
        d,
        "app_events",
        "An app's events",
        "The events about one app (kubectl get events --for): deploys, rollouts, health changes, restarts, newest last. `since` is the last seq you saw; stack_wide=true adds the events of its whole stack.",
        obj(
            json!({
                "name": {"type": "string"},
                "since": {"type": "integer", "minimum": 0},
                "limit": {"type": "integer", "minimum": 1, "maximum": 1000},
                "stack_wide": {"type": "boolean"}
            }),
            &["name"]
        ),
        annotations("app_events", ann),
        app_events
    );
    tool!(
        r,
        d,
        "app_restart",
        "Restart an app",
        "Replace an app's replicas one by one with fresh instances of the same revision (kubectl rollout restart): in the order its update_config says (stop-first by default, start-first for no downtime). Picks up a moved image tag. wait=true blocks until the rollout settles (at most `timeout`, default 10m). Members and up.",
        obj(
            json!({
                "name": {"type": "string"},
                "wait": {"type": "boolean"},
                "timeout": {"type": "string", "description": "How long wait may take, e.g. 5m."}
            }),
            &["name"]
        ),
        annotations("app_restart", ann),
        app_restart
    );
    tool!(
        r,
        d,
        "app_scale",
        "Scale an app",
        "Set an app's replica count (kubectl scale; 0 stops it without removing it). The app's own replicas setting changes too, so a later deploy keeps the count.",
        obj(
            json!({"name": {"type": "string"}, "replicas": {"type": "integer", "minimum": 0, "maximum": 100}}),
            &["name", "replicas"]
        ),
        annotations("app_scale", ann),
        app_scale
    );
    tool!(
        r,
        d,
        "instance_restart",
        "Restart an instance",
        "Replace one instance (kubectl delete pod): a stack or app replica is deleted and the controller creates its replacement for the slot (wait=true blocks until that one runs, at most `timeout`, default 2m); a sandbox is restarted in place. The workspace has workspace_restart. Members and up.",
        obj(
            json!({
                "name": {"type": "string", "description": "The instance's name, from instance_list."},
                "wait": {"type": "boolean"},
                "timeout": {"type": "string"}
            }),
            &["name"]
        ),
        annotations("instance_restart", ann),
        instance_restart
    );
    tool!(
        r,
        d,
        "instance_file_read",
        "Read a file in an instance",
        "Read one file of an instance, running or stopped (kubectl cp from it, for small files): at most 4 MiB. Text comes back as `utf8`, anything else as `base64` (or ask with `encoding`). It can hold secrets, so it is for members and up, and recorded in the audit log. Not the kernel's /proc, /sys, /dev, nor a workspace's token. For larger files run `head`, `tail` or `split` with instance_exec.",
        obj(
            json!({
                "name": {"type": "string", "description": "The instance's name."},
                "path": {"type": "string", "description": "Absolute path in the instance."},
                "encoding": {"type": "string", "enum": ["auto", "utf8", "base64"]}
            }),
            &["name", "path"]
        ),
        annotations("instance_file_read", ann),
        instance_file_read
    );
    tool!(
        r,
        d,
        "instance_file_write",
        "Write a file in an instance",
        "Write one file into an instance, running or stopped, replacing it (kubectl cp into it, for small files): at most 4 MiB, as utf8 text or base64, owned by uid/gid (default root) with `mode` (default 0644); missing parent directories are created unless parents=false. Refused: /run/isb, /run/secrets, /etc/isb, files isb delivers from org secrets (an app's `files`, a stack's secrets), the kernel's /proc, /sys, /dev. The audit log records the path and size, not the content. Members and up.",
        obj(
            json!({
                "name": {"type": "string", "description": "The instance's name."},
                "path": {"type": "string", "description": "Absolute path in the instance."},
                "content": {"type": "string"},
                "encoding": {"type": "string", "enum": ["utf8", "base64"]},
                "mode": {"type": "string", "description": "Octal, e.g. 0644."},
                "uid": {"type": "integer", "minimum": 0},
                "gid": {"type": "integer", "minimum": 0},
                "parents": {"type": "boolean", "description": "Create missing directories (default true)."}
            }),
            &["name", "path", "content"]
        ),
        annotations("instance_file_write", ann),
        instance_file_write
    );
    Ok(())
}

#[cfg(test)]
#[path = "kube_tests.rs"]
mod tests;
