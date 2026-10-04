//! `isb instance ...`, `isb cp` and the app commands that look into
//! running replicas (`exec`, `logs`, `restart`, `scale`, `top`, `events`):
//! isb for kubectl users (docs/guides/kubectl.md).

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::time::Duration;

use clap::{Args, Subcommand};
use serde_json::{Value, json};

use isb::{Error, Result};

use super::{SHORT, call, print_json, table};

fn with_org(org: &Option<String>, mut args: Value) -> Value {
    if let Some(o) = org {
        args["org"] = json!(o);
    }
    args
}

/// A command to run in a replica or an instance.
#[derive(Args)]
pub struct ExecArgs {
    /// Run in this replica (its slot); default: a healthy one.
    #[arg(long, conflicts_with = "instance")]
    pub replica: Option<u32>,
    /// Run in this instance of the app (by name).
    #[arg(long, hide = true)]
    pub instance: Option<String>,
    /// Guest user (name, uid or uid:gid).
    #[arg(short, long)]
    pub user: Option<String>,
    /// Working directory.
    #[arg(short = 'w', long)]
    pub cwd: Option<String>,
    /// `KEY=VALUE` (repeatable).
    #[arg(short, long = "env")]
    pub env: Vec<String>,
    /// Feed this process's stdin to the command (at most 1 MiB).
    #[arg(short = 'i', long)]
    pub stdin: bool,
    /// Kill the command after this long (default 60s, at most 15m).
    #[arg(long)]
    pub timeout: Option<String>,
    /// The command and its arguments, passed as-is (no shell).
    #[arg(last = true, required = true)]
    pub argv: Vec<String>,
}

#[derive(Subcommand)]
pub enum InstanceCmd {
    /// Every instance isb manages in the org: replicas, databases, the
    /// workspace, sandboxes (kubectl get pods).
    #[command(alias = "list", alias = "ps")]
    Ls {
        #[arg(long)]
        app: Option<String>,
        #[arg(long)]
        stack: Option<String>,
        #[arg(long)]
        service: Option<String>,
        /// app, database, stack, tunnel, workspace, build or sandbox.
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// One instance in full (kubectl describe pod).
    #[command(alias = "describe")]
    Get {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Run a command in an instance: `isb instance exec web-1 -- ls -l`.
    Exec {
        name: String,
        #[command(flatten)]
        a: ExecArgs,
    },
    /// Replace one instance: a replica is deleted and the controller makes
    /// its successor; a sandbox restarts (kubectl delete pod).
    Restart {
        name: String,
        /// Wait until the replacement runs.
        #[arg(long)]
        wait: bool,
    },
}

#[derive(Args)]
pub struct AppLogs {
    pub name: String,
    /// One replica's slot (default: all).
    #[arg(long)]
    pub replica: Option<u32>,
    /// Lines per replica (default 200).
    #[arg(long, short = 'n')]
    pub tail: Option<usize>,
    /// Only lines newer than this, e.g. 10m.
    #[arg(long)]
    pub since: Option<String>,
}

fn dur_s(t: &Option<String>) -> Duration {
    t.as_deref()
        .and_then(|t| isb::parse_duration(t).ok())
        .unwrap_or(Duration::from_secs(60))
}

fn exec_args(org: &Option<String>, name: &str, a: &ExecArgs) -> Result<Value> {
    let mut env = BTreeMap::new();
    for e in &a.env {
        let (k, v) = e
            .split_once('=')
            .ok_or_else(|| Error::Invalid(format!("--env {e}: want KEY=VALUE")))?;
        env.insert(k.to_string(), v.to_string());
    }
    let mut args = json!({"name": name, "argv": a.argv});
    if let Some(r) = a.replica {
        args["replica"] = json!(r);
    }
    if let Some(i) = &a.instance {
        args["instance"] = json!(i);
    }
    if let Some(u) = &a.user {
        args["user"] = json!(u);
    }
    if let Some(w) = &a.cwd {
        args["cwd"] = json!(w);
    }
    if !env.is_empty() {
        args["env"] = json!(env);
    }
    if let Some(t) = &a.timeout {
        args["timeout"] = json!(t);
    }
    if a.stdin {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .map_err(|e| Error::Invalid(format!("stdin: {e}")))?;
        args["stdin"] = json!(s);
    }
    Ok(with_org(org, args))
}

/// Run `tool` (app_exec or instance_exec) and relay its output and exit code.
pub fn exec(org: &Option<String>, tool: &str, name: &str, a: &ExecArgs) -> Result<u8> {
    let args = exec_args(org, name, a)?;
    let timeout = dur_s(&a.timeout) + SHORT;
    let r = call(tool, args, timeout)?;
    print!("{}", r["stdout"].as_str().unwrap_or(""));
    eprint!("{}", r["stderr"].as_str().unwrap_or(""));
    let _ = std::io::stdout().flush();
    if r["truncated"] == json!(true) {
        eprintln!("isb: output was cut to its last 1 MiB per stream");
    }
    if r["timed_out"] == json!(true) {
        eprintln!("isb: timed out and was killed");
        return Ok(124);
    }
    Ok(r["exit_code"].as_i64().unwrap_or(1).clamp(0, 255) as u8)
}

pub fn logs(org: &Option<String>, a: AppLogs) -> Result<u8> {
    let mut args = json!({"name": a.name});
    if let Some(r) = a.replica {
        args["replica"] = json!(r);
    }
    if let Some(n) = a.tail {
        args["tail"] = json!(n);
    }
    if let Some(s) = &a.since {
        args["since"] = json!(s);
    }
    let r = call("app_logs", with_org(org, args), SHORT)?;
    let logs = r["logs"].as_object().cloned().unwrap_or_default();
    let many = logs.len() > 1;
    for (inst, text) in &logs {
        let text = text.as_str().unwrap_or("");
        for line in text.lines() {
            if many {
                println!("[{}] {line}", r["replicas"][inst].as_u64().unwrap_or(0));
            } else {
                println!("{line}");
            }
        }
    }
    if let Some(n) = r["note"].as_str() {
        eprintln!("isb: {n}");
    }
    Ok(0)
}

pub fn restart(org: &Option<String>, name: &str, wait: bool) -> Result<u8> {
    let r = call(
        "app_restart",
        with_org(org, json!({"name": name, "wait": wait, "timeout": "10m"})),
        Duration::from_secs(660),
    )?;
    eprintln!(
        "restarting {} replica(s) of {name}, one by one",
        r["restarting"]
    );
    if wait {
        let s = &r["status"];
        eprintln!(
            "{name}: {} ({}/{} healthy)",
            s["state"].as_str().unwrap_or(""),
            s["healthy"],
            s["replicas"]
        );
    }
    Ok(0)
}

pub fn scale(org: &Option<String>, name: &str, replicas: u32) -> Result<u8> {
    let r = call(
        "app_scale",
        with_org(org, json!({"name": name, "replicas": replicas})),
        SHORT,
    )?;
    eprintln!("{}", r["message"].as_str().unwrap_or(""));
    Ok(0)
}

fn mem(b: &Value) -> String {
    match b.as_u64() {
        Some(b) if b >= 1 << 30 => format!("{:.1}Gi", b as f64 / (1u64 << 30) as f64),
        Some(b) if b >= 1 << 20 => format!("{}Mi", b >> 20),
        Some(b) => format!("{}Ki", b >> 10),
        None => "-".into(),
    }
}

fn cpu(c: &Value) -> String {
    c.as_f64().map_or("-".into(), |c| format!("{c:.1}%"))
}

fn age(s: &Value) -> String {
    match s.as_i64() {
        Some(s) if s >= 86400 => format!("{}d", s / 86400),
        Some(s) if s >= 3600 => format!("{}h", s / 3600),
        Some(s) if s >= 60 => format!("{}m", s / 60),
        Some(s) => format!("{s}s"),
        None => "-".into(),
    }
}

pub fn top(org: &Option<String>, name: &str, json: bool) -> Result<u8> {
    let r = call("app_top", with_org(org, json!({"name": name})), SHORT)?;
    if json {
        print_json(&r);
        return Ok(0);
    }
    let mut rows = vec![vec![
        "REPLICA".into(),
        "INSTANCE".into(),
        "STATUS".into(),
        "HEALTH".into(),
        "CPU".into(),
        "MEMORY".into(),
    ]];
    for i in r["replicas"].as_array().into_iter().flatten() {
        rows.push(vec![
            i["replica"].to_string(),
            i["instance"].as_str().unwrap_or("").into(),
            i["status"].as_str().unwrap_or("").into(),
            i["health"].as_str().unwrap_or("").into(),
            cpu(&i["cpu_pct"]),
            mem(&i["mem_bytes"]),
        ]);
    }
    table(rows);
    println!(
        "total: {} replica(s), {} CPU, {} memory",
        r["total"]["replicas"],
        cpu(&r["total"]["cpu_pct"]),
        mem(&r["total"]["mem_bytes"])
    );
    Ok(0)
}

pub fn events(org: &Option<String>, name: &str, stack_wide: bool, json: bool) -> Result<u8> {
    let r = call(
        "app_events",
        with_org(org, json!({"name": name, "stack_wide": stack_wide})),
        SHORT,
    )?;
    if json {
        print_json(&r["events"]);
        return Ok(0);
    }
    for e in r["events"].as_array().into_iter().flatten() {
        let secs = e["at"].as_u64().unwrap_or(0) / 1000;
        let t = secs % 86400;
        println!(
            "{:02}:{:02}:{:02}  {:<5}  {}",
            t / 3600,
            t % 3600 / 60,
            t % 60,
            e["level"].as_str().unwrap_or(""),
            e["message"].as_str().unwrap_or("")
        );
    }
    Ok(0)
}

pub fn instance(org: &Option<String>, cmd: InstanceCmd) -> Result<u8> {
    match cmd {
        InstanceCmd::Ls {
            app,
            stack,
            service,
            kind,
            status,
            json,
        } => {
            let mut a = json!({});
            for (k, v) in [
                ("app", app),
                ("stack", stack),
                ("service", service),
                ("kind", kind),
                ("status", status),
            ] {
                if let Some(v) = v {
                    a[k] = json!(v);
                }
            }
            let r = call("instance_list", with_org(org, a), SHORT)?;
            if json {
                print_json(&r["instances"]);
                return Ok(0);
            }
            let mut rows = vec![vec![
                "NAME".into(),
                "KIND".into(),
                "OWNER".into(),
                "SLOT".into(),
                "STATUS".into(),
                "HEALTH".into(),
                "ROTATION".into(),
                "IP".into(),
                "RESTARTS".into(),
                "AGE".into(),
                "CPU".into(),
                "MEMORY".into(),
            ]];
            for i in r["instances"].as_array().into_iter().flatten() {
                let owner = match (
                    i["app"].as_str(),
                    i["service"].as_str(),
                    i["stack"].as_str(),
                ) {
                    (Some(a), _, _) => a.to_string(),
                    (None, Some(s), Some(st)) => format!("{st}/{s}"),
                    _ => "-".into(),
                };
                rows.push(vec![
                    i["name"].as_str().unwrap_or("").into(),
                    i["kind"].as_str().unwrap_or("").into(),
                    owner,
                    i["slot"].as_u64().map_or("-".into(), |s| s.to_string()),
                    i["status"].as_str().unwrap_or("").into(),
                    i["health"].as_str().unwrap_or("").into(),
                    i["in_rotation"]
                        .as_bool()
                        .map_or("-", |b| if b { "yes" } else { "no" })
                        .into(),
                    i["ip"].as_str().unwrap_or("-").into(),
                    i["restarts"].as_u64().map_or("-".into(), |s| s.to_string()),
                    age(&i["age_s"]),
                    cpu(&i["cpu_pct"]),
                    mem(&i["mem_bytes"]),
                ]);
            }
            table(rows);
        }
        InstanceCmd::Get { name, json } => {
            let r = call("instance_get", with_org(org, json!({"name": name})), SHORT)?;
            if json {
                print_json(&r);
                return Ok(0);
            }
            describe(&r);
        }
        InstanceCmd::Exec { name, a } => return exec(org, "instance_exec", &name, &a),
        InstanceCmd::Restart { name, wait } => {
            let r = call(
                "instance_restart",
                with_org(org, json!({"name": name, "wait": wait})),
                Duration::from_secs(180),
            )?;
            if let Some(d) = r["deleted"].as_str() {
                eprintln!("deleted {d}; the controller replaces it");
                if let Some(n) = r["replacement"]["name"].as_str() {
                    eprintln!("replacement {n} is {}", r["replacement"]["health"]);
                }
            } else {
                eprintln!("restarted {name}");
            }
        }
    }
    Ok(0)
}

/// `kubectl describe`: the fields, then the lists.
fn describe(r: &Value) {
    let field = |k: &str, v: String| println!("{:<16}{v}", format!("{k}:"));
    let s = |k: &str| r[k].as_str().unwrap_or("-").to_string();
    field("Name", s("name"));
    field("Kind", s("kind"));
    if r["app"].is_string() {
        field(
            "App",
            format!(
                "{} (project {}, environment {})",
                s("app"),
                s("project"),
                s("environment")
            ),
        );
    }
    if r["stack"].is_string() {
        field("Stack/Service", format!("{}/{}", s("stack"), s("service")));
        field("Slot", r["slot"].to_string());
        field("Revision", s("revision"));
    }
    field("Status", s("status"));
    field(
        "Health",
        format!(
            "{} (in rotation: {}, restarts: {})",
            s("health"),
            r["in_rotation"],
            r["restarts"]
        ),
    );
    field("IP", s("ip"));
    field("Image", s("image"));
    field("Age", age(&r["age_s"]));
    field(
        "CPU / memory",
        format!("{} / {}", cpu(&r["cpu_pct"]), mem(&r["mem_bytes"])),
    );
    if let Some(l) = r["config_limits"].as_object().filter(|l| !l.is_empty()) {
        let v: Vec<String> = l
            .iter()
            .map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or("")))
            .collect();
        field("Limits", v.join(" "));
    }
    let list = |k: &str| -> Vec<String> {
        r[k].as_array()
            .into_iter()
            .flatten()
            .map(|v| v.as_str().map_or_else(|| v.to_string(), String::from))
            .collect()
    };
    let names = list("env_names");
    if !names.is_empty() {
        field("Environment", names.join(", "));
    }
    let files = list("managed_files");
    if !files.is_empty() {
        field("Managed files", files.join(", "));
    }
    for d in r["domains"].as_array().into_iter().flatten() {
        field(
            "Domain",
            format!(
                "{} ({}, routed to this: {})",
                d["url"].as_str().or(d["host"].as_str()).unwrap_or(""),
                d["state"].as_str().unwrap_or(""),
                d["routed_to_this"]
            ),
        );
    }
    for v in r["volumes"].as_array().into_iter().flatten() {
        field("Volume", v.to_string());
    }
    for d in r["devices"].as_array().into_iter().flatten() {
        field("Device", d.to_string());
    }
    if let Some(p) = r["last_probe"].as_str().filter(|p| !p.is_empty()) {
        field("Last probe", p.to_string());
    }
    println!("History:");
    for h in r["history"].as_array().into_iter().flatten() {
        let secs = h["time"].as_i64().unwrap_or(0) / 1000;
        let t = secs % 86400;
        println!(
            "  {:02}:{:02}:{:02}  {:<22} {}",
            t / 3600,
            t % 3600 / 60,
            t % 60,
            h["kind"].as_str().unwrap_or(""),
            h["message"].as_str().unwrap_or("")
        );
    }
}

/// `isb cp SRC DST`: one side is `INSTANCE:/path`, the other a local path.
pub fn cp(org: &Option<String>, src: &str, dst: &str) -> Result<u8> {
    fn remote(s: &str) -> Option<(&str, &str)> {
        let (i, p) = s.split_once(':')?;
        (!i.is_empty() && !i.contains('/') && p.starts_with('/')).then_some((i, p))
    }
    match (remote(src), remote(dst)) {
        (Some(_), Some(_)) => Err(Error::Invalid(
            "copy between two instances: copy to a local file first".into(),
        )),
        (None, None) => Err(Error::Invalid(
            "one side is INSTANCE:/path in an instance of the org".into(),
        )),
        (Some((inst, path)), None) => {
            let r = call(
                "instance_file_read",
                with_org(
                    org,
                    json!({"name": inst, "path": path, "encoding": "base64"}),
                ),
                SHORT,
            )?;
            let bytes = isb::rpc::b64_decode(r["content"].as_str().unwrap_or(""))?;
            let mut local = std::path::PathBuf::from(dst);
            if local.is_dir() {
                local.push(path.rsplit('/').next().unwrap_or("file"));
            }
            std::fs::write(&local, &bytes)
                .map_err(|e| Error::Invalid(format!("cannot write {}: {e}", local.display())))?;
            eprintln!("{} bytes to {}", bytes.len(), local.display());
            Ok(0)
        }
        (None, Some((inst, path))) => {
            let bytes = std::fs::read(src)
                .map_err(|e| Error::Invalid(format!("cannot read {src}: {e}")))?;
            let mut path = path.to_string();
            if path.ends_with('/') {
                path.push_str(
                    std::path::Path::new(src)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("file"),
                );
            }
            let mut a = json!({
                "name": inst, "path": path,
                "content": isb::rpc::b64_encode(&bytes), "encoding": "base64",
            });
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(m) = std::fs::metadata(src) {
                    a["mode"] = json!(format!("{:04o}", m.permissions().mode() & 0o7777));
                }
            }
            let r = call("instance_file_write", with_org(org, a), SHORT)?;
            eprintln!(
                "{} bytes to {inst}:{}",
                r["bytes"],
                r["path"].as_str().unwrap_or("")
            );
            Ok(0)
        }
    }
}
