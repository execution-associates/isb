//! A terminal over a websocket: `GET /orgs/<org>/api/v1/terminal?app=NAME`
//! upgrades to a websocket bridged to a pseudo-terminal the embedder opens
//! (`isb serve`: a shell in one of the app's replicas).
//!
//! The gate is the REST surface's: the caller authenticates as for any tool
//! (session, API token, Access), and is admitted as if calling
//! `sandbox_exec` in the org, so the org rules and `--deny-tools` apply.
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

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
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

/// What the browser asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermRequest {
    pub app: String,
    /// A replica's slot; `None` picks one.
    pub slot: Option<u32>,
    pub cols: u16,
    pub rows: u16,
}

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
}

/// Opens a terminal for `caller` in `org`; refusals become an error frame.
pub type Terminal = Arc<
    dyn Fn(&Caller, &crate::org::OrgId, &TermRequest) -> crate::Result<Box<dyn Pty>> + Send + Sync,
>;

static ACTIVE: AtomicUsize = AtomicUsize::new(0);

struct Slot;

impl Drop for Slot {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::SeqCst);
    }
}

fn param(req: &Request, key: &str) -> Option<String> {
    req.query.as_deref()?.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        (k == key).then(|| v.to_string())
    })
}

/// The request's terminal parameters, or what is wrong with them.
pub fn term_request(req: &Request) -> std::result::Result<TermRequest, String> {
    let app = param(req, "app").ok_or("app= is required")?;
    if app.is_empty()
        || app.len() > 64
        || !app
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err("app= is not an app name".into());
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
        Some(_) => Some(num("slot", 1, 1000, 1)?),
    };
    Ok(TermRequest {
        app,
        slot,
        cols: num("cols", 2, 1000, 80)? as u16,
        rows: num("rows", 2, 1000, 24)? as u16,
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
/// bearer token is not sent by browsers on their own, so it needs no Origin.
pub fn origin_allowed(req: &Request) -> bool {
    let bearer = req.header("authorization").is_some();
    match req.header("origin") {
        None => bearer,
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
            if ACTIVE.fetch_add(1, Ordering::SeqCst) >= MAX_SESSIONS {
                ACTIVE.fetch_sub(1, Ordering::SeqCst);
                refuse(
                    &mut ws,
                    "too many terminals are open on this server; close one and try again",
                );
                return;
            }
            let _slot = Slot;
            match open() {
                Ok(pty) => bridge(&mut ws, pty, IDLE, MAX_AGE),
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

fn would_block(e: &tungstenite::Error) -> bool {
    matches!(e, tungstenite::Error::Io(io) if matches!(io.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut))
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
            peer: Peer::Unix { uid: None },
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
                rows: 40
            }
        );
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
        ] {
            assert!(term_request(&req(bad, &[])).is_err(), "{bad}");
        }
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
