//! SSH without open ports: `GET /orgs/<org>/api/v1/ssh?instance=NAME`
//! upgrades to a websocket whose binary frames are an SSH connection's
//! bytes, both ways, to an sshd the embedder starts inside the instance
//! (`isb serve`: `sshd -i` through incus exec, docs/guides/ssh.md). Nothing in the
//! instance listens, and nothing on the host opens a port: the websocket is
//! the daemon's own, behind its usual authentication.
//!
//! The gate is the web terminal's ([`super::terminal`]): the caller
//! authenticates as for any tool and is admitted as if calling
//! `sandbox_exec` in the org, so viewers, `read`/`deploy` tokens and
//! `--deny-tools sandbox_exec` are refused. A cookie needs an `Origin`
//! naming this site; the unix socket and bearer tokens need none.
//!
//! Text frames are control messages from the server only: `{"type":
//! "exit", "code"}` and `{"type": "error", "message"}` (why the session
//! ended, for the person running `ssh`).
//!
//! The client half is here too: [`Remote`] (the unix socket, or a URL and a
//! token) and [`pump`], which `isb ssh-proxy` runs between its stdio and the
//! websocket for `ProxyCommand`.

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::mpsc::{TryRecvError, sync_channel};
use std::time::Duration;

use serde_json::Value;
use tungstenite::{Message, WebSocket};

use super::http::{Peer, Request};
use super::mcp::Caller;
use super::terminal::{Limits, Pty, plain_name, query_param};
use crate::error::{Error, Result};

/// What the client asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshRequest {
    /// The instance in the org.
    pub instance: String,
    /// Whose keys to let in, for a caller with no isb account of its own
    /// (the unix socket, a superadmin token): an account's email. Anyone
    /// else may only name themselves.
    pub keys_of: Option<String>,
}

impl SshRequest {
    pub fn query(&self) -> String {
        let mut q = format!("instance={}", self.instance);
        if let Some(e) = &self.keys_of {
            q.push_str(&format!("&as={}", encode(e)));
        }
        q
    }
}

/// Opens the SSH session for `caller` in `org`; refusals become an error
/// frame. The [`Pty`] carries the connection's bytes (no resize).
pub type Ssh =
    Arc<dyn Fn(&Caller, &crate::org::OrgId, &SshRequest) -> Result<Box<dyn Pty>> + Send + Sync>;

static ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// SSH sessions: more than terminals (an editor or herdr holds several),
/// longer-lived, and kept alive by the client's `ServerAliveInterval`.
pub static LIMITS: Limits = Limits {
    active: &ACTIVE,
    max_sessions: 64,
    idle: Duration::from_secs(2 * 3600),
    max_age: Duration::from_secs(24 * 3600),
    busy: "too many SSH sessions are open on this server; close one and try again",
};

/// The request's parameters, or what is wrong with them.
pub fn ssh_request(req: &Request) -> std::result::Result<SshRequest, String> {
    let instance = query_param(req, "instance").ok_or("instance= is required")?;
    if !plain_name(&instance) {
        return Err("instance= is not an instance name".into());
    }
    let keys_of = match query_param(req, "as") {
        None => None,
        Some(e) => {
            let e = decode(&e).ok_or("as= is not an email")?;
            if e.is_empty() || e.len() > 254 || !e.contains('@') || e.contains(char::is_control) {
                return Err("as= is not an email".into());
            }
            Some(e)
        }
    };
    Ok(SshRequest { instance, keys_of })
}

/// The unix socket and bearer tokens need no `Origin`; a cookie needs one
/// naming this site, as for the terminal. An `Origin` that is there must
/// name this site, whatever the transport.
pub fn origin_allowed(req: &Request) -> bool {
    match req.header("origin") {
        Some(_) => super::terminal::origin_allowed(req),
        None => matches!(req.peer, Peer::Unix { .. }) || super::terminal::origin_allowed(req),
    }
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'@' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn decode(s: &str) -> Option<String> {
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

// ---- the client half ----

/// Where `isb serve` is: its unix socket (as the local user), or a URL and
/// an API token (`ISB_URL`, `ISB_TOKEN`).
#[derive(Clone)]
pub enum Remote {
    Socket(PathBuf),
    Url { base: String, token: Option<String> },
}

impl std::fmt::Debug for Remote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Remote::Socket(p) => write!(f, "Socket({})", p.display()),
            // Never the token.
            Remote::Url { base, token } => write!(
                f,
                "Url({base}, token: {})",
                if token.is_some() { "set" } else { "none" }
            ),
        }
    }
}

/// A connection the websocket runs over.
pub trait Conn: Read + Write + Send {
    fn set_timeout(&self, t: Option<Duration>) -> std::io::Result<()>;
}

impl Conn for TcpStream {
    fn set_timeout(&self, t: Option<Duration>) -> std::io::Result<()> {
        self.set_read_timeout(t)
    }
}

impl Conn for UnixStream {
    fn set_timeout(&self, t: Option<Duration>) -> std::io::Result<()> {
        self.set_read_timeout(t)
    }
}

impl Conn for rustls::StreamOwned<rustls::ClientConnection, TcpStream> {
    fn set_timeout(&self, t: Option<Duration>) -> std::io::Result<()> {
        self.sock.set_read_timeout(t)
    }
}

/// A `http(s)://host[:port][/prefix]` base: TLS, host, port, prefix.
pub fn split_base(base: &str) -> Result<(bool, String, u16, String)> {
    let (tls, rest) = if let Some(r) = base.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = base.strip_prefix("http://") {
        (false, r)
    } else {
        return Err(Error::invalid(format!(
            "{base:?}: want an http:// or https:// URL"
        )));
    };
    let (authority, prefix) = match rest.find('/') {
        Some(i) => (&rest[..i], rest[i..].trim_end_matches('/')),
        None => (rest, ""),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !h.ends_with(']') || h.starts_with('[') => match p.parse::<u16>() {
            Ok(p) => (h, p),
            Err(_) => (authority, if tls { 443 } else { 80 }),
        },
        _ => (authority, if tls { 443 } else { 80 }),
    };
    if host.is_empty() {
        return Err(Error::invalid(format!("{base:?}: no host")));
    }
    Ok((tls, host.to_string(), port, prefix.to_string()))
}

impl Remote {
    /// Call a tool in `org`: over the socket's MCP, or the URL's REST
    /// surface (`/orgs/<org>/api/v1/tools/<tool>`).
    pub fn call_tool(&self, org: &str, tool: &str, mut args: Value) -> Result<Value> {
        match self {
            Remote::Socket(p) => {
                args["org"] = Value::String(org.to_string());
                super::client::call_tool(p, tool, args, Duration::from_secs(60))
            }
            Remote::Url { .. } => {
                let (status, v) = self.http(
                    "POST",
                    &format!("/orgs/{org}/api/v1/tools/{tool}"),
                    Some(&args),
                )?;
                if status == 200 {
                    return Ok(v.get("result").cloned().unwrap_or(Value::Null));
                }
                Err(answer_error(status, &v))
            }
        }
    }

    /// One JSON request to the URL (`ISB_URL`): its status and body.
    pub fn http(&self, method: &str, path: &str, body: Option<&Value>) -> Result<(u16, Value)> {
        let Remote::Url { base, token } = self else {
            return Err(Error::invalid(
                "this needs isb serve's URL: pass --url or set ISB_URL",
            ));
        };
        let url = format!("{}{path}", base.trim_end_matches('/'));
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(60)))
            .http_status_as_error(false)
            .user_agent(concat!("isb/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        let auth = token.as_ref().map(|t| format!("Bearer {}", t.trim()));
        let payload = match body {
            Some(b) => serde_json::to_vec(b)?,
            None => Vec::new(),
        };
        macro_rules! go {
            ($req:expr) => {{
                let mut r = $req.header("X-Isb-Csrf", "1");
                if let Some(a) = &auth {
                    r = r.header("Authorization", a);
                }
                r
            }};
        }
        let resp = match method {
            "GET" => go!(agent.get(&url)).call(),
            "DELETE" => go!(agent.delete(&url)).call(),
            "POST" => go!(agent.post(&url))
                .header("Content-Type", "application/json")
                .send(&payload[..]),
            m => return Err(Error::invalid(format!("unsupported method {m}"))),
        };
        let mut resp = resp.map_err(|e| Error::invalid(format!("{method} {url}: {e}")))?;
        let status = resp.status().as_u16();
        let text = resp
            .body_mut()
            .with_config()
            .limit(16 << 20)
            .read_to_string()
            .unwrap_or_default();
        Ok((status, serde_json::from_str(&text).unwrap_or(Value::Null)))
    }

    /// Open the websocket at `path` (with its query) on this server.
    pub fn websocket(&self, path: &str) -> Result<WebSocket<Box<dyn Conn>>> {
        use tungstenite::client::IntoClientRequest;
        let timeout = Duration::from_secs(15);
        let (conn, url, token): (Box<dyn Conn>, String, Option<&String>) = match self {
            Remote::Socket(p) => {
                let s = UnixStream::connect(p).map_err(|e| {
                    Error::invalid(format!(
                        "cannot connect to isb serve at {}: {e}",
                        p.display()
                    ))
                })?;
                (Box::new(s), format!("ws://localhost{path}"), None)
            }
            Remote::Url { base, token } => {
                let (tls, host, port, prefix) = split_base(base)?;
                let addr = (host.trim_start_matches('[').trim_end_matches(']'), port)
                    .to_socket_addrs()
                    .map_err(|e| Error::invalid(format!("{host}: {e}")))?
                    .next()
                    .ok_or_else(|| Error::invalid(format!("{host} does not resolve")))?;
                let sock = TcpStream::connect_timeout(&addr, timeout)
                    .map_err(|e| Error::invalid(format!("cannot connect to {base}: {e}")))?;
                let _ = sock.set_nodelay(true);
                sock.set_read_timeout(Some(timeout))?;
                let authority = if (tls && port == 443) || (!tls && port == 80) {
                    host.clone()
                } else {
                    format!("{host}:{port}")
                };
                let scheme = if tls { "wss" } else { "ws" };
                let url = format!("{scheme}://{authority}{prefix}{path}");
                let conn: Box<dyn Conn> = if tls {
                    let name = rustls::pki_types::ServerName::try_from(
                        host.trim_start_matches('[')
                            .trim_end_matches(']')
                            .to_string(),
                    )
                    .map_err(|e| Error::invalid(format!("{host}: {e}")))?;
                    let c = rustls::ClientConnection::new(crate::net::default_tls(), name)
                        .map_err(|e| Error::invalid(format!("TLS: {e}")))?;
                    Box::new(rustls::StreamOwned::new(c, sock))
                } else {
                    Box::new(sock)
                };
                (conn, url, token.as_ref())
            }
        };
        conn.set_timeout(Some(timeout))?;
        let mut req = url
            .into_client_request()
            .map_err(|e| Error::WebSocket(e.to_string()))?;
        if let Some(t) = token {
            req.headers_mut().insert(
                "authorization",
                format!("Bearer {}", t.trim())
                    .parse()
                    .map_err(|_| Error::invalid("the token is not a valid header value"))?,
            );
        }
        let (ws, _) = tungstenite::client(req, conn).map_err(|e| match e {
            tungstenite::HandshakeError::Failure(tungstenite::Error::Http(r)) => {
                let status = r.status().as_u16();
                let body = r
                    .body()
                    .as_ref()
                    .map(|b| String::from_utf8_lossy(b).into_owned())
                    .unwrap_or_default();
                let v: Value = serde_json::from_str(&body).unwrap_or_else(|_| match body.trim() {
                    // The handshake often ends before the body is read:
                    // say what the status means here.
                    "" => serde_json::json!({ "message": match status {
                        401 => "the API token was refused (unknown, expired or revoked)",
                        403 => "refused: SSH needs exec in the org (members and up; not viewers, and not read- or deploy-scoped tokens), from this site",
                        404 => "no SSH here: no such org, or SSH is off on this server (--deny-tools sandbox_exec)",
                        _ => "the server refused the connection",
                    }}),
                    b => serde_json::json!({ "message": b }),
                });
                answer_error(status, &v)
            }
            e => Error::WebSocket(e.to_string()),
        })?;
        Ok(ws)
    }
}

/// A REST error answer (`{"error", "message"}`) as an isb error.
pub fn answer_error(status: u16, v: &Value) -> Error {
    let message = v["message"]
        .as_str()
        .map(String::from)
        .unwrap_or_else(|| format!("HTTP {status}"));
    match status {
        401 | 403 => Error::Forbidden(message),
        404 => Error::NotFound(message),
        _ => Error::Remote {
            code: v["error"].as_str().unwrap_or("server_error").into(),
            message,
            data: v.get("data").cloned().unwrap_or(Value::Null),
        },
    }
}

fn would_block(e: &tungstenite::Error) -> bool {
    matches!(e, tungstenite::Error::Io(i) if matches!(i.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut))
}

/// Shuttle bytes between `input`/`output` (ssh's end of a `ProxyCommand`)
/// and the websocket until either side ends. `Ok` when the session ended
/// normally; the server's reason otherwise.
pub fn pump<S: Conn + ?Sized>(
    ws: &mut WebSocket<Box<S>>,
    input: impl Read + Send + 'static,
    mut output: impl Write,
) -> Result<()> {
    const CHUNK: usize = 32 * 1024;
    let (tx, rx) = sync_channel::<Option<Vec<u8>>>(64);
    std::thread::spawn(move || {
        let mut input = input;
        let mut buf = vec![0u8; CHUNK];
        loop {
            match input.read(&mut buf) {
                Ok(0) | Err(_) => {
                    let _ = tx.send(None);
                    return;
                }
                Ok(n) => {
                    if tx.send(Some(buf[..n].to_vec())).is_err() {
                        return;
                    }
                }
            }
        }
    });
    ws.get_ref().set_timeout(Some(Duration::from_millis(10)))?;
    loop {
        match ws.read() {
            Ok(Message::Binary(b)) => {
                output.write_all(&b)?;
                output.flush()?;
            }
            Ok(Message::Text(t)) => {
                let v: Value = serde_json::from_str(t.as_str()).unwrap_or(Value::Null);
                match v["type"].as_str() {
                    Some("error") => {
                        return Err(Error::Remote {
                            code: "ssh".into(),
                            message: v["message"].as_str().unwrap_or("refused").to_string(),
                            data: Value::Null,
                        });
                    }
                    Some("exit") => return Ok(()),
                    _ => {}
                }
            }
            Ok(Message::Close(_)) => return Ok(()),
            Ok(_) => {}
            Err(e) if would_block(&e) => {}
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                return Ok(());
            }
            Err(e) => return Err(Error::WebSocket(e.to_string())),
        }
        // What ssh wrote meanwhile, all of it.
        loop {
            match rx.try_recv() {
                Ok(Some(d)) => match ws.send(Message::binary(d)) {
                    Ok(()) => {}
                    Err(e) if would_block(&e) => {}
                    Err(e) => return Err(Error::WebSocket(e.to_string())),
                },
                Ok(None) | Err(TryRecvError::Disconnected) => {
                    // ssh is done with us.
                    let _ = ws.close(None);
                    let _ = ws.flush();
                    return Ok(());
                }
                Err(TryRecvError::Empty) => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(query: &str, peer: Peer, headers: &[(&str, &str)]) -> Request {
        Request {
            method: "GET".into(),
            path: "/orgs/acme/api/v1/ssh".into(),
            query: Some(query.into()),
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            body: vec![],
            peer,
        }
    }

    fn tcp() -> Peer {
        Peer::Tcp("127.0.0.1:5000".parse().unwrap())
    }

    #[test]
    fn parses_requests() {
        let r = ssh_request(&req("instance=box", tcp(), &[])).unwrap();
        assert_eq!(
            r,
            SshRequest {
                instance: "box".into(),
                keys_of: None,
            }
        );
        let r = ssh_request(&req("instance=box&as=a%2Bb%40example.com", tcp(), &[])).unwrap();
        assert_eq!(r.keys_of.as_deref(), Some("a+b@example.com"));
        assert_eq!(r.query(), "instance=box&as=a%2Bb@example.com");
        assert_eq!(ssh_request(&req(&r.query(), tcp(), &[])).unwrap(), r);
        for bad in [
            "",
            "instance=",
            "instance=Box",
            "instance=../x",
            "instance=a&as=nobody",
            "instance=a&as=%zz",
        ] {
            assert!(ssh_request(&req(bad, tcp(), &[])).is_err(), "{bad}");
        }
    }

    #[test]
    fn origin_rules() {
        let host = ("Host", "isb.example.com");
        // The unix socket: no Origin needed, but a foreign one is refused.
        assert!(origin_allowed(&req("", Peer::Unix { uid: None }, &[])));
        assert!(!origin_allowed(&req(
            "",
            Peer::Unix { uid: None },
            &[host, ("Origin", "https://evil.example")]
        )));
        // A bearer token over TCP: none needed either.
        assert!(origin_allowed(&req(
            "",
            tcp(),
            &[host, ("Authorization", "Bearer x")]
        )));
        // A cookie: only from this site.
        assert!(!origin_allowed(&req(
            "",
            tcp(),
            &[host, ("Cookie", "isb_session=x")]
        )));
        assert!(!origin_allowed(&req(
            "",
            tcp(),
            &[
                host,
                ("Cookie", "isb_session=x"),
                ("Origin", "https://evil.example")
            ]
        )));
        assert!(origin_allowed(&req(
            "",
            tcp(),
            &[
                host,
                ("Cookie", "isb_session=x"),
                ("Origin", "https://isb.example.com")
            ]
        )));
    }

    #[test]
    fn splits_bases() {
        assert_eq!(
            split_base("https://isb.example.com").unwrap(),
            (true, "isb.example.com".into(), 443, "".into())
        );
        assert_eq!(
            split_base("http://127.0.0.1:8092/").unwrap(),
            (false, "127.0.0.1".into(), 8092, "".into())
        );
        assert_eq!(
            split_base("https://h.example:8443/isb/").unwrap(),
            (true, "h.example".into(), 8443, "/isb".into())
        );
        assert!(split_base("isb.example.com").is_err());
        assert!(split_base("https://").is_err());
        let r = Remote::Url {
            base: "https://x".into(),
            token: Some("isb_tok_secret".into()),
        };
        assert!(!format!("{r:?}").contains("secret"));
    }

    /// The client's pump against a server bridge over a socket pair: bytes
    /// both ways, and the server's error frame becomes the error.
    #[test]
    fn pumps_bytes_both_ways() {
        use super::super::http::Duplex;
        use super::super::terminal::{PtyOutput, bridge};
        use std::sync::mpsc::{Receiver, Sender, channel};

        struct Upper(Sender<PtyOutput>, Receiver<PtyOutput>);
        impl Pty for Upper {
            fn input(&mut self, d: &[u8]) -> crate::Result<()> {
                if d == b"bye" {
                    self.0
                        .send(PtyOutput::Failed("the key was removed".into()))
                        .unwrap();
                } else {
                    self.0
                        .send(PtyOutput::Data(d.to_ascii_uppercase()))
                        .unwrap();
                }
                Ok(())
            }
            fn resize(&mut self, _: u16, _: u16) {}
            fn output(&mut self, w: Duration) -> PtyOutput {
                self.1.recv_timeout(w).unwrap_or(PtyOutput::Idle)
            }
            fn close(&mut self) {}
        }
        let (a, b) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            let mut s = a;
            let d: &mut dyn Duplex = &mut s;
            let mut ws = WebSocket::from_raw_socket(d, tungstenite::protocol::Role::Server, None);
            let (tx, rx) = channel();
            bridge(
                &mut ws,
                Box::new(Upper(tx, rx)),
                Duration::from_secs(5),
                Duration::from_secs(5),
            );
        });
        let conn: Box<UnixStream> = Box::new(b);
        let mut ws = WebSocket::from_raw_socket(conn, tungstenite::protocol::Role::Client, None);
        // Input: "hello", then "bye" a little later (separate frames).
        let (mut w, r) = UnixStream::pair().unwrap();
        let feeder = std::thread::spawn(move || {
            w.write_all(b"hello").unwrap();
            std::thread::sleep(Duration::from_millis(200));
            w.write_all(b"bye").unwrap();
            std::thread::sleep(Duration::from_millis(2000));
        });
        let mut out = Vec::new();
        let e = pump(&mut ws, r, &mut out).unwrap_err();
        assert_eq!(out, b"HELLO");
        assert!(e.to_string().contains("the key was removed"), "{e}");
        server.join().unwrap();
        feeder.join().unwrap();
    }
}
