//! `isb rpc`: the protocol the language SDKs speak.
//!
//! Line-delimited JSON over stdin/stdout, one object per line, documented in
//! docs/rpc.md. The server announces itself first, then answers requests:
//!
//! ```text
//! <- {"isb":"0.1.1","protocol":1}
//! -> {"id":1,"method":"sandbox.get","params":{"name":"web"}}
//! <- {"id":1,"result":{...}}
//! ```
//!
//! Requests run concurrently, each on its own thread, so a long `exec` or `up`
//! never blocks another request. A streaming exec sends `event` lines (output,
//! base64) before its result, and accepts `exec.write`/`exec.signal`/... calls
//! naming its request id while it runs. When stdin closes, the server exits;
//! commands still running then lose their control connection and incus kills
//! them.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::compose::{self, LoadOptions};
use crate::error::{Error, Result};
use crate::exec::{self, ExecController, ExecEvent, ExecOptions, Stdin};
use crate::plan::{DiffOptions, VolumeDefs};
use crate::sandbox::{self, EnsureOptions, LabelFilter, Sandbox};
use crate::spec::{ExecDefaults, PortSpec, ReadyCheck, SandboxSpec};
use crate::{Client, flex};

/// Protocol version. Bumped on incompatible changes.
pub const PROTOCOL: u32 = 1;

type Out = Arc<Mutex<Box<dyn Write + Send>>>;

struct Server {
    out: Out,
    client: Client,
    execs: Mutex<HashMap<String, ExecController>>,
}

#[derive(Deserialize)]
struct Request {
    #[serde(default)]
    id: Value,
    method: String,
    #[serde(default)]
    params: Value,
}

/// Serve requests from `input` until it closes, writing to `output`.
pub fn serve<R: BufRead, W: Write + Send + 'static>(
    input: R,
    output: W,
    client: Client,
) -> Result<()> {
    let server = Arc::new(Server {
        out: Arc::new(Mutex::new(Box::new(output))),
        client,
        execs: Mutex::new(HashMap::new()),
    });
    server.send(&json!({"isb": env!("CARGO_PKG_VERSION"), "protocol": PROTOCOL}));
    let mut workers = Vec::new();
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let req: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                server.send(&json!({"id": null, "error": {"code": "bad_request", "message": format!("not a request: {e}")}}));
                continue;
            }
        };
        // Stream control must never queue behind the stream it controls.
        if req.method.starts_with("exec.") {
            server.clone().handle(req);
            continue;
        }
        let s = server.clone();
        workers.push(std::thread::spawn(move || s.handle(req)));
        workers.retain(|w| !w.is_finished());
    }
    // Input closed: let in-flight requests that do not depend on the client
    // finish writing, briefly, then exit.
    for w in workers {
        let _ = w.join();
    }
    Ok(())
}

impl Server {
    fn send(&self, v: &Value) {
        let mut o = self.out.lock().unwrap_or_else(|p| p.into_inner());
        let _ = writeln!(o, "{v}");
        let _ = o.flush();
    }

    fn event(&self, id: &Value, event: &str, data: Value) {
        self.send(&json!({"id": id, "event": event, "data": data}));
    }

    fn handle(self: Arc<Self>, req: Request) {
        let id = req.id.clone();
        let r = self.dispatch(&req);
        match r {
            Ok(v) => self.send(&json!({"id": id, "result": v})),
            Err(e) => self.send(&json!({"id": id, "error": error_json(&e)})),
        }
    }

    fn client(&self, project: &Option<String>) -> Client {
        match project {
            Some(p) => self.client.clone().project(p),
            None => self.client.clone(),
        }
    }

    fn dispatch(&self, req: &Request) -> Result<Value> {
        let id = &req.id;
        let p = &req.params;
        let mut progress = |line: &str| self.event(id, "progress", json!(line));
        match req.method.as_str() {
            "version" => Ok(json!({"isb": env!("CARGO_PKG_VERSION"), "protocol": PROTOCOL})),
            "schema" => Ok(crate::spec::compose_schema()),

            "sandbox.create" => {
                let a: SpecParams = params(p)?;
                let c = self.client(&a.project);
                let opts = EnsureOptions {
                    wait_ready: a.wait_ready,
                    ..Default::default()
                };
                let d = resolve_spec(&c, &a)?;
                let sb = create_desired(&c, &d, opts, &mut progress)?;
                Ok(json!(sb.info()?))
            }
            "sandbox.ensure" => {
                let a: SpecParams = params(p)?;
                let c = self.client(&a.project);
                let opts = EnsureOptions {
                    wait_ready: a.wait_ready,
                    diff: DiffOptions {
                        prune_devices: a.prune_devices,
                    },
                    ..Default::default()
                };
                let d = resolve_spec(&c, &a)?;
                let report = sandbox::ensure(&c, &d, opts, &mut progress)?;
                let info = Sandbox::get(&c, &d.name)?.info()?;
                Ok(json!({"info": info, "report": report}))
            }
            "sandbox.plan" => {
                let a: SpecParams = params(p)?;
                let c = self.client(&a.project);
                let d = resolve_spec(&c, &a)?;
                Ok(json!(sandbox::plan_desired(
                    &c,
                    &d,
                    DiffOptions {
                        prune_devices: a.prune_devices
                    }
                )?))
            }
            "sandbox.resolve" => {
                let a: SpecParams = params(p)?;
                let c = self.client(&a.project);
                Ok(json!(resolve_spec(&c, &a)?))
            }
            "sandbox.get" => {
                let a: NameParams = params(p)?;
                Ok(json!(
                    Sandbox::get(&self.client(&a.project), &a.name)?.info()?
                ))
            }
            "sandbox.list" => {
                let a: ListParams = params(p)?;
                let filters: Vec<LabelFilter> =
                    a.labels.iter().map(|l| LabelFilter::parse(l)).collect();
                Ok(json!(Sandbox::list_with(
                    &self.client(&a.project),
                    &filters
                )?))
            }
            "sandbox.remove" => {
                let a: RemoveParams = params(p)?;
                Sandbox::remove(&self.client(&a.project), &a.name, a.force)?;
                Ok(Value::Null)
            }
            "sandbox.start" => {
                let a: NameParams = params(p)?;
                Sandbox::get(&self.client(&a.project), &a.name)?.start()?;
                Ok(Value::Null)
            }
            "sandbox.stop" => {
                let a: StopParams = params(p)?;
                let t = match &a.timeout {
                    Some(s) => flex::parse_duration(s).map_err(Error::Invalid)?,
                    None => Duration::from_secs(30),
                };
                Sandbox::get(&self.client(&a.project), &a.name)?.stop(a.force, t)?;
                Ok(Value::Null)
            }
            "sandbox.restart" => {
                let a: NameParams = params(p)?;
                Sandbox::get(&self.client(&a.project), &a.name)?.restart()?;
                Ok(Value::Null)
            }
            "sandbox.wait_ready" => {
                let a: ReadyParams = params(p)?;
                let t = match &a.ready_timeout {
                    Some(s) => flex::parse_duration(s).map_err(Error::Invalid)?,
                    None => Duration::from_secs(60),
                };
                let checks = a.ready.unwrap_or_else(|| vec![ReadyCheck::Running]);
                sandbox::wait_ready(&self.client(&a.project), &a.name, &checks, t, &a.exec)?;
                Ok(Value::Null)
            }
            "sandbox.exec" => self.exec(id, p),
            "exec.write" => {
                let a: ExecWriteParams = params(p)?;
                let data = b64_decode(&a.data)?;
                self.controller(&a.exec)?.write_stdin(&data)?;
                Ok(Value::Null)
            }
            "exec.close_stdin" => {
                let a: ExecRef = params(p)?;
                self.controller(&a.exec)?.close_stdin()?;
                Ok(Value::Null)
            }
            "exec.signal" => {
                let a: ExecSignalParams = params(p)?;
                self.controller(&a.exec)?.signal(a.signal)?;
                Ok(Value::Null)
            }
            "exec.resize" => {
                let a: ExecResizeParams = params(p)?;
                self.controller(&a.exec)?.resize(a.width, a.height)?;
                Ok(Value::Null)
            }
            "sandbox.add_port" => {
                let a: AddPortParams = params(p)?;
                let listen = Sandbox::get(&self.client(&a.project), &a.name)?.add_port(&a.port)?;
                Ok(json!({"listen": listen}))
            }
            "sandbox.remove_device" => {
                let a: DeviceParams = params(p)?;
                let removed =
                    Sandbox::get(&self.client(&a.project), &a.name)?.remove_device(&a.device)?;
                Ok(json!({"removed": removed}))
            }

            "volume.list" => {
                let a: PoolParams = params(p)?;
                let c = self.client(&a.project);
                let pools = match a.pool {
                    Some(p) => vec![p],
                    None => sandbox::host_facts(&c)?.pools,
                };
                let mut all = Vec::new();
                for pool in pools {
                    all.extend(crate::volume::list(&c, &pool)?);
                }
                Ok(json!(all))
            }
            "volume.get" => {
                let a: VolumeParams = params(p)?;
                let c = self.client(&a.project);
                let pool = sandbox::host_facts(&c)?.pick_pool(a.pool.as_deref())?;
                crate::volume::get(&c, &pool, &a.name)?
                    .map(|v| json!(v))
                    .ok_or_else(|| Error::NotFound(format!("volume {} in pool {pool}", a.name)))
            }
            "volume.create" => {
                let a: VolumeParams = params(p)?;
                let c = self.client(&a.project);
                let pool = sandbox::host_facts(&c)?.pick_pool(a.pool.as_deref())?;
                let created = crate::volume::ensure(&c, &pool, &a.name, &a.config)?;
                Ok(json!({"created": created, "pool": pool}))
            }
            "volume.remove" => {
                let a: VolumeParams = params(p)?;
                let c = self.client(&a.project);
                let pool = sandbox::host_facts(&c)?.pick_pool(a.pool.as_deref())?;
                crate::volume::remove(&c, &pool, &a.name)?;
                Ok(Value::Null)
            }
            "prune" => {
                let a: PruneParams = params(p)?;
                Ok(json!(sandbox::prune_missing_path(
                    &self.client(&a.project),
                    &a.label,
                    a.dry_run,
                    &mut progress
                )?))
            }

            "compose.load" => {
                let a: ComposeParams = params(p)?;
                let proj = compose::load(&a.load())?;
                Ok(
                    json!({"name": proj.name, "base_dir": proj.base_dir, "files": proj.files, "file": proj.file}),
                )
            }
            "compose.up" => {
                let a: ComposeParams = params(p)?;
                let proj = compose::load(&a.load())?;
                let opts = EnsureOptions {
                    wait_ready: a.wait_ready,
                    diff: DiffOptions {
                        prune_devices: a.prune_devices,
                    },
                    ..Default::default()
                };
                let out = compose::up(
                    &self.client(&a.project),
                    &proj,
                    &a.services,
                    opts,
                    &mut progress,
                )?;
                Ok(json!(
                    out.into_iter()
                        .map(|(s, r)| json!({"service": s, "report": r}))
                        .collect::<Vec<_>>()
                ))
            }
            "compose.plan" => {
                let a: ComposeParams = params(p)?;
                let proj = compose::load(&a.load())?;
                Ok(json!(compose::plan(
                    &self.client(&a.project),
                    &proj,
                    &a.services,
                    DiffOptions {
                        prune_devices: a.prune_devices
                    }
                )?))
            }
            "compose.down" => {
                let a: ComposeParams = params(p)?;
                let proj = compose::load(&a.load())?;
                compose::down(
                    &self.client(&a.project),
                    &proj,
                    &a.services,
                    a.volumes,
                    &mut progress,
                )?;
                Ok(Value::Null)
            }
            other => Err(Error::Protocol(format!("unknown method {other:?}"))),
        }
    }

    fn controller(&self, exec: &Value) -> Result<ExecController> {
        let key = exec_key(exec);
        self.execs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&key)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("running exec {key}")))
    }

    fn exec(&self, id: &Value, p: &Value) -> Result<Value> {
        let a: ExecParams = params(p)?;
        let c = self.client(&a.project);
        let stdin = match &a.stdin {
            None => Stdin::Null,
            Some(StdinParam::Mode(m)) if m == "null" => Stdin::Null,
            Some(StdinParam::Mode(m)) if m == "piped" => Stdin::Piped,
            Some(StdinParam::Mode(m)) => {
                return Err(Error::invalid(format!(
                    "stdin must be \"null\", \"piped\" or {{\"data\": base64}}, got {m:?}"
                )));
            }
            Some(StdinParam::Data { data }) => Stdin::Bytes(b64_decode(data)?),
        };
        let timeout = match &a.timeout {
            Some(t) => Some(flex::parse_duration(t).map_err(Error::Invalid)?),
            None => None,
        };
        let opts = ExecOptions {
            cwd: a.cwd.clone(),
            user: a.user.clone(),
            env: a.env.clone(),
            login: a.login,
            tty: a.tty,
            width: a.width,
            height: a.height,
            timeout,
            stdin: stdin.clone(),
        };
        let req = exec::build_request(&c, &a.name, &a.argv, &a.defaults, &opts)?;
        let mut stream = exec::start_with_timeout(&c, &a.name, req, stdin, timeout)?;
        let key = exec_key(id);
        self.execs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(key.clone(), stream.controller());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        while let Some(ev) = stream.next_event() {
            match (ev, a.stream) {
                (ExecEvent::Stdout(b), true) => self.event(id, "stdout", json!(b64_encode(&b))),
                (ExecEvent::Stderr(b), true) => self.event(id, "stderr", json!(b64_encode(&b))),
                (ExecEvent::Stdout(b), false) => stdout.extend(b),
                (ExecEvent::Stderr(b), false) => stderr.extend(b),
            }
        }
        let code = stream.wait();
        self.execs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&key);
        let code = code?;
        if a.stream {
            Ok(json!({"exit_code": code}))
        } else {
            Ok(
                json!({"exit_code": code, "stdout": b64_encode(&stdout), "stderr": b64_encode(&stderr)}),
            )
        }
    }
}

fn exec_key(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn params<T: DeserializeOwned>(p: &Value) -> Result<T> {
    let v = if p.is_null() { json!({}) } else { p.clone() };
    serde_json::from_value(v).map_err(|e| Error::Invalid(format!("params: {e}")))
}

fn resolve_spec(c: &Client, a: &SpecParams) -> Result<crate::plan::Desired> {
    let base = match &a.base_dir {
        Some(b) => PathBuf::from(b),
        None => std::env::current_dir()?,
    };
    sandbox::resolve(c, &a.spec, &a.volumes, Path::new(&base))
}

fn create_desired(
    c: &Client,
    d: &crate::plan::Desired,
    opts: EnsureOptions,
    report: &mut dyn FnMut(&str),
) -> Result<Sandbox> {
    let _lock =
        crate::lock::NameLock::acquire(c.project_name(), &d.name, opts.lock_wait, &mut |_| {})?;
    if Sandbox::get(c, &d.name).is_ok() {
        return Err(Error::AlreadyExists(d.name.clone()));
    }
    let plan = sandbox::plan_desired(c, d, opts.diff)?;
    sandbox::apply(c, d, &plan, report)?;
    if opts.wait_ready {
        sandbox::wait_ready(c, &d.name, &d.ready, d.ready_timeout, &d.exec)?;
    }
    Sandbox::get(c, &d.name)
}

/// The error object: a stable `code`, a human `message`, and extra `data`.
pub fn error_json(e: &Error) -> Value {
    let (code, data) = match e {
        Error::Connect { socket, .. } => ("connect", json!({"socket": socket})),
        Error::RequestTimeout {
            method,
            path,
            timeout,
        } => (
            "request_timeout",
            json!({"method": method, "path": path, "timeout_secs": timeout.as_secs_f64()}),
        ),
        Error::Api {
            status,
            method,
            path,
            ..
        } => (
            if *status == 404 {
                "not_found"
            } else if e.is_conflict() {
                "already_exists"
            } else {
                "api"
            },
            json!({"status": status, "method": method, "path": path}),
        ),
        Error::OperationTimeout {
            step,
            operation,
            cancelled,
            waited,
            ..
        } => (
            "operation_timeout",
            json!({"step": step, "operation": operation, "cancelled": cancelled, "waited_secs": waited.as_secs_f64()}),
        ),
        Error::OperationFailed { step, .. } => ("operation_failed", json!({"step": step})),
        Error::NotReady {
            sandbox,
            check,
            detail,
            waited,
        } => (
            "not_ready",
            json!({"sandbox": sandbox, "check": check, "detail": detail, "waited_secs": waited.as_secs_f64()}),
        ),
        Error::ExecTimeout { timeout, .. } => (
            "exec_timeout",
            json!({"timeout_secs": timeout.as_secs_f64()}),
        ),
        Error::NotFound(_) => ("not_found", Value::Null),
        Error::AlreadyExists(_) => ("already_exists", Value::Null),
        Error::Invalid(_) => ("invalid", Value::Null),
        Error::Interpolation(_) => ("interpolation", Value::Null),
        Error::Parse { path, .. } => ("parse", json!({"path": path})),
        Error::WebSocket(_) => ("websocket", Value::Null),
        Error::Protocol(_) => ("protocol", Value::Null),
        Error::Io(_) => ("io", Value::Null),
        Error::Json(_) => ("json", Value::Null),
    };
    let mut o = json!({"code": code, "message": e.to_string()});
    if !data.is_null() {
        o["data"] = data;
    }
    o
}

fn yes() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SpecParams {
    spec: SandboxSpec,
    #[serde(default)]
    base_dir: Option<String>,
    #[serde(default)]
    volumes: VolumeDefs,
    #[serde(default = "yes")]
    wait_ready: bool,
    #[serde(default)]
    prune_devices: bool,
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NameParams {
    name: String,
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListParams {
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveParams {
    name: String,
    #[serde(default)]
    force: bool,
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StopParams {
    name: String,
    #[serde(default)]
    force: bool,
    #[serde(default)]
    timeout: Option<String>,
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadyParams {
    name: String,
    #[serde(default)]
    ready: Option<Vec<ReadyCheck>>,
    #[serde(default)]
    ready_timeout: Option<String>,
    #[serde(default)]
    exec: ExecDefaults,
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StdinParam {
    Mode(String),
    Data { data: String },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecParams {
    name: String,
    argv: Vec<String>,
    #[serde(default)]
    defaults: ExecDefaults,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    login: Option<bool>,
    #[serde(default)]
    tty: bool,
    #[serde(default)]
    width: Option<u16>,
    #[serde(default)]
    height: Option<u16>,
    /// `"90s"`, `"5m"` or seconds as a string.
    #[serde(default)]
    timeout: Option<String>,
    #[serde(default)]
    stdin: Option<StdinParam>,
    /// Send output as events instead of collecting it into the result.
    #[serde(default)]
    stream: bool,
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecRef {
    exec: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecWriteParams {
    exec: Value,
    data: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecSignalParams {
    exec: Value,
    signal: i32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecResizeParams {
    exec: Value,
    width: u16,
    height: u16,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddPortParams {
    name: String,
    port: PortSpec,
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeviceParams {
    name: String,
    device: String,
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PoolParams {
    #[serde(default)]
    pool: Option<String>,
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VolumeParams {
    name: String,
    #[serde(default)]
    pool: Option<String>,
    #[serde(default)]
    config: BTreeMap<String, String>,
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PruneParams {
    label: String,
    #[serde(default = "yes")]
    dry_run: bool,
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ComposeParams {
    #[serde(default)]
    files: Vec<PathBuf>,
    #[serde(default)]
    env_files: Vec<PathBuf>,
    #[serde(default)]
    project_name: Option<String>,
    #[serde(default)]
    vars: BTreeMap<String, String>,
    #[serde(default)]
    services: Vec<String>,
    #[serde(default = "yes")]
    wait_ready: bool,
    #[serde(default)]
    prune_devices: bool,
    #[serde(default)]
    volumes: bool,
    #[serde(default)]
    project: Option<String>,
}

impl ComposeParams {
    fn load(&self) -> LoadOptions {
        LoadOptions {
            files: self.files.clone(),
            env_files: self.env_files.clone(),
            project_name: self.project_name.clone(),
            vars: self.vars.clone(),
        }
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding.
pub fn b64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            B64[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Decode standard base64 (padding optional).
pub fn b64_decode(s: &str) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            b'\n' | b'\r' | b' ' => continue,
            _ => return Err(Error::invalid("invalid base64")),
        };
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[derive(Clone)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().write(b)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn run(input: &str) -> Vec<Value> {
        let buf = Buf(Arc::new(Mutex::new(Vec::new())));
        let client = Client::with_socket("/nonexistent/isb-test.socket");
        serve(Cursor::new(input.to_string()), buf.clone(), client).unwrap();
        let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        out.lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn by_id(msgs: &[Value], id: i64) -> Value {
        msgs.iter()
            .find(|m| {
                m["id"] == json!(id) && (m.get("result").is_some() || m.get("error").is_some())
            })
            .cloned()
            .unwrap_or_else(|| panic!("no reply for {id}: {msgs:?}"))
    }

    #[test]
    fn hello_version_schema_and_errors() {
        let msgs = run(concat!(
            "{\"id\":1,\"method\":\"version\"}\n",
            "{\"id\":2,\"method\":\"schema\"}\n",
            "not json\n",
            "{\"id\":3,\"method\":\"nope\"}\n",
            "{\"id\":4,\"method\":\"sandbox.get\",\"params\":{\"nam\":\"x\"}}\n",
            "{\"id\":5,\"method\":\"sandbox.get\",\"params\":{\"name\":\"x\"}}\n",
            "{\"id\":6,\"method\":\"exec.signal\",\"params\":{\"exec\":99,\"signal\":2}}\n",
        ));
        assert_eq!(msgs[0]["protocol"], json!(PROTOCOL));
        assert_eq!(by_id(&msgs, 1)["result"]["protocol"], json!(PROTOCOL));
        assert!(by_id(&msgs, 2)["result"].to_string().contains("sandboxes"));
        assert!(msgs.iter().any(|m| m["error"]["code"] == "bad_request"));
        assert_eq!(by_id(&msgs, 3)["error"]["code"], "protocol");
        assert_eq!(by_id(&msgs, 4)["error"]["code"], "invalid");
        let e = by_id(&msgs, 5);
        assert_eq!(e["error"]["code"], "connect");
        assert_eq!(e["error"]["data"]["socket"], "/nonexistent/isb-test.socket");
        assert_eq!(by_id(&msgs, 6)["error"]["code"], "not_found");
    }

    #[test]
    fn compose_load_over_rpc() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("isb.yaml");
        std::fs::write(&f, "sandboxes:\n  web: {image: \"${IMG}\", cpus: 2}\n").unwrap();
        let req = json!({"id": 1, "method": "compose.load", "params": {"files": [f], "vars": {"IMG": "dev-base"}, "project_name": "demo"}});
        let msgs = run(&format!("{req}\n"));
        let r = &by_id(&msgs, 1)["result"];
        assert_eq!(r["name"], "demo");
        assert_eq!(r["file"]["sandboxes"]["web"]["image"], "dev-base");
        assert_eq!(r["file"]["sandboxes"]["web"]["name"], "demo-web");
    }

    #[test]
    fn base64_roundtrip() {
        for s in [
            &b""[..],
            b"f",
            b"fo",
            b"foo",
            b"foob",
            b"fooba",
            b"foobar",
            &[0, 255, 128, 7],
        ] {
            assert_eq!(b64_decode(&b64_encode(s)).unwrap(), s);
        }
        assert_eq!(b64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(b64_encode(b"fo"), "Zm8=");
        assert!(b64_decode("@@").is_err());
    }
}
