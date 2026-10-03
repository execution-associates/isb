//! Running commands in a sandbox over the incus exec websocket API.
//!
//! - argv is passed as a list, never joined into `sh -c`.
//! - Output is streamed as produced, and there is no default timeout.
//! - stdin is forwarded and closed properly. A caller whose own stdin is an
//!   inherited pipe that never reaches EOF does not hang: the forwarder polls and
//!   stops when the command exits.
//! - Closing the control websocket kills the command in incus, so it is held open
//!   until the operation finishes.

use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tungstenite::{Message, WebSocket};

use crate::client::{Client, Reply, encode_segment};
use crate::error::{Error, Result};

type Ws = WebSocket<UnixStream>;

/// Output chunks a command may run ahead of its reader.
const EVENTS_QUEUED: usize = 256;
/// Input chunks queued for a command's stdin.
const STDIN_QUEUED: usize = 16;

/// Where the command's stdin comes from.
#[derive(Debug, Clone, Default)]
pub enum Stdin {
    /// Closed immediately (like `/dev/null`).
    #[default]
    Null,
    /// These bytes, then EOF.
    Bytes(Vec<u8>),
    /// This process's own stdin (fd 0), until EOF or until the command exits.
    Inherit,
    /// Written through [`ExecStream::write_stdin`], ended by
    /// [`ExecStream::close_stdin`].
    Piped,
}

/// Per-call exec options. Unset fields fall back to the sandbox's exec defaults.
#[derive(Debug, Clone, Default)]
pub struct ExecOptions {
    pub cwd: Option<String>,
    /// A guest user name, `uid`, or `uid:gid`.
    pub user: Option<String>,
    pub env: BTreeMap<String, String>,
    /// Run through the user's login shell.
    pub login: Option<bool>,
    /// Allocate a pseudo-terminal (stdout and stderr are merged, as on any tty).
    pub tty: bool,
    pub width: Option<u16>,
    pub height: Option<u16>,
    /// Kill the command after this long. None (the default) means no limit.
    pub timeout: Option<Duration>,
    pub stdin: Stdin,
}

impl ExecOptions {
    pub fn cwd(mut self, c: impl Into<String>) -> Self {
        self.cwd = Some(c.into());
        self
    }
    pub fn user(mut self, u: impl Into<String>) -> Self {
        self.user = Some(u.into());
        self
    }
    pub fn env(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.env.insert(k.into(), v.into());
        self
    }
    pub fn login(mut self, l: bool) -> Self {
        self.login = Some(l);
        self
    }
    pub fn tty(mut self, t: bool) -> Self {
        self.tty = t;
        self
    }
    pub fn timeout(mut self, t: Duration) -> Self {
        self.timeout = Some(t);
        self
    }
    pub fn stdin(mut self, s: Stdin) -> Self {
        self.stdin = s;
        self
    }
}

/// Captured result of [`crate::Sandbox::exec`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecOutput {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl ExecOutput {
    pub fn success(&self) -> bool {
        self.exit_code == 0
    }
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

/// A chunk of output, in the order it was produced per stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecEvent {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
}

/// A guest user resolved to ids.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GuestUser {
    pub name: Option<String>,
    pub uid: u32,
    pub gid: u32,
    pub home: Option<String>,
    pub shell: Option<String>,
}

/// Parse a `getent passwd` line.
pub fn parse_passwd(line: &str) -> Option<GuestUser> {
    let f: Vec<&str> = line.trim().split(':').collect();
    if f.len() < 7 {
        return None;
    }
    Some(GuestUser {
        name: Some(f[0].to_string()),
        uid: f[2].parse().ok()?,
        gid: f[3].parse().ok()?,
        home: Some(f[5].to_string()).filter(|s| !s.is_empty()),
        shell: Some(f[6].to_string()).filter(|s| !s.is_empty()),
    })
}

/// Resolve `dev`, `1000` or `1000:1000` to ids inside the guest.
#[doc(hidden)]
pub fn resolve_user(client: &Client, instance: &str, user: &str) -> Result<GuestUser> {
    if let Some((u, g)) = user.split_once(':') {
        if let (Ok(uid), Ok(gid)) = (u.parse::<u32>(), g.parse::<u32>()) {
            let mut gu = lookup_passwd(client, instance, u)?.unwrap_or_default();
            gu.uid = uid;
            gu.gid = gid;
            return Ok(gu);
        }
        // `name:group` — resolve both.
        let mut gu = lookup_passwd(client, instance, u)?
            .ok_or_else(|| Error::invalid(format!("user {u:?} does not exist in {instance}")))?;
        gu.gid = match g.parse::<u32>() {
            Ok(n) => n,
            Err(_) => lookup_group(client, instance, g)?,
        };
        return Ok(gu);
    }
    match lookup_passwd(client, instance, user)? {
        Some(u) => Ok(u),
        None => match user.parse::<u32>() {
            Ok(uid) => Ok(GuestUser {
                uid,
                gid: uid,
                ..Default::default()
            }),
            Err(_) => Err(Error::invalid(format!(
                "user {user:?} does not exist in {instance}"
            ))),
        },
    }
}

fn lookup_passwd(client: &Client, instance: &str, user: &str) -> Result<Option<GuestUser>> {
    let out = run_captured(
        client,
        instance,
        &["getent".into(), "passwd".into(), user.into()],
        &Request::root(),
        Stdin::Null,
        Some(Duration::from_secs(30)),
    )?;
    if !out.success() {
        return Ok(None);
    }
    Ok(out.stdout_text().lines().next().and_then(parse_passwd))
}

fn lookup_group(client: &Client, instance: &str, group: &str) -> Result<u32> {
    let out = run_captured(
        client,
        instance,
        &["getent".into(), "group".into(), group.into()],
        &Request::root(),
        Stdin::Null,
        Some(Duration::from_secs(30)),
    )?;
    out.stdout_text()
        .lines()
        .next()
        .and_then(|l| l.split(':').nth(2))
        .and_then(|g| g.parse().ok())
        .ok_or_else(|| Error::invalid(format!("group {group:?} does not exist in {instance}")))
}

/// A fully resolved exec request.
#[derive(Debug, Clone, Default)]
pub(crate) struct Request {
    pub argv: Vec<String>,
    pub cwd: Option<String>,
    pub uid: u32,
    pub gid: u32,
    pub env: BTreeMap<String, String>,
    pub tty: bool,
    pub width: Option<u16>,
    pub height: Option<u16>,
}

impl Request {
    fn root() -> Self {
        Request::default()
    }
}

/// Merge sandbox defaults with call options and resolve the user.
pub(crate) fn build_request(
    client: &Client,
    instance: &str,
    argv: &[String],
    defaults: &crate::spec::ExecDefaults,
    opts: &ExecOptions,
) -> Result<Request> {
    if argv.is_empty() {
        return Err(Error::invalid("exec needs a command"));
    }
    let mut env = defaults.env.clone();
    env.extend(opts.env.clone());
    let user = opts.user.clone().or_else(|| defaults.user.clone());
    let login = opts.login.unwrap_or(defaults.login);
    let gu = match &user {
        Some(u) => Some(resolve_user(client, instance, u)?),
        None => None,
    };
    if let Some(g) = &gu {
        if let Some(h) = &g.home {
            env.entry("HOME".into()).or_insert_with(|| h.clone());
        }
        if let Some(n) = &g.name {
            env.entry("USER".into()).or_insert_with(|| n.clone());
            env.entry("LOGNAME".into()).or_insert_with(|| n.clone());
        }
    }
    let mut final_argv = argv.to_vec();
    if login {
        let shell = gu
            .as_ref()
            .and_then(|g| g.shell.clone())
            .filter(|s| !s.ends_with("nologin") && !s.ends_with("/false"))
            .unwrap_or_else(|| "/bin/sh".into());
        // argv stays a list: the shell only runs `exec "$@"`.
        let mut v = vec![
            shell,
            "-l".into(),
            "-c".into(),
            "exec \"$@\"".into(),
            "isb".into(),
        ];
        v.extend(final_argv);
        final_argv = v;
    }
    if opts.tty {
        env.entry("TERM".into())
            .or_insert_with(|| std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".into()));
    }
    Ok(Request {
        argv: final_argv,
        cwd: opts
            .cwd
            .clone()
            .or_else(|| defaults.cwd.clone())
            .or_else(|| gu.as_ref().and_then(|g| g.home.clone())),
        uid: gu.as_ref().map(|g| g.uid).unwrap_or(0),
        gid: gu.as_ref().map(|g| g.gid).unwrap_or(0),
        env,
        tty: opts.tty,
        width: opts.width,
        height: opts.height,
    })
}

/// A running command. Iterate it for output; [`ExecStream::wait`] for the exit code.
pub struct ExecStream {
    events: Receiver<ExecEvent>,
    control: Arc<Mutex<Option<Ws>>>,
    stdin_tx: Option<SyncSender<Option<Vec<u8>>>>,
    waiter: Option<JoinHandle<Result<i32>>>,
    stop: Arc<AtomicBool>,
    tty: bool,
}

/// A cloneable handle for driving a running command (stdin, signals, window
/// size) from other threads while one thread reads its output.
#[derive(Clone)]
pub struct ExecController {
    control: Arc<Mutex<Option<Ws>>>,
    stdin_tx: Option<SyncSender<Option<Vec<u8>>>>,
    tty: bool,
}

impl ExecController {
    /// Write to stdin (only with [`Stdin::Piped`]).
    pub fn write_stdin(&self, data: &[u8]) -> Result<()> {
        match &self.stdin_tx {
            Some(tx) => tx
                .send(Some(data.to_vec()))
                .map_err(|_| Error::WebSocket("stdin is closed".into())),
            None => Err(Error::invalid("stdin is not piped")),
        }
    }

    /// Send EOF on stdin (only with [`Stdin::Piped`]). Later writes fail.
    pub fn close_stdin(&self) -> Result<()> {
        match &self.stdin_tx {
            Some(tx) => {
                let _ = tx.send(None);
                Ok(())
            }
            None => Err(Error::invalid("stdin is not piped")),
        }
    }

    pub fn signal(&self, signal: i32) -> Result<()> {
        send_control(
            &self.control,
            json!({"command": "signal", "signal": signal}),
        )
    }

    pub fn resize(&self, width: u16, height: u16) -> Result<()> {
        if !self.tty {
            return Ok(());
        }
        send_control(
            &self.control,
            json!({"command": "window-resize", "args": {"width": width.to_string(), "height": height.to_string()}}),
        )
    }
}

impl ExecStream {
    /// A handle for stdin, signals and resizes usable from other threads.
    pub fn controller(&self) -> ExecController {
        ExecController {
            control: self.control.clone(),
            stdin_tx: self.stdin_tx.clone(),
            tty: self.tty,
        }
    }

    /// Next chunk of output, blocking. `None` once all output has been read.
    pub fn next_event(&mut self) -> Option<ExecEvent> {
        self.events.recv().ok()
    }

    /// Next chunk of output within `wait`: `Ok(None)` if none came, `Err`
    /// with the exit code once the command ended and its output was read.
    pub fn poll_event(
        &mut self,
        wait: Duration,
    ) -> std::result::Result<Option<ExecEvent>, Result<i32>> {
        use std::sync::mpsc::RecvTimeoutError;
        let r = if wait.is_zero() {
            self.events.try_recv().map_err(|e| match e {
                std::sync::mpsc::TryRecvError::Empty => RecvTimeoutError::Timeout,
                std::sync::mpsc::TryRecvError::Disconnected => RecvTimeoutError::Disconnected,
            })
        } else {
            self.events.recv_timeout(wait)
        };
        match r {
            Ok(ev) => Ok(Some(ev)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(self.finish()),
        }
    }

    /// Write to the command's stdin (only with [`Stdin::Piped`]).
    pub fn write_stdin(&self, data: &[u8]) -> Result<()> {
        match &self.stdin_tx {
            Some(tx) => tx
                .send(Some(data.to_vec()))
                .map_err(|_| Error::WebSocket("stdin is closed".into())),
            None => Err(Error::invalid("stdin is not piped")),
        }
    }

    /// Send EOF on stdin (only with [`Stdin::Piped`]).
    pub fn close_stdin(&mut self) {
        if let Some(tx) = self.stdin_tx.take() {
            let _ = tx.send(None);
        }
    }

    /// Send a signal to the command (e.g. 2 for SIGINT, 15 for SIGTERM).
    pub fn signal(&self, signal: i32) -> Result<()> {
        send_control(
            &self.control,
            json!({"command": "signal", "signal": signal}),
        )
    }

    /// Resize the pseudo-terminal (tty mode only).
    pub fn resize(&self, width: u16, height: u16) -> Result<()> {
        if !self.tty {
            return Ok(());
        }
        send_control(
            &self.control,
            json!({"command": "window-resize", "args": {"width": width.to_string(), "height": height.to_string()}}),
        )
    }

    /// Discard any remaining output and return the exit code.
    pub fn wait(mut self) -> Result<i32> {
        while self.events.recv().is_ok() {}
        self.finish()
    }

    fn finish(&mut self) -> Result<i32> {
        let r = match self.waiter.take() {
            Some(h) => h
                .join()
                .unwrap_or_else(|_| Err(Error::Protocol("exec waiter panicked".into()))),
            None => Err(Error::Protocol("exec already finished".into())),
        };
        self.stop.store(true, Ordering::SeqCst);
        r
    }

    /// Collect everything into an [`ExecOutput`].
    pub fn collect_output(mut self) -> Result<ExecOutput> {
        let mut out = ExecOutput::default();
        while let Some(ev) = self.next_event() {
            match ev {
                ExecEvent::Stdout(b) => out.stdout.extend(b),
                ExecEvent::Stderr(b) => out.stderr.extend(b),
            }
        }
        out.exit_code = self.finish()?;
        Ok(out)
    }
}

impl Iterator for ExecStream {
    type Item = ExecEvent;
    fn next(&mut self) -> Option<ExecEvent> {
        self.next_event()
    }
}

impl Drop for ExecStream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn send_control(control: &Arc<Mutex<Option<Ws>>>, msg: Value) -> Result<()> {
    let mut g = control.lock().unwrap_or_else(|p| p.into_inner());
    match g.as_mut() {
        Some(ws) => ws
            .send(Message::text(msg.to_string()))
            .map_err(|e| Error::WebSocket(format!("control: {e}"))),
        None => Err(Error::WebSocket("command has finished".into())),
    }
}

fn is_would_block(e: &tungstenite::Error) -> bool {
    matches!(e, tungstenite::Error::Io(io) if matches!(io.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut))
}

/// Read one output websocket to the end, forwarding binary frames.
fn pump_output(mut ws: Ws, tx: SyncSender<ExecEvent>, stderr: bool) {
    loop {
        match ws.read() {
            Ok(Message::Binary(b)) => {
                if b.is_empty() {
                    continue;
                }
                let ev = if stderr {
                    ExecEvent::Stderr(b.to_vec())
                } else {
                    ExecEvent::Stdout(b.to_vec())
                };
                if tx.send(ev).is_err() {
                    // Receiver gone; keep draining so the server is never blocked.
                }
            }
            // incus ends a stream with an empty text frame (its "barrier"), sent
            // after the last byte of output.
            Ok(Message::Text(_)) | Ok(Message::Close(_)) => {
                let _ = ws.close(None);
                let _ = ws.flush();
                break;
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
pub(crate) fn start_with_timeout(
    client: &Client,
    instance: &str,
    req: Request,
    stdin: Stdin,
    timeout: Option<Duration>,
) -> Result<ExecStream> {
    let mut body = json!({
        "command": req.argv,
        "environment": req.env,
        "wait-for-websocket": true,
        "interactive": req.tty,
        "user": req.uid,
        "group": req.gid,
    });
    if let Some(c) = &req.cwd {
        body["cwd"] = json!(c);
    }
    if req.tty {
        body["width"] = json!(req.width.unwrap_or(80));
        body["height"] = json!(req.height.unwrap_or(24));
    }
    let path = format!("/1.0/instances/{}/exec", encode_segment(instance));
    let (operation, meta) =
        match client.request("POST", &path, Some(&body), client.timeouts.request)? {
            Reply::Async {
                operation,
                metadata,
            } => (operation, metadata),
            Reply::Sync(_) => {
                return Err(Error::Protocol("exec did not return an operation".into()));
            }
        };
    let fds = meta
        .pointer("/metadata/fds")
        .and_then(Value::as_object)
        .cloned()
        .ok_or_else(|| Error::Protocol("exec operation has no websocket secrets".into()))?;
    let secret = |k: &str| -> Result<String> {
        fds.get(k)
            .and_then(Value::as_str)
            .map(String::from)
            .ok_or_else(|| Error::Protocol(format!("exec operation has no {k} websocket")))
    };

    let control_ws = client.websocket(&operation, &secret("control")?)?;
    let control = Arc::new(Mutex::new(Some(control_ws)));
    // Bounded both ways, so a slow reader (a backup streaming to S3) or a
    // fast writer (a restore) holds the command back instead of buffering
    // its whole output or input in memory.
    let (tx, rx) = sync_channel::<ExecEvent>(EVENTS_QUEUED);
    let stop = Arc::new(AtomicBool::new(false));
    let mut readers: Vec<JoinHandle<()>> = Vec::new();
    let mut stdin_tx = None;

    // The stdin source, as a channel of chunks (None = EOF), fed by a thread.
    let (in_tx, in_rx) = sync_channel::<Option<Vec<u8>>>(STDIN_QUEUED);
    match stdin {
        Stdin::Null => {
            let _ = in_tx.send(None);
        }
        Stdin::Bytes(b) => {
            std::thread::spawn(move || {
                for c in b.chunks(64 * 1024) {
                    if in_tx.send(Some(c.to_vec())).is_err() {
                        return;
                    }
                }
                let _ = in_tx.send(None);
            });
        }
        Stdin::Piped => stdin_tx = Some(in_tx),
        Stdin::Inherit => {
            let stop2 = stop.clone();
            std::thread::spawn(move || forward_fd0(in_tx, stop2));
        }
    }

    if req.tty {
        // One bidirectional socket. Reads poll with a short timeout so the writer
        // can take the lock between them.
        let ws = client.websocket(&operation, &secret("0")?)?;
        ws.get_ref()
            .set_read_timeout(Some(Duration::from_millis(20)))?;
        let ws = Arc::new(Mutex::new(ws));
        let reader_ws = ws.clone();
        let tx2 = tx.clone();
        readers.push(std::thread::spawn(move || {
            loop {
                let r = {
                    let mut g = reader_ws.lock().unwrap_or_else(|p| p.into_inner());
                    g.read()
                };
                match r {
                    Ok(Message::Binary(b)) => {
                        let _ = tx2.send(ExecEvent::Stdout(b.to_vec()));
                    }
                    // End of output (the barrier). incus finishes the operation
                    // only once the client closes this socket, so close it now;
                    // waiting for our own stdin to end would hang on a terminal.
                    Ok(Message::Text(_)) | Ok(Message::Close(_)) => {
                        let mut g = reader_ws.lock().unwrap_or_else(|p| p.into_inner());
                        let _ = g.close(None);
                        let _ = g.flush();
                        break;
                    }
                    Ok(_) => {}
                    Err(e) if is_would_block(&e) => {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(_) => break,
                }
            }
        }));
        let stop2 = stop.clone();
        std::thread::spawn(move || {
            while let Ok(Some(chunk)) = in_rx.recv() {
                if stop2.load(Ordering::SeqCst) {
                    break;
                }
                let mut g = ws.lock().unwrap_or_else(|p| p.into_inner());
                if g.send(Message::binary(chunk)).is_err() {
                    break;
                }
            }
        });
    } else {
        // Connect all three before anything else: incus starts the command once
        // stdin, stdout and stderr are attached.
        let mut ws_in = client.websocket(&operation, &secret("0")?)?;
        let ws_out = client.websocket(&operation, &secret("1")?)?;
        let ws_err = client.websocket(&operation, &secret("2")?)?;
        let tx1 = tx.clone();
        readers.push(std::thread::spawn(move || pump_output(ws_out, tx1, false)));
        let tx2 = tx.clone();
        readers.push(std::thread::spawn(move || pump_output(ws_err, tx2, true)));
        let stop2 = stop.clone();
        std::thread::spawn(move || {
            loop {
                match in_rx.recv() {
                    Ok(Some(chunk)) => {
                        if stop2.load(Ordering::SeqCst)
                            || ws_in.send(Message::binary(chunk)).is_err()
                        {
                            return;
                        }
                    }
                    // EOF (or the source went away): an empty text frame is incus'
                    // end-of-stdin barrier; then close cleanly.
                    Ok(None) | Err(_) => {
                        let _ = ws_in.send(Message::text(""));
                        let _ = ws_in.close(None);
                        let _ = ws_in.flush();
                        return;
                    }
                }
            }
        });
    }
    drop(tx);

    let client2 = client.clone();
    let control2 = control.clone();
    let argv_desc = req.argv.join(" ");
    let stop3 = stop.clone();
    let waiter = std::thread::spawn(move || -> Result<i32> {
        let started = Instant::now();
        let mut killed_at: Option<Instant> = None;
        let result = loop {
            let secs = match (timeout, killed_at) {
                (_, Some(_)) => 1,
                (Some(t), None) => t.saturating_sub(started.elapsed()).as_secs().min(30),
                (None, None) => 30,
            };
            if secs == 0 {
                std::thread::sleep(Duration::from_millis(50));
            }
            match client2.poll_operation(&operation, "exec", secs) {
                Ok(Some(meta)) => {
                    break Ok(meta.get("return").and_then(Value::as_i64).unwrap_or(-1) as i32);
                }
                Ok(None) => {}
                Err(e) => break Err(e),
            }
            if let Some(t) = timeout {
                match killed_at {
                    None if started.elapsed() >= t => {
                        let _ = send_control(&control2, json!({"command": "signal", "signal": 9}));
                        killed_at = Some(Instant::now());
                    }
                    Some(k) if k.elapsed() >= Duration::from_secs(10) => {
                        break Err(Error::ExecTimeout {
                            argv: argv_desc.clone(),
                            timeout: t,
                        });
                    }
                    _ => {}
                }
            }
        };
        // Output sockets close once the process and its mirrors are done.
        for r in readers {
            let _ = r.join();
        }
        stop3.store(true, Ordering::SeqCst);
        // Only now is it safe to drop control: closing it earlier kills the command.
        if let Some(mut ws) = control2.lock().unwrap_or_else(|p| p.into_inner()).take() {
            let _ = ws.close(None);
            let _ = ws.flush();
        }
        match (result, killed_at, timeout) {
            (Ok(_), Some(_), Some(t)) => Err(Error::ExecTimeout {
                argv: argv_desc,
                timeout: t,
            }),
            (r, _, _) => r,
        }
    });

    Ok(ExecStream {
        events: rx,
        control,
        stdin_tx,
        waiter: Some(waiter),
        stop,
        tty: req.tty,
    })
}

/// Forward fd 0 until EOF, or until `stop` is set. Polls so that an inherited
/// stdin that never reaches EOF cannot keep anything alive.
fn forward_fd0(tx: SyncSender<Option<Vec<u8>>>, stop: Arc<AtomicBool>) {
    use rustix::event::{PollFd, PollFlags, poll};
    let stdin = std::io::stdin();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let ready = {
            let mut fds = [PollFd::new(&stdin, PollFlags::IN)];
            let ts = rustix::event::Timespec {
                tv_sec: 0,
                tv_nsec: 100_000_000,
            };
            match poll(&mut fds, Some(&ts)) {
                Ok(n) => n > 0,
                Err(rustix::io::Errno::INTR) => false,
                Err(_) => {
                    let _ = tx.send(None);
                    return;
                }
            }
        };
        if !ready {
            continue;
        }
        match rustix::io::read(&stdin, &mut buf) {
            Ok(0) => {
                let _ = tx.send(None);
                return;
            }
            Ok(n) => {
                if tx.send(Some(buf[..n].to_vec())).is_err() {
                    return;
                }
            }
            Err(rustix::io::Errno::INTR) | Err(rustix::io::Errno::AGAIN) => {}
            Err(_) => {
                let _ = tx.send(None);
                return;
            }
        }
    }
}

/// Run to completion and capture output.
pub(crate) fn run_captured(
    client: &Client,
    instance: &str,
    argv: &[String],
    base: &Request,
    stdin: Stdin,
    timeout: Option<Duration>,
) -> Result<ExecOutput> {
    let req = Request {
        argv: argv.to_vec(),
        tty: false,
        ..base.clone()
    };
    start_with_timeout(client, instance, req, stdin, timeout)?.collect_output()
}

/// Run attached to this process's terminal: stdio forwarded, raw mode and window
/// size in tty mode, signals forwarded. Returns the exit code.
pub(crate) fn attach(
    client: &Client,
    instance: &str,
    req: Request,
    stdin: Stdin,
    timeout: Option<Duration>,
) -> Result<i32> {
    use signal_hook::consts::signal::*;
    let tty = req.tty;
    let _raw = if tty { RawMode::enable() } else { None };
    let mut stream = start_with_timeout(client, instance, req, stdin, timeout)?;
    if tty {
        if let Some((w, h)) = terminal_size() {
            let _ = stream.resize(w, h);
        }
    }
    let mut signals = signal_hook::iterator::Signals::new([
        SIGINT, SIGTERM, SIGHUP, SIGQUIT, SIGUSR1, SIGUSR2, SIGWINCH,
    ])?;
    let handle = signals.handle();
    let control = stream.control.clone();
    let sig_thread = std::thread::spawn(move || {
        for sig in signals.forever() {
            if sig == SIGWINCH {
                if tty {
                    if let Some((w, h)) = terminal_size() {
                        let _ = send_control(
                            &control,
                            json!({"command": "window-resize", "args": {"width": w.to_string(), "height": h.to_string()}}),
                        );
                    }
                }
                continue;
            }
            let _ = send_control(
                &control,
                json!({"command": "signal", "signal": linux_signal(sig)}),
            );
        }
    });
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    while let Some(ev) = stream.next_event() {
        match ev {
            ExecEvent::Stdout(b) => {
                let mut o = stdout.lock();
                let _ = o.write_all(&b);
                let _ = o.flush();
            }
            ExecEvent::Stderr(b) => {
                let mut e = stderr.lock();
                let _ = e.write_all(&b);
                let _ = e.flush();
            }
        }
    }
    let code = stream.finish();
    handle.close();
    let _ = sig_thread.join();
    code
}

/// The guest is Linux whatever the host is, and the host's numbering can
/// differ: SIGUSR1/SIGUSR2 are 30/31 on macOS, 10/12 on Linux. The others
/// forwarded (INT, TERM, HUP, QUIT) agree everywhere.
fn linux_signal(sig: i32) -> i32 {
    use signal_hook::consts::signal::{SIGUSR1, SIGUSR2};
    match sig {
        SIGUSR1 => 10,
        SIGUSR2 => 12,
        s => s,
    }
}

/// Width and height of the terminal on stdout, if it is one.
pub fn terminal_size() -> Option<(u16, u16)> {
    let ws = rustix::termios::tcgetwinsize(std::io::stdout()).ok()?;
    if ws.ws_col == 0 || ws.ws_row == 0 {
        return None;
    }
    Some((ws.ws_col, ws.ws_row))
}

/// Whether both stdin and stdout are terminals (the default for `-t`).
pub fn stdio_is_tty() -> bool {
    rustix::termios::isatty(std::io::stdin()) && rustix::termios::isatty(std::io::stdout())
}

/// Puts the local terminal in raw mode for the life of the guard.
struct RawMode {
    saved: rustix::termios::Termios,
}

impl RawMode {
    fn enable() -> Option<RawMode> {
        let stdin = std::io::stdin();
        let saved = rustix::termios::tcgetattr(&stdin).ok()?;
        let mut raw = saved.clone();
        raw.make_raw();
        rustix::termios::tcsetattr(&stdin, rustix::termios::OptionalActions::Now, &raw).ok()?;
        Some(RawMode { saved })
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = rustix::termios::tcsetattr(
            std::io::stdin(),
            rustix::termios::OptionalActions::Now,
            &self.saved,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwd_lines() {
        let u = parse_passwd("dev:x:1000:1000:Dev,,,:/home/dev:/bin/bash\n").unwrap();
        assert_eq!(u.uid, 1000);
        assert_eq!(u.gid, 1000);
        assert_eq!(u.home.as_deref(), Some("/home/dev"));
        assert_eq!(u.shell.as_deref(), Some("/bin/bash"));
        assert!(parse_passwd("short:x:1").is_none());
        assert!(parse_passwd("bad:x:a:b:c:d:e").is_none());
    }
}
