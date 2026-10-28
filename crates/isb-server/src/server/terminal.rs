//! A terminal over a websocket: `GET /orgs/<org>/api/v1/terminal?app=NAME`
//! (or `?stack=NAME&service=SERVICE`, or `?instance=NAME`) upgrades to a
//! websocket bridged to a pseudo-terminal the embedder opens (`isb serve`: a
//! shell in one of the app's or service's replicas, or in an instance of the
//! org). With `argv=` (repeated, in order) it runs that command instead of a
//! shell, with `user=`, `cwd=` and `env=KEY=VALUE` (repeated): `isb stack
//! exec -t` and its siblings.
//!
//! The gate is the REST surface's: the caller authenticates as for any tool
//! (session, API token, Access), and is admitted as if calling
//! `sandbox_exec` in the org (a command: the exec tool that runs it without
//! a terminal), so the org rules and `--deny-tools` apply.
//! A browser's cookie rides along on a cross-site websocket too, so a
//! cookie-authenticated upgrade must come from this site (`Origin` naming
//! the request's `Host`).
//!
//! On the wire: binary frames are terminal bytes both ways; text frames are
//! JSON control messages: from the browser `{"type": "resize", "cols",
//! "rows"}`, from the server `{"type": "exit", "code"}` and `{"type":
//! "error", "message"}`.
//!
//! Bounded: at most [`MAX_SESSIONS`] at once, [`IDLE`] without a byte either
//! way, [`MAX_AGE`] in all, messages up to [`MAX_MESSAGE`].

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::json;
use tungstenite::protocol::{CloseFrame, Role, WebSocketConfig, frame::coding::CloseCode};
use tungstenite::{Message, WebSocket};

use super::http::{Duplex, Request, Response};
use super::mcp::Caller;

pub const MAX_SESSIONS: usize = 16;
pub const IDLE: Duration = Duration::from_secs(30 * 60);
pub const MAX_AGE: Duration = Duration::from_secs(8 * 3600);
pub const MAX_MESSAGE: usize = 64 * 1024;
/// How long one turn of the bridge waits for the browser before polling the
/// terminal's output.
const POLL: Duration = Duration::from_millis(15);

/// The most arguments and environment variables a command may have (as
/// the exec tools take).
const MAX_ARGV: usize = 1024;
const MAX_ENV: usize = 128;

/// What the browser (or `isb ... exec -t`) asked for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TermRequest {
    /// The app; with `stack`, that stack's service; empty when `instance`
    /// names the target.
    pub app: String,
    /// The compose stack `app` is a service of.
    pub stack: Option<String>,
    /// A replica's slot; `None` picks one.
    pub slot: Option<u32>,
    /// An instance of the org (a sandbox, a replica) instead of an app.
    pub instance: Option<String>,
    pub cols: u16,
    pub rows: u16,
    /// Run this instead of a login shell.
    pub command: Option<TermCommand>,
}

/// A command to run on the terminal, as the exec tools take one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TermCommand {
    pub argv: Vec<String>,
    pub user: Option<String>,
    pub cwd: Option<String>,
    pub env: BTreeMap<String, String>,
}

impl TermRequest {
    /// The query string that asks for this terminal again (forwarding, and
    /// the CLI's `exec -t`).
    pub fn query(&self) -> String {
        let mut q = match (&self.instance, &self.stack) {
            (Some(i), _) => format!("instance={i}"),
            (None, Some(s)) => format!("stack={s}&service={}", encode(&self.app)),
            (None, None) => format!("app={}", self.app),
        };
        q.push_str(&format!("&cols={}&rows={}", self.cols, self.rows));
        if let Some(n) = self.slot {
            q.push_str(&format!("&slot={n}"));
        }
        if let Some(c) = &self.command {
            for a in &c.argv {
                q.push_str(&format!("&argv={}", encode(a)));
            }
            if let Some(u) = &c.user {
                q.push_str(&format!("&user={}", encode(u)));
            }
            if let Some(w) = &c.cwd {
                q.push_str(&format!("&cwd={}", encode(w)));
            }
            for (k, v) in &c.env {
                q.push_str(&format!("&env={}", encode(&format!("{k}={v}"))));
            }
        }
        q
    }

    /// What it opens on, for logs: the app, the stack's service or the
    /// instance.
    pub fn target(&self) -> String {
        match (&self.instance, &self.stack) {
            (Some(i), _) => i.clone(),
            (None, Some(s)) => format!("{s}/{}", self.app),
            (None, None) => self.app.clone(),
        }
    }

    /// What the audit log keeps of the request: the target, and a
    /// command's argv, user and cwd, as an exec tool's call is kept, with
    /// its environment's names only.
    pub fn audit_args(&self, org: &crate::org::OrgId) -> serde_json::Value {
        let mut args = match (&self.instance, &self.stack) {
            (Some(i), _) => json!({"org": org.as_str(), "name": i}),
            (None, Some(s)) => json!({"org": org.as_str(), "stack": s, "service": self.app}),
            (None, None) => json!({"org": org.as_str(), "app": self.app}),
        };
        if let Some(c) = &self.command {
            args["argv"] = json!(c.argv);
            args["user"] = json!(c.user);
            args["cwd"] = json!(c.cwd);
            args["env"] = json!(c.env.keys().collect::<Vec<_>>());
        }
        args
    }

    /// The tool whose admission a session of this kind takes: a shell is
    /// `sandbox_exec`'s; a command, that of the exec tool that would run it
    /// without a terminal.
    pub fn tool(&self) -> &'static str {
        match (&self.command, &self.instance, &self.stack) {
            (None, _, _) => "sandbox_exec",
            (Some(_), Some(_), _) => "instance_exec",
            (Some(_), None, Some(_)) => "stack_exec",
            (Some(_), None, None) => "app_exec",
        }
    }
}

/// How many sessions of one kind may be open at once, and for how long.
#[derive(Debug)]
pub struct Limits {
    pub active: &'static AtomicUsize,
    pub max_sessions: usize,
    pub idle: Duration,
    pub max_age: Duration,
    /// Said when `max_sessions` are open.
    pub busy: &'static str,
}

/// The web terminal's limits.
pub static TERMINALS: Limits = Limits {
    active: &ACTIVE,
    max_sessions: MAX_SESSIONS,
    idle: IDLE,
    max_age: MAX_AGE,
    busy: "too many terminals are open on this server; close one and try again",
};

/// One terminal session's output, polled.
#[derive(Debug, PartialEq, Eq)]
pub enum PtyOutput {
    Data(Vec<u8>),
    /// Nothing within the wait.
    Idle,
    /// The program ended (with its exit code, when known).
    Exit(Option<i32>),
    /// It could not run, or the exec broke: shown to the person, then closed.
    Failed(String),
}

/// A pseudo-terminal the bridge drives.
pub trait Pty: Send {
    fn input(&mut self, data: &[u8]) -> crate::Result<()>;
    fn resize(&mut self, cols: u16, rows: u16);
    fn output(&mut self, wait: Duration) -> PtyOutput;
    /// End the program; the session is over.
    fn close(&mut self);
    /// What it runs in (an instance name), for the audit log.
    fn target(&self) -> Option<String> {
        None
    }
    /// What it learnt while running that the audit log's closing row should
    /// keep (an SSH session's user and key fingerprint). Never content.
    fn details(&self) -> Option<serde_json::Map<String, serde_json::Value>> {
        None
    }
}

/// Opens a terminal for `caller` in `org`; refusals become an error frame.
pub type Terminal = Arc<
    dyn Fn(&Caller, &crate::org::OrgId, &TermRequest) -> crate::Result<Box<dyn Pty>> + Send + Sync,
>;

static ACTIVE: AtomicUsize = AtomicUsize::new(0);

struct Slot(&'static AtomicUsize);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn param(req: &Request, key: &str) -> Option<String> {
    params(req, key).into_iter().next()
}

/// Every raw value of a repeated query parameter, in order.
fn params(req: &Request, key: &str) -> Vec<String> {
    let Some(q) = req.query.as_deref() else {
        return Vec::new();
    };
    q.split('&')
        .filter_map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            (k == key).then(|| v.to_string())
        })
        .collect()
}

/// Percent-encode a query value.
pub(crate) fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'@' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Undo [`encode`] (and a form's `+`); `None` when it is not UTF-8 or a
/// `%` is not followed by two hex digits.
pub(crate) fn decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                let h = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
                out.push(u8::from_str_radix(h, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// A compose service's name: letters, digits, `.`, `_` and `-`, at most 64.
fn service_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// The command a request carries in `argv=` (with `user=`, `cwd=`, `env=`),
/// if any.
fn term_command(req: &Request) -> std::result::Result<Option<TermCommand>, String> {
    let all = |k: &str| -> std::result::Result<Vec<String>, String> {
        params(req, k)
            .iter()
            .map(|v| decode(v).ok_or_else(|| format!("{k}= is not percent-encoded UTF-8")))
            .collect()
    };
    let one = |k: &str| -> std::result::Result<Option<String>, String> {
        Ok(all(k)?.into_iter().next().filter(|v| !v.is_empty()))
    };
    let argv = all("argv")?;
    if argv.is_empty() {
        for k in ["user", "cwd", "env"] {
            if param(req, k).is_some() {
                return Err(format!("{k}= is for a command (argv=)"));
            }
        }
        return Ok(None);
    }
    if argv[0].is_empty() || argv.len() > MAX_ARGV || argv.iter().any(|a| a.contains('\0')) {
        return Err(format!(
            "argv=: a command, at most {MAX_ARGV} arguments, none with a NUL"
        ));
    }
    let mut env = BTreeMap::new();
    for e in all("env")? {
        match e.split_once('=') {
            Some((k, v)) if !k.is_empty() && !k.contains('\0') && !v.contains('\0') => {
                env.insert(k.to_string(), v.to_string());
            }
            _ => return Err(format!("env= wants KEY=VALUE (it was {e:?})")),
        }
    }
    if env.len() > MAX_ENV {
        return Err(format!("env=: at most {MAX_ENV} variables"));
    }
    Ok(Some(TermCommand {
        argv,
        user: one("user")?,
        cwd: one("cwd")?,
        env,
    }))
}

/// A lower-case name of letters, digits and dashes, at most 64 long (an
/// app's or an instance's).
pub fn plain_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A query parameter's raw value.
pub fn query_param(req: &Request, key: &str) -> Option<String> {
    param(req, key)
}

/// The request's terminal parameters, or what is wrong with them.
pub fn term_request(req: &Request) -> std::result::Result<TermRequest, String> {
    let service = match param(req, "service") {
        Some(s) => Some(
            decode(&s)
                .filter(|s| service_name(s))
                .ok_or("service= is not a service name")?,
        ),
        None => None,
    };
    let (app, stack, instance) = match (
        param(req, "app"),
        param(req, "stack"),
        param(req, "instance"),
    ) {
        (Some(a), None, None) if plain_name(&a) => (a, None, None),
        (Some(_), None, None) => return Err("app= is not an app name".into()),
        (None, Some(s), None) if plain_name(&s) => (
            service.clone().ok_or("stack= needs service=")?,
            Some(s),
            None,
        ),
        (None, Some(_), None) => return Err("stack= is not a stack name".into()),
        (None, None, Some(i)) if plain_name(&i) => (String::new(), None, Some(i)),
        (None, None, Some(_)) => return Err("instance= is not an instance name".into()),
        (None, None, None) => return Err("app= (or stack= or instance=) is required".into()),
        _ => return Err("give one of app=, stack= or instance=".into()),
    };
    if service.is_some() && stack.is_none() {
        return Err("service= is for stack=".into());
    }
    let num = |k: &str, lo: u32, hi: u32, def: u32| -> std::result::Result<u32, String> {
        match param(req, k) {
            None => Ok(def),
            Some(v) => v
                .parse::<u32>()
                .ok()
                .filter(|n| (lo..=hi).contains(n))
                .ok_or_else(|| format!("{k}= must be {lo}-{hi}")),
        }
    };
    let slot = match param(req, "slot") {
        None => None,
        Some(_) if instance.is_some() => return Err("slot= is for apps and stacks".into()),
        Some(_) => Some(num("slot", 1, 1000, 1)?),
    };
    Ok(TermRequest {
        app,
        stack,
        slot,
        instance,
        cols: num("cols", 2, 1000, 80)? as u16,
        rows: num("rows", 2, 1000, 24)? as u16,
        command: term_command(req)?,
    })
}

/// `Origin`'s host and port, when it is an http(s) origin.
fn origin_authority(origin: &str) -> Option<&str> {
    let rest = origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"))?;
    (!rest.is_empty() && !rest.contains('/')).then_some(rest)
}

/// A cookie-authenticated upgrade must come from a page on this site. A
/// bearer token is not sent by browsers on their own, nor is the unix
/// socket reached by one (`isb ... exec -t` on the daemon's host), so
/// neither needs an Origin; an Origin that is there must name this site.
pub fn origin_allowed(req: &Request) -> bool {
    let bearer = req.header("authorization").is_some();
    match req.header("origin") {
        None => bearer || matches!(req.peer, super::http::Peer::Unix { .. }),
        Some(o) => match (origin_authority(o), req.header("host")) {
            (Some(a), Some(h)) => a.eq_ignore_ascii_case(h.trim()),
            _ => false,
        },
    }
}

/// The client's websocket key, if this is a websocket upgrade request.
pub fn websocket_key(req: &Request) -> Option<String> {
    let has = |name: &str, token: &str| {
        req.header(name)
            .is_some_and(|v| v.split(',').any(|t| t.trim().eq_ignore_ascii_case(token)))
    };
    if req.method != "GET" || !has("upgrade", "websocket") || !has("connection", "upgrade") {
        return None;
    }
    if req.header("sec-websocket-version").map(str::trim) != Some("13") {
        return None;
    }
    let key = req.header("sec-websocket-key")?.trim();
    (key.len() == 24).then(|| key.to_string())
}

/// The 101 that hands the connection to a session opened by `open`. The
/// opening happens after the upgrade, so its errors reach the browser as an
/// error frame rather than a bare failed handshake.
pub fn upgrade<F>(key: &str, open: F) -> Response
where
    F: FnOnce() -> crate::Result<Box<dyn Pty>> + Send + 'static,
{
    upgrade_with(key, &TERMINALS, open)
}

/// [`upgrade`] under `limits` (the SSH bridge has its own).
pub fn upgrade_with<F>(key: &str, limits: &'static Limits, open: F) -> Response
where
    F: FnOnce() -> crate::Result<Box<dyn Pty>> + Send + 'static,
{
    let accept = tungstenite::handshake::derive_accept_key(key.as_bytes());
    Response::upgrade(
        "websocket",
        Box::new(move |s: &mut dyn Duplex| {
            let mut ws = WebSocket::from_raw_socket(
                s,
                Role::Server,
                Some(
                    WebSocketConfig::default()
                        .max_message_size(Some(MAX_MESSAGE))
                        .max_frame_size(Some(MAX_MESSAGE)),
                ),
            );
            if limits.active.fetch_add(1, Ordering::SeqCst) >= limits.max_sessions {
                limits.active.fetch_sub(1, Ordering::SeqCst);
                refuse(&mut ws, limits.busy);
                return;
            }
            let _slot = Slot(limits.active);
            match open() {
                Ok(pty) => bridge(&mut ws, pty, limits.idle, limits.max_age),
                Err(e) => refuse(&mut ws, &e.to_string()),
            }
        }),
    )
    .header("Sec-WebSocket-Accept", accept)
}

fn control(v: serde_json::Value) -> Message {
    Message::text(v.to_string())
}

fn refuse<S: std::io::Read + std::io::Write>(ws: &mut WebSocket<S>, message: &str) {
    let _ = ws.send(control(json!({"type": "error", "message": message})));
    let _ = ws.close(Some(CloseFrame {
        code: CloseCode::Policy,
        reason: "refused".into(),
    }));
    let _ = ws.flush();
}

/// Nothing to read yet, or a read cut short by a signal (SIGWINCH in `exec
/// -t`): try again on the next turn.
fn would_block(e: &tungstenite::Error) -> bool {
    use std::io::ErrorKind::{Interrupted, TimedOut, WouldBlock};
    matches!(e, tungstenite::Error::Io(io) if matches!(io.kind(), WouldBlock | TimedOut | Interrupted))
}

/// Shuttle bytes between the websocket and the terminal until either ends,
/// the session idles for `idle`, or it reaches `max_age`.
#[expect(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
pub fn bridge(
    ws: &mut WebSocket<&mut dyn Duplex>,
    mut pty: Box<dyn Pty>,
    idle: Duration,
    max_age: Duration,
) {
    let _ = ws.get_mut().set_read_timeout(Some(POLL));
    let started = Instant::now();
    let mut last = Instant::now();
    let mut why: Option<&str> = None;
    'session: loop {
        if started.elapsed() >= max_age {
            why = Some("the session reached its time limit");
            break;
        }
        if last.elapsed() >= idle {
            why = Some("closed after being idle");
            break;
        }
        match ws.read() {
            Ok(Message::Binary(b)) => {
                last = Instant::now();
                if let Err(e) = pty.input(&b) {
                    let _ = ws.send(control(json!({"type": "error", "message": e.to_string()})));
                    break;
                }
            }
            Ok(Message::Text(t)) => {
                last = Instant::now();
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
                    if v["type"] == "resize" {
                        let n = |k: &str| v[k].as_u64().map(|x| x.clamp(2, 1000) as u16);
                        if let (Some(c), Some(r)) = (n("cols"), n("rows")) {
                            pty.resize(c, r);
                        }
                    }
                }
            }
            Ok(Message::Close(_)) => {
                pty.close();
                return;
            }
            Ok(_) => {}
            Err(e) if would_block(&e) => {}
            Err(_) => {
                pty.close();
                return;
            }
        }
        // Pass on what the terminal wrote, a bounded amount per turn.
        for _ in 0..64 {
            match pty.output(Duration::ZERO) {
                PtyOutput::Data(d) => {
                    last = Instant::now();
                    if let Err(e) = ws.send(Message::binary(d)) {
                        if !would_block(&e) {
                            pty.close();
                            return;
                        }
                    }
                }
                PtyOutput::Idle => break,
                PtyOutput::Failed(m) => {
                    let _ = ws.send(control(json!({"type": "error", "message": m})));
                    let _ = ws.close(Some(CloseFrame {
                        code: CloseCode::Error,
                        reason: "failed".into(),
                    }));
                    let _ = ws.flush();
                    break 'session;
                }
                PtyOutput::Exit(code) => {
                    let _ = ws.send(control(json!({"type": "exit", "code": code})));
                    let _ = ws.close(Some(CloseFrame {
                        code: CloseCode::Normal,
                        reason: "exited".into(),
                    }));
                    let _ = ws.flush();
                    break 'session;
                }
            }
        }
        match ws.flush() {
            Ok(()) => {}
            Err(e) if would_block(&e) => {}
            Err(_) => {
                pty.close();
                return;
            }
        }
    }
    if let Some(w) = why {
        let _ = ws.send(control(json!({"type": "error", "message": w})));
        let _ = ws.close(Some(CloseFrame {
            code: CloseCode::Normal,
            reason: "closed".into(),
        }));
        let _ = ws.flush();
    }
    pty.close();
    // Let the close handshake finish, briefly.
    let until = Instant::now() + Duration::from_millis(500);
    while Instant::now() < until {
        match ws.read() {
            Ok(_) => {}
            Err(e) if would_block(&e) => {}
            Err(_) => break,
        }
    }
}

/// The client's end of a terminal (`isb ... exec -t`): what `input` gives
/// goes out as terminal bytes and what comes back goes to `output`; the
/// window size, `size()`, is sent whenever `resized` is set (on SIGWINCH);
/// `stop` (a signal to this process) ends the session, which kills the
/// program. Returns the program's exit code, `None` when it ended without
/// one.
pub fn attach<S: super::ssh::Conn + ?Sized>(
    ws: &mut WebSocket<Box<S>>,
    input: impl std::io::Read + Send + 'static,
    mut output: impl std::io::Write,
    size: impl Fn() -> Option<(u16, u16)>,
    resized: &AtomicBool,
    stop: &AtomicBool,
) -> crate::Result<Option<i32>> {
    use crate::error::Error;
    let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(64);
    // A thread of its own: a read of a terminal blocks. It is left behind,
    // blocked, when the session ends, and goes with the process.
    std::thread::spawn(move || {
        let mut input = input;
        let mut buf = vec![0u8; 32 * 1024];
        while let Ok(n) = input.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                return;
            }
        }
    });
    let sent = |ws: &mut WebSocket<Box<S>>, m: Message| match ws.send(m) {
        Err(e) if !would_block(&e) => Err(Error::WebSocket(e.to_string())),
        _ => Ok(()),
    };
    ws.get_ref().set_timeout(Some(POLL))?;
    loop {
        if stop.load(Ordering::SeqCst) {
            let _ = ws.close(None);
            let _ = ws.flush();
            return Ok(None);
        }
        if resized.swap(false, Ordering::SeqCst) {
            if let Some((cols, rows)) = size() {
                sent(
                    ws,
                    control(json!({"type": "resize", "cols": cols, "rows": rows})),
                )?;
            }
        }
        match ws.read() {
            Ok(Message::Binary(b)) => {
                output.write_all(&b)?;
                output.flush()?;
            }
            Ok(Message::Text(t)) => {
                let v: serde_json::Value = serde_json::from_str(t.as_str()).unwrap_or_default();
                match v["type"].as_str() {
                    Some("exit") => return Ok(v["code"].as_i64().map(|c| c as i32)),
                    Some("error") => {
                        return Err(Error::Remote {
                            code: "terminal".into(),
                            message: v["message"].as_str().unwrap_or("refused").to_string(),
                            data: serde_json::Value::Null,
                        });
                    }
                    _ => {}
                }
            }
            Ok(Message::Close(_)) => return Ok(None),
            Ok(_) => {}
            Err(e) if would_block(&e) => {}
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                return Ok(None);
            }
            Err(e) => return Err(Error::WebSocket(e.to_string())),
        }
        while let Ok(d) = rx.try_recv() {
            sent(ws, Message::binary(d))?;
        }
        match ws.flush() {
            Err(e) if !would_block(&e) => return Err(Error::WebSocket(e.to_string())),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::http::Peer;
    use std::os::unix::net::UnixStream;
    use std::sync::Mutex;
    use std::sync::mpsc::{Receiver, Sender, channel};

    fn req(query: &str, headers: &[(&str, &str)]) -> Request {
        Request {
            method: "GET".into(),
            path: "/orgs/acme/api/v1/terminal".into(),
            query: Some(query.into()),
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            body: vec![],
            peer: Peer::Tcp("127.0.0.1:5000".parse().unwrap()),
        }
    }

    #[test]
    fn parses_terminal_requests() {
        let t = term_request(&req("app=web&slot=2&cols=120&rows=40", &[])).unwrap();
        assert_eq!(
            t,
            TermRequest {
                app: "web".into(),
                slot: Some(2),
                cols: 120,
                rows: 40,
                ..Default::default()
            }
        );
        let t = term_request(&req("instance=box-1", &[])).unwrap();
        assert_eq!((t.app.as_str(), t.instance.as_deref()), ("", Some("box-1")));
        assert_eq!(t.query(), "instance=box-1&cols=80&rows=24");
        assert_eq!(t.target(), "box-1");
        let t = term_request(&req("app=web", &[])).unwrap();
        assert_eq!((t.slot, t.cols, t.rows), (None, 80, 24));
        for bad in [
            "",
            "app=",
            "app=../x",
            "app=Web",
            "app=web&slot=0",
            "app=web&cols=1",
            "app=web&rows=x",
            "instance=Box",
            "instance=a&app=b",
            "instance=a&slot=1",
            "stack=shop",
            "stack=Shop&service=web",
            "stack=shop&service=a%2Fb",
            "app=web&service=db",
            "app=web&stack=shop&service=web",
            "app=web&user=root",
            "app=web&argv=",
            "app=web&argv=sh&env=NOEQUALS",
            "app=web&argv=%zz",
            "app=web&argv=a%00b",
        ] {
            assert!(term_request(&req(bad, &[])).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_stack_service_and_a_command_go_through_the_query() {
        let t = TermRequest {
            app: "my_svc.1".into(),
            stack: Some("shop".into()),
            slot: Some(2),
            cols: 100,
            rows: 30,
            command: Some(TermCommand {
                argv: vec![
                    "stalwart-migrate".into(),
                    "prepare".into(),
                    "--to=a b&c".into(),
                    "ünï".into(),
                ],
                user: Some("1000:1000".into()),
                cwd: Some("/var/lib/x y".into()),
                env: [("TERM".into(), "xterm".into()), ("A".into(), "1=2".into())].into(),
            }),
            ..Default::default()
        };
        let back = term_request(&req(&t.query(), &[])).unwrap();
        assert_eq!(back, t);
        assert_eq!(t.target(), "shop/my_svc.1");
        assert_eq!(t.tool(), "stack_exec");
        // A shell is sandbox_exec's, whatever it opens in; a command, its
        // exec tool's.
        let shell = TermRequest {
            command: None,
            ..t.clone()
        };
        assert_eq!(shell.tool(), "sandbox_exec");
        let app = TermRequest {
            stack: None,
            app: "web".into(),
            ..t.clone()
        };
        assert_eq!(app.tool(), "app_exec");
        let inst = TermRequest {
            instance: Some("box-1".into()),
            app: String::new(),
            stack: None,
            slot: None,
            ..t
        };
        assert_eq!(inst.tool(), "instance_exec");
        assert_eq!(term_request(&req(&inst.query(), &[])).unwrap(), inst);
    }

    #[test]
    fn origin_must_be_this_site_for_cookies() {
        let host = ("Host", "isb.example.com");
        assert!(origin_allowed(&req(
            "",
            &[host, ("Origin", "https://isb.example.com")]
        )));
        assert!(origin_allowed(&req(
            "",
            &[
                ("Host", "localhost:8092"),
                ("Origin", "http://localhost:8092")
            ]
        )));
        assert!(!origin_allowed(&req(
            "",
            &[host, ("Origin", "https://evil.example")]
        )));
        assert!(!origin_allowed(&req(
            "",
            &[host, ("Origin", "https://isb.example.com.evil.example")]
        )));
        assert!(!origin_allowed(&req("", &[host, ("Origin", "null")])));
        // No Origin: only a bearer token, which a browser never adds by itself.
        assert!(!origin_allowed(&req(
            "",
            &[host, ("Cookie", "isb_session=x")]
        )));
        assert!(origin_allowed(&req(
            "",
            &[host, ("Authorization", "Bearer isb_tok_x")]
        )));
        // The unix socket: no browser reaches it, so no Origin is needed;
        // one that is there must still name this site.
        let mut unix = req("", &[host]);
        unix.peer = Peer::Unix { uid: None };
        assert!(origin_allowed(&unix));
        unix.headers
            .push(("Origin".into(), "https://evil.example".into()));
        assert!(!origin_allowed(&unix));
    }

    #[test]
    fn recognises_a_websocket_upgrade() {
        let ok = [
            ("Upgrade", "websocket"),
            ("Connection", "keep-alive, Upgrade"),
            ("Sec-WebSocket-Version", "13"),
            ("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ];
        assert_eq!(
            websocket_key(&req("", &ok)).as_deref(),
            Some("dGhlIHNhbXBsZSBub25jZQ==")
        );
        assert_eq!(
            tungstenite::handshake::derive_accept_key(b"dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
        let without = |skip: &str| {
            let h: Vec<_> = ok.iter().copied().filter(|(k, _)| *k != skip).collect();
            websocket_key(&req("", &h))
        };
        for h in [
            "Upgrade",
            "Connection",
            "Sec-WebSocket-Version",
            "Sec-WebSocket-Key",
        ] {
            assert!(without(h).is_none(), "{h}");
        }
    }

    /// A terminal that echoes input upper-cased, and exits on "exit".
    struct Echo {
        rx: Receiver<PtyOutput>,
        tx: Sender<PtyOutput>,
        resized: Arc<Mutex<Option<(u16, u16)>>>,
        closed: Arc<Mutex<bool>>,
    }

    impl Pty for Echo {
        fn input(&mut self, d: &[u8]) -> crate::Result<()> {
            if d == b"exit" {
                self.tx.send(PtyOutput::Exit(Some(3))).unwrap();
            } else {
                self.tx
                    .send(PtyOutput::Data(d.to_ascii_uppercase()))
                    .unwrap();
            }
            Ok(())
        }
        fn resize(&mut self, c: u16, r: u16) {
            *self.resized.lock().unwrap() = Some((c, r));
        }
        fn output(&mut self, wait: Duration) -> PtyOutput {
            self.rx.recv_timeout(wait).unwrap_or(PtyOutput::Idle)
        }
        fn close(&mut self) {
            *self.closed.lock().unwrap() = true;
        }
    }

    type Resized = Arc<Mutex<Option<(u16, u16)>>>;

    fn echo() -> (Box<dyn Pty>, Resized, Arc<Mutex<bool>>) {
        let (tx, rx) = channel();
        let resized = Arc::new(Mutex::new(None));
        let closed = Arc::new(Mutex::new(false));
        (
            Box::new(Echo {
                rx,
                tx,
                resized: resized.clone(),
                closed: closed.clone(),
            }),
            resized,
            closed,
        )
    }

    fn pair() -> (UnixStream, WebSocket<UnixStream>) {
        let (a, b) = UnixStream::pair().unwrap();
        b.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        (a, WebSocket::from_raw_socket(b, Role::Client, None))
    }

    #[test]
    fn bridges_bytes_resizes_and_exit() {
        let (server, mut client) = pair();
        let (pty, resized, closed) = echo();
        let t = std::thread::spawn(move || {
            let mut s = server;
            let d: &mut dyn Duplex = &mut s;
            let mut ws = WebSocket::from_raw_socket(d, Role::Server, None);
            bridge(&mut ws, pty, IDLE, MAX_AGE);
        });
        client
            .send(Message::text(r#"{"type":"resize","cols":100,"rows":30}"#))
            .unwrap();
        client.send(Message::binary(b"ls".to_vec())).unwrap();
        assert_eq!(client.read().unwrap(), Message::binary(b"LS".to_vec()));
        client.send(Message::binary(b"exit".to_vec())).unwrap();
        let m = client.read().unwrap();
        let v: serde_json::Value = serde_json::from_str(m.to_text().unwrap()).unwrap();
        assert_eq!(v, json!({"type": "exit", "code": 3}));
        assert!(matches!(client.read(), Ok(Message::Close(_))));
        t.join().unwrap();
        assert_eq!(*resized.lock().unwrap(), Some((100, 30)));
        assert!(*closed.lock().unwrap());
    }

    /// `exec -t`'s end against the server's: keys out, output back, a
    /// resize on SIGWINCH, and the program's exit code.
    #[test]
    fn the_client_end_relays_keys_resizes_and_the_exit_code() {
        let (server, client) = UnixStream::pair().unwrap();
        let (pty, resized, closed) = echo();
        let t = std::thread::spawn(move || {
            let mut s = server;
            let d: &mut dyn Duplex = &mut s;
            let mut ws = WebSocket::from_raw_socket(d, Role::Server, None);
            bridge(&mut ws, pty, IDLE, MAX_AGE);
        });
        let mut ws = WebSocket::from_raw_socket(Box::new(client), Role::Client, None);
        // The keys: "ls", then "exit" a moment later (one frame each).
        let (mut keys, input) = UnixStream::pair().unwrap();
        std::thread::spawn(move || {
            use std::io::Write;
            keys.write_all(b"ls").unwrap();
            std::thread::sleep(Duration::from_millis(300));
            keys.write_all(b"exit").unwrap();
            std::thread::sleep(Duration::from_secs(5));
        });
        let winch = AtomicBool::new(true);
        let stop = AtomicBool::new(false);
        let mut out = Vec::new();
        let code = attach(&mut ws, input, &mut out, || Some((132, 43)), &winch, &stop).unwrap();
        assert_eq!(code, Some(3));
        assert_eq!(out, b"LS");
        t.join().unwrap();
        assert_eq!(*resized.lock().unwrap(), Some((132, 43)));
        assert!(*closed.lock().unwrap());
    }

    #[test]
    fn a_stopped_client_closes_the_session_and_a_refusal_is_an_error() {
        let (server, client) = UnixStream::pair().unwrap();
        let (pty, _, closed) = echo();
        let t = std::thread::spawn(move || {
            let mut s = server;
            let d: &mut dyn Duplex = &mut s;
            let mut ws = WebSocket::from_raw_socket(d, Role::Server, None);
            bridge(&mut ws, pty, IDLE, MAX_AGE);
        });
        let mut ws = WebSocket::from_raw_socket(Box::new(client), Role::Client, None);
        let (_keys, input) = UnixStream::pair().unwrap();
        let (no, yes) = (AtomicBool::new(false), AtomicBool::new(true));
        let code = attach(&mut ws, input, Vec::new(), || None, &no, &yes).unwrap();
        assert_eq!(code, None);
        t.join().unwrap();
        assert!(*closed.lock().unwrap(), "the program goes with the session");

        let (server, client) = UnixStream::pair().unwrap();
        std::thread::spawn(move || {
            let mut ws = WebSocket::from_raw_socket(server, Role::Server, None);
            refuse(&mut ws, "stack_exec is not allowed");
        });
        let mut ws = WebSocket::from_raw_socket(Box::new(client), Role::Client, None);
        let (_keys, input) = UnixStream::pair().unwrap();
        let e = attach(&mut ws, input, Vec::new(), || None, &no, &no).unwrap_err();
        assert!(e.to_string().contains("stack_exec is not allowed"), "{e}");
    }

    #[test]
    fn idle_sessions_are_closed() {
        let (server, mut client) = pair();
        let (pty, _, closed) = echo();
        let t = std::thread::spawn(move || {
            let mut s = server;
            let d: &mut dyn Duplex = &mut s;
            let mut ws = WebSocket::from_raw_socket(d, Role::Server, None);
            bridge(&mut ws, pty, Duration::from_millis(100), MAX_AGE);
        });
        let m = client.read().unwrap();
        assert!(m.to_text().unwrap().contains("idle"), "{m:?}");
        assert!(matches!(client.read(), Ok(Message::Close(_))));
        t.join().unwrap();
        assert!(*closed.lock().unwrap());
    }

    #[test]
    fn a_dropped_browser_closes_the_terminal() {
        let (server, client) = pair();
        let (pty, _, closed) = echo();
        let t = std::thread::spawn(move || {
            let mut s = server;
            let d: &mut dyn Duplex = &mut s;
            let mut ws = WebSocket::from_raw_socket(d, Role::Server, None);
            bridge(&mut ws, pty, IDLE, MAX_AGE);
        });
        drop(client);
        t.join().unwrap();
        assert!(*closed.lock().unwrap());
    }
}
