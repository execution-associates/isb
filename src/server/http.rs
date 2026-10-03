//! A minimal synchronous HTTP/1.1 server: one request per connection, a thread
//! per connection, a hard cap on connections, and every read and write bounded.
//!
//! It serves a JSON API to a tunnel and a local CLI, nothing else, so it speaks
//! just enough HTTP for that: Content-Length bodies only, `Connection: close` on
//! every response. Anything it does not understand is refused with a status
//! rather than guessed at.

use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown as NetShutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::error::{Error, Result};

/// Bounds on what one connection may cost.
#[derive(Debug, Clone)]
pub struct Limits {
    /// Request line plus headers.
    pub max_header_bytes: usize,
    pub max_body_bytes: usize,
    /// Reading the whole request, start to finish.
    pub read_timeout: Duration,
    pub write_timeout: Duration,
    /// Across every listener of one [`HttpServer`]; beyond it new connections
    /// get a 503 instead of a thread.
    pub max_connections: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_header_bytes: 64 * 1024,
            max_body_bytes: 4 * 1024 * 1024,
            read_timeout: Duration::from_secs(30),
            write_timeout: Duration::from_secs(30),
            max_connections: 256,
        }
    }
}

/// Who is on the other end of a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Peer {
    Tcp(SocketAddr),
    /// `uid` from SO_PEERCRED (getpeereid on macOS), when the platform has it.
    Unix {
        uid: Option<u32>,
    },
}

#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub query: Option<String>,
    /// In arrival order, names as sent; look them up with [`Request::header`].
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub peer: Peer,
}

impl Request {
    /// The first header named `name`, case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Writes a streamed body (server-sent events) until it returns; the
/// connection closes after it, which is what delimits the body.
pub type StreamFn = Box<dyn FnOnce(&mut dyn Write) -> std::io::Result<()> + Send>;

/// A connection a handler takes over after `101 Switching Protocols` (a
/// websocket): both directions, and a read deadline it can shorten to poll.
pub trait Duplex: Read + Write + Send {
    fn set_read_timeout(&mut self, t: Option<Duration>) -> std::io::Result<()>;
}

impl Duplex for TcpStream {
    fn set_read_timeout(&mut self, t: Option<Duration>) -> std::io::Result<()> {
        TcpStream::set_read_timeout(self, t)
    }
}

impl Duplex for UnixStream {
    fn set_read_timeout(&mut self, t: Option<Duration>) -> std::io::Result<()> {
        UnixStream::set_read_timeout(self, t)
    }
}

/// Runs on the connection after a 101; the connection closes when it returns.
pub type UpgradeFn = Box<dyn FnOnce(&mut dyn Duplex) + Send>;

#[derive(Clone)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// When set, written after the headers in place of `body`.
    pub stream: Option<Arc<std::sync::Mutex<Option<StreamFn>>>>,
    /// When set, the status is 101 and this takes the connection over.
    pub upgrade: Option<Arc<std::sync::Mutex<Option<UpgradeFn>>>>,
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .field("body", &self.body.len())
            .field("stream", &self.stream.is_some())
            .field("upgrade", &self.upgrade.is_some())
            .finish()
    }
}

impl Response {
    pub fn new(status: u16) -> Self {
        Response {
            status,
            headers: Vec::new(),
            body: Vec::new(),
            stream: None,
            upgrade: None,
        }
    }

    /// `101 Switching Protocols` to `protocol`, then `f` owns the connection.
    pub fn upgrade(protocol: &str, f: UpgradeFn) -> Self {
        let mut r = Response::new(101).header("Upgrade", protocol);
        r.upgrade = Some(Arc::new(std::sync::Mutex::new(Some(f))));
        r
    }

    /// A streamed response: `f` writes the body and the connection closes
    /// when it returns.
    pub fn stream(status: u16, content_type: &str, f: StreamFn) -> Self {
        let mut r = Response::new(status).header("Content-Type", content_type);
        r.stream = Some(Arc::new(std::sync::Mutex::new(Some(f))));
        r
    }

    pub fn json(status: u16, v: &Value) -> Self {
        let mut body = serde_json::to_vec(v).unwrap_or_default();
        body.push(b'\n');
        Response::new(status)
            .header("Content-Type", "application/json")
            .body(body)
    }

    pub fn text(status: u16, s: &str) -> Self {
        Response::new(status)
            .header("Content-Type", "text/plain; charset=utf-8")
            .body(format!("{s}\n").into_bytes())
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }

    pub fn get_header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

pub type Handler = Arc<dyn Fn(&Request) -> Response + Send + Sync>;

/// A flag every accept loop polls. Cloning shares it.
#[derive(Debug, Clone, Default)]
pub struct Shutdown(Arc<AtomicBool>);

impl Shutdown {
    pub fn new() -> Self {
        Self::default()
    }

    /// A flag that SIGINT and SIGTERM set.
    pub fn on_signals() -> Result<Self> {
        let s = Self::new();
        for sig in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
            signal_hook::flag::register(sig, s.0.clone())?;
        }
        Ok(s)
    }

    pub fn trigger(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_triggered(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// A bound listening socket.
#[derive(Debug)]
pub enum HttpListener {
    Tcp(TcpListener),
    Unix(UnixSocket),
    /// TLS with client certificates (an `isb serve --agent` listener): any
    /// address, since the handshake is the gate.
    Tls(TcpListener, TlsConfig),
}

/// A TLS server config that can be swapped while serving (certificate
/// rotation); each connection takes the one current when it arrives.
pub type TlsConfig = Arc<std::sync::RwLock<Arc<rustls::ServerConfig>>>;

/// A unix listener that removes its socket file on drop, but only while the
/// file is still the one it bound, so a successor's socket is never deleted.
#[derive(Debug)]
pub struct UnixSocket {
    listener: UnixListener,
    path: PathBuf,
    ino: (u64, u64),
}

impl Drop for UnixSocket {
    fn drop(&mut self) {
        if std::fs::symlink_metadata(&self.path).is_ok_and(|m| (m.dev(), m.ino()) == self.ino) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

impl HttpListener {
    /// Bind a TCP address that must resolve to loopback only. Remote access
    /// belongs behind the tunnel, which reaches loopback; a public bind would
    /// skip Cloudflare Access entirely.
    pub fn bind_tcp(addr: &str) -> Result<Self> {
        let addrs: Vec<SocketAddr> = addr
            .to_socket_addrs()
            .map_err(|e| Error::invalid(format!("listen address {addr:?}: {e}")))?
            .collect();
        if addrs.is_empty() || addrs.iter().any(|a| !a.ip().is_loopback()) {
            return Err(Error::invalid(format!(
                "listen address {addr:?} is not loopback; expose it through a Cloudflare Tunnel instead"
            )));
        }
        let l = TcpListener::bind(addrs[0])?;
        l.set_nonblocking(true)?;
        Ok(HttpListener::Tcp(l))
    }

    /// Bind a unix socket, mode 0600. A parent directory isb creates is 0700. A
    /// stale socket is replaced; a live one (something answers) is an error,
    /// and so is a path that is not a socket.
    pub fn bind_unix(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
        }
        match std::fs::symlink_metadata(path) {
            Ok(m) if m.file_type().is_socket() => {
                if UnixStream::connect(path).is_ok() {
                    return Err(Error::invalid(format!(
                        "{} is in use by another server",
                        path.display()
                    )));
                }
                std::fs::remove_file(path)?;
            }
            Ok(_) => {
                return Err(Error::invalid(format!(
                    "{} exists and is not a socket",
                    path.display()
                )));
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let listener = UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let m = std::fs::symlink_metadata(path)?;
        Ok(HttpListener::Unix(UnixSocket {
            listener,
            path: path.to_path_buf(),
            ino: (m.dev(), m.ino()),
        }))
    }

    /// Bind `addr` (any address) for TLS. Only for a config that requires
    /// client certificates: nothing else stands between it and the network.
    pub fn bind_tls(addr: &str, config: TlsConfig) -> Result<Self> {
        let a = addr
            .to_socket_addrs()
            .map_err(|e| Error::invalid(format!("listen address {addr:?}: {e}")))?
            .next()
            .ok_or_else(|| Error::invalid(format!("listen address {addr:?} does not resolve")))?;
        let l = TcpListener::bind(a)?;
        l.set_nonblocking(true)?;
        Ok(HttpListener::Tls(l, config))
    }

    pub fn local_addr(&self) -> Option<SocketAddr> {
        match self {
            HttpListener::Tcp(l) | HttpListener::Tls(l, _) => l.local_addr().ok(),
            HttpListener::Unix(_) => None,
        }
    }

    fn poll(&self, timeout: Duration) -> bool {
        use rustix::event::{PollFd, PollFlags, Timespec, poll};
        let ts = Timespec {
            tv_sec: timeout.as_secs() as _,
            tv_nsec: timeout.subsec_nanos() as _,
        };
        let r = match self {
            HttpListener::Tcp(l) | HttpListener::Tls(l, _) => {
                poll(&mut [PollFd::new(l, PollFlags::IN)], Some(&ts))
            }
            HttpListener::Unix(u) => {
                poll(&mut [PollFd::new(&u.listener, PollFlags::IN)], Some(&ts))
            }
        };
        matches!(r, Ok(n) if n > 0)
    }
}

/// Owns the connection cap and the shutdown flag shared by every listener.
#[derive(Debug)]
pub struct HttpServer {
    limits: Limits,
    active: AtomicUsize,
    shutdown: Shutdown,
}

/// Holds one slot of the connection cap; released on drop, including when the
/// thread that would have owned it fails to spawn.
struct Slot(Arc<HttpServer>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}

impl HttpServer {
    pub fn new(limits: Limits, shutdown: Shutdown) -> Arc<Self> {
        Arc::new(HttpServer {
            limits,
            active: AtomicUsize::new(0),
            shutdown,
        })
    }

    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    /// Connections currently being served.
    pub fn active(&self) -> usize {
        self.active.load(Ordering::SeqCst)
    }

    /// Accept connections until shutdown. The listener (and a unix socket's
    /// file) is closed on return.
    pub fn run(self: &Arc<Self>, listener: HttpListener, handler: Handler) -> Result<()> {
        while !self.shutdown.is_triggered() {
            // Poll rather than block in accept, so shutdown is noticed promptly.
            if !listener.poll(Duration::from_millis(250)) {
                continue;
            }
            let accepted = match &listener {
                HttpListener::Tcp(l) => l.accept().map(|(s, a)| Conn::Tcp(s, a)),
                HttpListener::Tls(l, c) => l.accept().map(|(s, a)| {
                    let cfg = c.read().unwrap_or_else(|p| p.into_inner()).clone();
                    Conn::Tls(s, a, cfg)
                }),
                HttpListener::Unix(u) => u.listener.accept().map(|(s, _)| Conn::Unix(s)),
            };
            let conn = match accepted {
                Ok(c) => c,
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {
                    continue;
                }
                Err(e) => {
                    // EMFILE and friends: back off instead of spinning.
                    eprintln!("isb serve: accept: {e}");
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
            };
            if self.active.fetch_add(1, Ordering::SeqCst) >= self.limits.max_connections {
                self.active.fetch_sub(1, Ordering::SeqCst);
                conn.reject_busy();
                continue;
            }
            let slot = Slot(self.clone());
            let h = handler.clone();
            let spawned = std::thread::Builder::new()
                .name("isb-http".into())
                .spawn(move || {
                    let s = slot;
                    conn.serve(&s.0.limits, &h);
                });
            if let Err(e) = spawned {
                eprintln!("isb serve: cannot spawn connection thread: {e}");
            }
        }
        Ok(())
    }

    /// Wait, at most `timeout`, for in-flight connections to finish.
    pub fn drain(&self, timeout: Duration) {
        let started = Instant::now();
        while self.active() > 0 && started.elapsed() < timeout {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

enum Conn {
    Tcp(TcpStream, SocketAddr),
    Unix(UnixStream),
    Tls(TcpStream, SocketAddr, Arc<rustls::ServerConfig>),
}

/// A server-side TLS connection, as a [`Duplex`].
struct TlsConn(rustls::StreamOwned<rustls::ServerConnection, TcpStream>);

impl Read for TlsConn {
    fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(b)
    }
}

impl Write for TlsConn {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.write(b)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

impl Duplex for TlsConn {
    fn set_read_timeout(&mut self, t: Option<Duration>) -> std::io::Result<()> {
        self.0.sock.set_read_timeout(t)
    }
}

impl Conn {
    /// Runs on the accept thread, so nothing here may block for long: a short
    /// write, then discard whatever request bytes already arrived (closing on
    /// unread input resets the connection and loses the 503).
    fn reject_busy(self) {
        fn refuse<S: Read + Write>(s: &mut S) {
            let r = Response::text(503, "server busy").header("Retry-After", "1");
            let _ = write_response(s, &r);
            let mut sink = [0u8; 16384];
            for _ in 0..4 {
                if !matches!(s.read(&mut sink), Ok(n) if n > 0) {
                    break;
                }
            }
        }
        // Non-blocking: a 503 fits in any socket buffer, and a client that
        // has not finished sending is not worth waiting for.
        match self {
            Conn::Tcp(mut s, _) => {
                let _ = s.set_nonblocking(true);
                refuse(&mut s);
                let _ = s.shutdown(NetShutdown::Write);
            }
            // No TLS session yet to say it in: just close.
            Conn::Tls(s, _, _) => {
                let _ = s.shutdown(NetShutdown::Both);
            }
            Conn::Unix(mut s) => {
                let _ = s.set_nonblocking(true);
                refuse(&mut s);
                let _ = s.shutdown(NetShutdown::Write);
            }
        }
    }

    fn serve(self, limits: &Limits, handler: &Handler) {
        match self {
            Conn::Tcp(s, addr) => {
                let _ = s.set_nonblocking(false);
                let _ = s.set_nodelay(true);
                let _ = s.set_read_timeout(Some(limits.read_timeout));
                let _ = s.set_write_timeout(Some(limits.write_timeout));
                let mut s = s;
                handle(&mut s, Peer::Tcp(addr), limits, handler);
                let _ = s.shutdown(NetShutdown::Write);
            }
            Conn::Tls(s, addr, cfg) => {
                let _ = s.set_nonblocking(false);
                let _ = s.set_nodelay(true);
                let _ = s.set_read_timeout(Some(limits.read_timeout));
                let _ = s.set_write_timeout(Some(limits.write_timeout));
                let Ok(conn) = rustls::ServerConnection::new(cfg) else {
                    return;
                };
                let mut t = TlsConn(rustls::StreamOwned::new(conn, s));
                // The handshake (and the client certificate check) first,
                // bounded by the socket timeouts.
                while t.0.conn.is_handshaking() {
                    if let Err(e) = t.0.conn.complete_io(&mut t.0.sock) {
                        eprintln!("isb serve: TLS handshake from {addr}: {e}");
                        let _ = t.0.sock.shutdown(NetShutdown::Both);
                        return;
                    }
                }
                handle(&mut t, Peer::Tcp(addr), limits, handler);
                t.0.conn.send_close_notify();
                let _ = t.0.flush();
                let _ = t.0.sock.shutdown(NetShutdown::Write);
            }
            Conn::Unix(s) => {
                let _ = s.set_nonblocking(false);
                let _ = s.set_read_timeout(Some(limits.read_timeout));
                let _ = s.set_write_timeout(Some(limits.write_timeout));
                let uid = peer_uid(&s);
                let mut s = s;
                handle(&mut s, Peer::Unix { uid }, limits, handler);
                let _ = s.shutdown(NetShutdown::Write);
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn peer_uid(s: &UnixStream) -> Option<u32> {
    rustix::net::sockopt::socket_peercred(s)
        .ok()
        .map(|c| c.uid.as_raw())
}

#[cfg(target_os = "macos")]
fn peer_uid(s: &UnixStream) -> Option<u32> {
    use std::os::fd::AsRawFd;
    let (mut uid, mut gid) = (0, 0);
    // SAFETY: a valid socket fd and two out-parameters.
    (unsafe { libc::getpeereid(s.as_raw_fd(), &mut uid, &mut gid) } == 0).then_some(uid)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn peer_uid(_: &UnixStream) -> Option<u32> {
    None
}

/// Serve one request on `stream`. Generic so tests can drive it in memory.
pub(crate) fn handle<S: Duplex>(stream: &mut S, peer: Peer, limits: &Limits, handler: &Handler) {
    match read_request(stream, peer, limits) {
        Ok(req) => {
            let resp = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(&req)))
                .unwrap_or_else(|_| {
                    eprintln!("isb serve: handler panicked on {} {}", req.method, req.path);
                    Response::text(500, "internal error")
                });
            if let Some(up) = &resp.upgrade {
                let f = up.lock().unwrap().take();
                if write_upgrade(stream, &resp).is_ok() {
                    if let Some(f) = f {
                        f(stream);
                    }
                }
                return;
            }
            let _ = write_response(stream, &resp);
        }
        Err(Some(resp)) => {
            let _ = write_response(stream, &resp);
            // The client may still be sending a body we refused. Closing with
            // unread input makes the kernel send RST, which can destroy the
            // response before the client reads it; swallow a bounded amount.
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut sink = [0u8; 16384];
            let mut left = limits.max_body_bytes;
            while left > 0 && Instant::now() < deadline {
                match stream.read(&mut sink) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => left = left.saturating_sub(n),
                }
            }
        }
        Err(None) => {}
    }
}

/// Read and parse one request. `Err(Some(resp))` is a refusal to send back;
/// `Err(None)` means the peer went away and there is nobody to answer.
pub(crate) fn read_request<S: Read + Write>(
    stream: &mut S,
    peer: Peer,
    limits: &Limits,
) -> std::result::Result<Request, Option<Response>> {
    let started = Instant::now();
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut chunk = [0u8; 8192];
    let head_len = loop {
        if !buf.is_empty() {
            let mut hs = [httparse::EMPTY_HEADER; 100];
            let mut r = httparse::Request::new(&mut hs);
            match r.parse(&buf) {
                Ok(httparse::Status::Complete(n)) => break n,
                Ok(httparse::Status::Partial) => {}
                Err(httparse::Error::TooManyHeaders) => {
                    return Err(Some(Response::text(431, "too many headers")));
                }
                Err(e) => return Err(Some(Response::text(400, &format!("bad request: {e}")))),
            }
        }
        if buf.len() > limits.max_header_bytes {
            return Err(Some(Response::text(431, "request headers too large")));
        }
        if started.elapsed() > limits.read_timeout {
            return Err(Some(Response::text(408, "request timeout")));
        }
        match stream.read(&mut chunk) {
            Ok(0) => return Err(None),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return Err(if buf.is_empty() {
                    None
                } else {
                    Some(Response::text(408, "request timeout"))
                });
            }
            Err(_) => return Err(None),
        }
    };
    if head_len > limits.max_header_bytes {
        return Err(Some(Response::text(431, "request headers too large")));
    }

    let mut hs = [httparse::EMPTY_HEADER; 100];
    let mut r = httparse::Request::new(&mut hs);
    // Parsed successfully above on the same bytes.
    let _ = r.parse(&buf[..head_len]);
    let method = r.method.unwrap_or_default().to_string();
    let target = r.path.unwrap_or_default();
    let headers: Vec<(String, String)> = r
        .headers
        .iter()
        .map(|h| {
            (
                h.name.to_string(),
                String::from_utf8_lossy(h.value).trim().to_string(),
            )
        })
        .collect();
    let (path, query) =
        split_target(target).ok_or_else(|| Some(Response::text(400, "bad request target")))?;

    fn find<'a>(
        headers: &'a [(String, String)],
        name: &'a str,
    ) -> impl Iterator<Item = &'a (String, String)> {
        headers
            .iter()
            .filter(move |(k, _)| k.eq_ignore_ascii_case(name))
    }
    if find(&headers, "transfer-encoding").next().is_some() {
        return Err(Some(Response::text(
            411,
            "chunked request bodies are not supported; send Content-Length",
        )));
    }
    let mut length: Option<usize> = None;
    for (_, v) in find(&headers, "content-length") {
        let n: usize = v
            .parse()
            .map_err(|_| Some(Response::text(400, "bad Content-Length")))?;
        if length.is_some_and(|l| l != n) {
            return Err(Some(Response::text(400, "conflicting Content-Length")));
        }
        length = Some(n);
    }
    let length = length.unwrap_or(0);
    if length > limits.max_body_bytes {
        return Err(Some(Response::text(413, "request body too large")));
    }

    let mut body = buf.split_off(head_len);
    body.truncate(length);
    if body.len() < length
        && find(&headers, "expect").any(|(_, v)| v.eq_ignore_ascii_case("100-continue"))
    {
        // curl waits for this (up to a second) before sending a large body.
        stream
            .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
            .map_err(|_| None)?;
    }
    while body.len() < length {
        if started.elapsed() > limits.read_timeout {
            return Err(Some(Response::text(408, "request timeout")));
        }
        let want = (length - body.len()).min(chunk.len());
        match stream.read(&mut chunk[..want]) {
            Ok(0) => return Err(None),
            Ok(n) => body.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return Err(Some(Response::text(408, "request timeout")));
            }
            Err(_) => return Err(None),
        }
    }
    Ok(Request {
        method,
        path,
        query,
        headers,
        body,
        peer,
    })
}

/// Origin-form `/path?query`, or absolute-form `http://host/path?query` with
/// the authority dropped.
fn split_target(target: &str) -> Option<(String, Option<String>)> {
    let t = if target.starts_with('/') {
        target
    } else {
        let rest = target
            .strip_prefix("http://")
            .or_else(|| target.strip_prefix("https://"))?;
        match rest.find('/') {
            Some(i) => &rest[i..],
            None => "/",
        }
    };
    Some(match t.split_once('?') {
        Some((p, q)) => (p.to_string(), Some(q.to_string())),
        None => (t.to_string(), None),
    })
}

/// The head of a `101 Switching Protocols`; no body follows.
fn write_upgrade<W: Write>(w: &mut W, r: &Response) -> std::io::Result<()> {
    let mut head = String::from("HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\n");
    for (k, v) in &r.headers {
        if k.eq_ignore_ascii_case("content-length")
            || k.eq_ignore_ascii_case("connection")
            || k.eq_ignore_ascii_case("transfer-encoding")
            || k.contains(['\r', '\n', ':'])
            || v.contains(['\r', '\n'])
        {
            continue;
        }
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    w.write_all(head.as_bytes())?;
    w.flush()
}

pub(crate) fn write_response<W: Write>(w: &mut W, r: &Response) -> std::io::Result<()> {
    let mut head = format!("HTTP/1.1 {} {}\r\n", r.status, reason(r.status));
    for (k, v) in &r.headers {
        // Framing is ours; and a value with a line break would split the header.
        if k.eq_ignore_ascii_case("content-length")
            || k.eq_ignore_ascii_case("connection")
            || k.eq_ignore_ascii_case("transfer-encoding")
            || k.contains(['\r', '\n', ':'])
            || v.contains(['\r', '\n'])
        {
            continue;
        }
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    if let Some(stream) = &r.stream {
        // No length: the body runs until the connection closes.
        head.push_str("Cache-Control: no-store\r\nConnection: close\r\n\r\n");
        w.write_all(head.as_bytes())?;
        w.flush()?;
        let f = stream.lock().unwrap().take();
        return match f {
            Some(f) => {
                f(w)?;
                w.flush()
            }
            None => Ok(()),
        };
    }
    head.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        r.body.len()
    ));
    w.write_all(head.as_bytes())?;
    w.write_all(&r.body)?;
    w.flush()
}

fn reason(status: u16) -> &'static str {
    match status {
        100 => "Continue",
        101 => "Switching Protocols",
        200 => "OK",
        202 => "Accepted",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        406 => "Not Acceptable",
        408 => "Request Timeout",
        411 => "Length Required",
        413 => "Content Too Large",
        415 => "Unsupported Media Type",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        _ => "Status",
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// An in-memory connection: each read returns at most one of `chunks`,
    /// and everything written is recorded.
    pub(crate) struct Mock {
        pub chunks: std::collections::VecDeque<Vec<u8>>,
        pub output: Vec<u8>,
    }

    impl Mock {
        pub fn new(input: impl Into<Vec<u8>>) -> Self {
            Mock::chunked(vec![input.into()])
        }

        pub fn chunked(chunks: Vec<Vec<u8>>) -> Self {
            Mock {
                chunks: chunks.into(),
                output: Vec::new(),
            }
        }
    }

    impl Read for Mock {
        fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
            let Some(front) = self.chunks.front_mut() else {
                return Ok(0);
            };
            let n = front.len().min(b.len());
            b[..n].copy_from_slice(&front[..n]);
            front.drain(..n);
            if front.is_empty() {
                self.chunks.pop_front();
            }
            Ok(n)
        }
    }

    impl Duplex for Mock {
        fn set_read_timeout(&mut self, _: Option<Duration>) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Write for Mock {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.output.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn parse(raw: &[u8], limits: &Limits) -> std::result::Result<Request, Option<Response>> {
        read_request(&mut Mock::new(raw), Peer::Unix { uid: None }, limits)
    }

    fn status(raw: &[u8], limits: &Limits) -> u16 {
        match parse(raw, limits) {
            Ok(_) => 0,
            Err(Some(r)) => r.status,
            Err(None) => 1,
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn unix_peer_is_this_uid() {
        let (a, _b) = UnixStream::pair().unwrap();
        assert_eq!(peer_uid(&a), Some(rustix::process::getuid().as_raw()));
    }

    #[test]
    fn parses_a_post() {
        let r = parse(
            b"POST /mcp?x=1 HTTP/1.1\r\nHost: a\r\nCONTENT-type: application/json\r\nContent-Length: 4\r\n\r\nabcdEXTRA",
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.method, "POST");
        assert_eq!(r.path, "/mcp");
        assert_eq!(r.query.as_deref(), Some("x=1"));
        assert_eq!(r.header("content-type"), Some("application/json"));
        assert_eq!(r.body, b"abcd");
    }

    #[test]
    fn absolute_form_target() {
        let r = parse(
            b"GET http://localhost:1/healthz HTTP/1.1\r\n\r\n",
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.path, "/healthz");
    }

    #[test]
    fn limits_and_malformed_input() {
        let small = Limits {
            max_header_bytes: 64,
            max_body_bytes: 8,
            ..Limits::default()
        };
        let big_header = format!("GET / HTTP/1.1\r\nX: {}\r\n\r\n", "a".repeat(200));
        assert_eq!(status(big_header.as_bytes(), &small), 431);
        // Never completes and never stops: still bounded by the header limit.
        let endless = format!("GET / HTTP/1.1\r\nX: {}", "a".repeat(10_000));
        assert_eq!(status(endless.as_bytes(), &small), 431);
        assert_eq!(
            status(b"POST / HTTP/1.1\r\nContent-Length: 9\r\n\r\n", &small),
            413
        );
        let l = Limits::default();
        assert_eq!(
            status(b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n", &l),
            411
        );
        assert_eq!(
            status(b"POST / HTTP/1.1\r\nContent-Length: x\r\n\r\n", &l),
            400
        );
        assert_eq!(
            status(
                b"POST / HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\nab",
                &l
            ),
            400
        );
        assert_eq!(status(b"\x00\x01garbage\r\n\r\n", &l), 400);
        assert_eq!(status(b"GET nope HTTP/1.1\r\n\r\n", &l), 400);
        let many: String = (0..150).map(|i| format!("H{i}: v\r\n")).collect();
        assert_eq!(
            status(format!("GET / HTTP/1.1\r\n{many}\r\n").as_bytes(), &l),
            431
        );
        // Peer hung up mid-request or mid-body: nobody to answer.
        assert_eq!(status(b"GET / HTTP/1.1\r\nHost:", &l), 1);
        assert_eq!(
            status(b"POST / HTTP/1.1\r\nContent-Length: 5\r\n\r\nab", &l),
            1
        );
        assert_eq!(status(b"", &l), 1);
    }

    #[test]
    fn expect_continue_is_answered() {
        let mut m = Mock::chunked(vec![
            b"POST / HTTP/1.1\r\nExpect: 100-continue\r\nContent-Length: 2\r\n\r\n".to_vec(),
            b"hi".to_vec(),
        ]);
        let r = read_request(&mut m, Peer::Unix { uid: None }, &Limits::default()).unwrap();
        assert_eq!(r.body, b"hi");
        assert_eq!(m.output, b"HTTP/1.1 100 Continue\r\n\r\n");
    }

    #[test]
    fn writes_framed_responses() {
        let h: Handler = Arc::new(|r: &Request| {
            Response::text(200, &r.path)
                .header("Content-Length", "999")
                .header("X-Bad", "a\r\nInjected: 1")
        });
        let mut m = Mock::new(&b"GET /hello HTTP/1.1\r\n\r\n"[..]);
        handle(&mut m, Peer::Unix { uid: None }, &Limits::default(), &h);
        let out = String::from_utf8(m.output).unwrap();
        assert!(out.starts_with("HTTP/1.1 200 OK\r\n"), "{out}");
        assert!(out.contains("Content-Length: 7\r\n"), "{out}");
        assert!(out.contains("Connection: close\r\n"));
        assert!(!out.contains("999") && !out.contains("Injected"));
        assert!(out.ends_with("\r\n\r\n/hello\n"));
    }

    #[test]
    fn an_upgrade_hands_over_the_connection() {
        let h: Handler = Arc::new(|_: &Request| {
            Response::upgrade(
                "websocket",
                Box::new(|s: &mut dyn Duplex| {
                    let _ = s.write_all(b"after");
                }),
            )
            .header("Sec-WebSocket-Accept", "k")
        });
        let mut m = Mock::new(&b"GET /ws HTTP/1.1\r\nUpgrade: websocket\r\n\r\n"[..]);
        handle(&mut m, Peer::Unix { uid: None }, &Limits::default(), &h);
        let out = String::from_utf8(m.output).unwrap();
        assert!(
            out.starts_with("HTTP/1.1 101 Switching Protocols\r\n"),
            "{out}"
        );
        assert!(out.contains("Connection: Upgrade\r\n") && out.contains("Upgrade: websocket\r\n"));
        assert!(out.contains("Sec-WebSocket-Accept: k\r\n"));
        assert!(!out.contains("Content-Length") && !out.contains("close"));
        assert!(out.ends_with("\r\n\r\nafter"), "{out}");
    }

    #[test]
    fn handler_panic_is_a_500() {
        let h: Handler = Arc::new(|_: &Request| panic!("boom"));
        let mut m = Mock::new(&b"GET / HTTP/1.1\r\n\r\n"[..]);
        handle(&mut m, Peer::Unix { uid: None }, &Limits::default(), &h);
        assert!(m.output.starts_with(b"HTTP/1.1 500 "));
    }

    #[test]
    fn tcp_bind_is_loopback_only() {
        assert!(HttpListener::bind_tcp("0.0.0.0:0").is_err());
        assert!(HttpListener::bind_tcp("192.0.2.1:0").is_err());
        let l = HttpListener::bind_tcp("127.0.0.1:0").unwrap();
        assert!(l.local_addr().unwrap().ip().is_loopback());
    }

    #[test]
    fn unix_bind_permissions_and_stale_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/isb.sock");
        let l = HttpListener::bind_unix(&path).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        // A live socket is not stolen.
        assert!(HttpListener::bind_unix(&path).is_err());
        drop(l);
        // A stale one (left behind by a crash) is replaced.
        drop(UnixListener::bind(&path).unwrap());
        assert!(path.exists());
        let l2 = HttpListener::bind_unix(&path).unwrap();
        drop(l2);
        assert!(!path.exists(), "socket removed on drop");
        // Something that is not a socket is never deleted.
        std::fs::write(&path, b"x").unwrap();
        assert!(HttpListener::bind_unix(&path).is_err());
        assert!(path.exists());
    }

    #[test]
    fn connection_cap_and_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cap.sock");
        let listener = HttpListener::bind_unix(&path).unwrap();
        let shutdown = Shutdown::new();
        let srv = HttpServer::new(
            Limits {
                max_connections: 1,
                ..Limits::default()
            },
            shutdown.clone(),
        );
        let gate = Arc::new(std::sync::Barrier::new(2));
        let g = gate.clone();
        let h: Handler = Arc::new(move |_: &Request| {
            g.wait();
            Response::text(200, "ok")
        });
        let s2 = srv.clone();
        let t = std::thread::spawn(move || s2.run(listener, h));

        let send = |p: &Path| {
            let mut s = UnixStream::connect(p).unwrap();
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            s.write_all(b"GET / HTTP/1.1\r\n\r\n").unwrap();
            s
        };
        let mut first = send(&path);
        // Wait until the first connection holds the only slot.
        let t0 = Instant::now();
        while srv.active() == 0 && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut second = send(&path);
        let mut out = String::new();
        second.read_to_string(&mut out).unwrap();
        assert!(out.starts_with("HTTP/1.1 503 "), "{out}");
        gate.wait();
        out.clear();
        first.read_to_string(&mut out).unwrap();
        assert!(out.starts_with("HTTP/1.1 200 "), "{out}");

        shutdown.trigger();
        t.join().unwrap().unwrap();
        srv.drain(Duration::from_secs(5));
        assert_eq!(srv.active(), 0);
        assert!(!path.exists());
    }
}
