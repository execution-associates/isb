//! The control plane's side of the wire to an agent: HTTPS with its client
//! certificate, one request per connection (the agent's server closes after
//! answering), and the terminal websocket.

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::wire::Assertion;
use crate::error::{Error, Result};
use crate::org::OrgId;
use crate::server::ssh::{self, SshRequest};
use crate::server::terminal::{Pty, PtyOutput, TermRequest};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RESPONSE: usize = 64 * 1024 * 1024;

type Tls = rustls::StreamOwned<rustls::ClientConnection, TcpStream>;

/// How the control plane reaches one agent.
#[derive(Clone)]
pub struct AgentClient {
    pub name: String,
    pub address: String,
    pub port: u16,
    tls: Arc<rustls::ClientConfig>,
}

impl std::fmt::Debug for AgentClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "AgentClient({} at {}:{})",
            self.name, self.address, self.port
        )
    }
}

/// An answer: status, headers, body.
#[derive(Debug, Clone)]
pub struct Answer {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

fn unreachable(name: &str, step: &str, e: impl std::fmt::Display) -> Error {
    Error::OperationFailed {
        step: format!("reach server {name} ({step})"),
        message: e.to_string(),
    }
}

impl AgentClient {
    pub fn new(name: &str, address: &str, port: u16, tls: Arc<rustls::ClientConfig>) -> Self {
        AgentClient {
            name: name.into(),
            address: address.into(),
            port,
            tls,
        }
    }

    fn server_name(&self) -> Result<rustls::pki_types::ServerName<'static>> {
        rustls::pki_types::ServerName::try_from(self.address.clone())
            .map_err(|e| Error::invalid(format!("server address {}: {e}", self.address)))
    }

    fn authority(&self) -> String {
        if self.address.contains(':') {
            format!("[{}]:{}", self.address, self.port)
        } else {
            format!("{}:{}", self.address, self.port)
        }
    }

    /// A TLS connection with the handshake done (so a refused certificate
    /// fails here, naming the step).
    fn connect(&self, timeout: Duration) -> Result<Tls> {
        let addr = (self.address.as_str(), self.port)
            .to_socket_addrs()
            .map_err(|e| unreachable(&self.name, "resolve", e))?
            .next()
            .ok_or_else(|| unreachable(&self.name, "resolve", "no address"))?;
        let sock = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT.min(timeout))
            .map_err(|e| unreachable(&self.name, "connect", e))?;
        sock.set_read_timeout(Some(CONNECT_TIMEOUT.min(timeout)))?;
        sock.set_write_timeout(Some(CONNECT_TIMEOUT.min(timeout)))?;
        let _ = sock.set_nodelay(true);
        let conn = rustls::ClientConnection::new(self.tls.clone(), self.server_name()?)
            .map_err(|e| unreachable(&self.name, "TLS", e))?;
        let mut s = rustls::StreamOwned::new(conn, sock);
        while s.conn.is_handshaking() {
            s.conn
                .complete_io(&mut s.sock)
                .map_err(|e| unreachable(&self.name, "TLS handshake", e))?;
        }
        Ok(s)
    }

    /// SHA-256 of the certificate the agent presents now.
    pub fn peer_fingerprint(&self) -> Result<String> {
        let s = self.connect(CONNECT_TIMEOUT)?;
        let c = s
            .conn
            .peer_certificates()
            .and_then(|c| c.first())
            .ok_or_else(|| unreachable(&self.name, "TLS", "no server certificate"))?;
        Ok(super::pki::hex(
            ring::digest::digest(&ring::digest::SHA256, c.as_ref()).as_ref(),
        ))
    }

    /// One request; the answer ends where the agent closes.
    pub fn request(
        &self,
        method: &str,
        path: &str,
        headers: &[(String, String)],
        body: &[u8],
        timeout: Duration,
    ) -> Result<Answer> {
        let started = Instant::now();
        let mut s = self.connect(timeout)?;
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nUser-Agent: isb/{}\r\nContent-Length: {}\r\nConnection: close\r\n",
            self.authority(),
            env!("CARGO_PKG_VERSION"),
            body.len()
        );
        for (k, v) in headers {
            if k.contains(['\r', '\n', ':']) || v.contains(['\r', '\n']) {
                continue;
            }
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str("\r\n");
        let io = |e: std::io::Error| unreachable(&self.name, "send", e);
        s.write_all(head.as_bytes()).map_err(io)?;
        s.write_all(body).map_err(io)?;
        s.flush().map_err(io)?;
        let mut buf = Vec::with_capacity(8192);
        let mut chunk = [0u8; 16384];
        loop {
            let left = timeout.saturating_sub(started.elapsed());
            if left.is_zero() {
                return Err(Error::Io(std::io::Error::new(
                    ErrorKind::TimedOut,
                    format!(
                        "server {} did not answer {method} {path} within {timeout:?}",
                        self.name
                    ),
                )));
            }
            s.sock.set_read_timeout(Some(left))?;
            match s.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    if buf.len() > MAX_RESPONSE {
                        return Err(Error::Protocol(format!(
                            "server {}: response over {MAX_RESPONSE} bytes",
                            self.name
                        )));
                    }
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                // A peer that closes without close_notify: what came is all.
                Err(e) if e.kind() == ErrorKind::UnexpectedEof => break,
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(e) => return Err(unreachable(&self.name, "read", e)),
            }
        }
        parse_answer(&buf).ok_or_else(|| {
            Error::Protocol(format!(
                "server {}: {method} {path}: an unreadable response",
                self.name
            ))
        })
    }

    /// Call a tool on the agent as `who`, in `org`'s endpoint (or the
    /// unscoped one for cross-org reads).
    pub fn call(
        &self,
        tool: &str,
        args: &Value,
        who: &Assertion,
        org: Option<&OrgId>,
        request_id: Option<&str>,
        timeout: Duration,
    ) -> Result<Value> {
        let path = match org {
            Some(o) => format!("/orgs/{o}/api/v1/tools/{tool}"),
            None => format!("/api/v1/tools/{tool}"),
        };
        let mut h = vec![
            ("Authorization".to_string(), who.header()),
            ("Content-Type".to_string(), "application/json".to_string()),
        ];
        if let Some(r) = request_id {
            h.push(("X-Request-Id".into(), r.into()));
        }
        let a = self.request("POST", &path, &h, &serde_json::to_vec(args)?, timeout)?;
        tool_answer(&self.name, &a)
    }

    /// GET or POST an internal JSON route as the control plane itself.
    pub fn internal(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        timeout: Duration,
    ) -> Result<Value> {
        let h = vec![
            (
                "Authorization".to_string(),
                Assertion::control_plane().header(),
            ),
            ("Content-Type".to_string(), "application/json".to_string()),
        ];
        let body = match body {
            Some(b) => serde_json::to_vec(b)?,
            None => Vec::new(),
        };
        let a = self.request(method, path, &h, &body, timeout)?;
        internal_answer(&self.name, path, &a)
    }

    /// A terminal on the agent, as `who`.
    pub fn terminal(&self, who: &Assertion, org: &OrgId, t: &TermRequest) -> Result<Box<dyn Pty>> {
        let path = format!("/orgs/{org}/api/v1/terminal?{}", t.query());
        let target = format!("{}:{}", self.name, t.target());
        Ok(Box::new(self.websocket(&path, who, &[], target)?))
    }

    /// An SSH session on the agent, as `who`, letting in `keys` (the
    /// caller's account's keys, as the control plane read them just now).
    pub fn ssh(
        &self,
        who: &Assertion,
        org: &OrgId,
        s: &SshRequest,
        keys: &[String],
    ) -> Result<Box<dyn Pty>> {
        let req = SshRequest {
            instance: s.instance.clone(),
            keys_of: None,
            forwarded_keys: None,
        };
        let path = format!("/orgs/{org}/api/v1/ssh?{}", req.query());
        let header = (ssh::KEYS_HEADER, ssh::keys_header(keys));
        let target = format!("{}:{}", self.name, s.instance);
        Ok(Box::new(self.websocket(&path, who, &[header], target)?))
    }

    /// A websocket to `path` on the agent, as `who`.
    fn websocket(
        &self,
        path: &str,
        who: &Assertion,
        headers: &[(&str, String)],
        target: String,
    ) -> Result<RemotePty> {
        use tungstenite::client::IntoClientRequest;
        let s = self.connect(CONNECT_TIMEOUT)?;
        let url = format!("wss://{}{path}", self.authority());
        let mut req = url
            .into_client_request()
            .map_err(|e| Error::WebSocket(e.to_string()))?;
        let mut put = |k: &str, v: &str| -> Result<()> {
            let name = tungstenite::http::HeaderName::from_bytes(k.as_bytes())
                .map_err(|_| Error::invalid(format!("header {k}")))?;
            let value = v
                .parse()
                .map_err(|_| Error::invalid(format!("header {k}")))?;
            req.headers_mut().insert(name, value);
            Ok(())
        };
        put("authorization", &who.header())?;
        for (k, v) in headers {
            put(k, v)?;
        }
        let (ws, _) = tungstenite::client(req, s).map_err(|e| match e {
            tungstenite::HandshakeError::Failure(tungstenite::Error::Http(r)) => {
                let body = r
                    .body()
                    .as_ref()
                    .map(|b| String::from_utf8_lossy(b).into_owned())
                    .unwrap_or_default();
                let status = r.status().as_u16();
                let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                let message = match v["message"].as_str().unwrap_or(&body) {
                    // The handshake can end before the body is read.
                    "" => format!("server {} refused it (HTTP {status})", self.name),
                    m => m.to_string(),
                };
                match v["error"].as_str() {
                    Some("forbidden") => Error::Forbidden(message),
                    None if matches!(status, 401 | 403) => Error::Forbidden(message),
                    code => Error::Remote {
                        code: code.unwrap_or("server_error").into(),
                        message,
                        data: Value::Null,
                    },
                }
            }
            e => Error::WebSocket(format!("server {}: {e}", self.name)),
        })?;
        Ok(RemotePty {
            ws,
            done: false,
            target,
        })
    }

    /// POST raw bytes to an internal route as the control plane itself.
    pub fn internal_bytes(&self, path: &str, body: &[u8], timeout: Duration) -> Result<Value> {
        let h = vec![
            (
                "Authorization".to_string(),
                Assertion::control_plane().header(),
            ),
            (
                "Content-Type".to_string(),
                "application/octet-stream".to_string(),
            ),
        ];
        let a = self.request("POST", path, &h, body, timeout)?;
        internal_answer(&self.name, path, &a)
    }
}

/// `HTTP/1.1 <status>`, headers, and the body (to Content-Length if given).
pub fn parse_answer(buf: &[u8]) -> Option<Answer> {
    let mut hs = [httparse::EMPTY_HEADER; 64];
    let mut r = httparse::Response::new(&mut hs);
    let n = match r.parse(buf).ok()? {
        httparse::Status::Complete(n) => n,
        httparse::Status::Partial => return None,
    };
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
    let mut body = buf[n..].to_vec();
    if let Some(l) = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse::<usize>().ok())
    {
        if body.len() < l {
            return None;
        }
        body.truncate(l);
    }
    Some(Answer {
        status: r.code?,
        headers,
        body,
    })
}

/// An internal route's answer: its JSON on 200, else the error it gave.
fn internal_answer(name: &str, path: &str, a: &Answer) -> Result<Value> {
    let v: Value = serde_json::from_slice(&a.body).unwrap_or(Value::Null);
    if a.status != 200 {
        return Err(Error::Remote {
            code: v["error"].as_str().unwrap_or("server_error").into(),
            message: format!(
                "server {name}: {path}: HTTP {}: {}",
                a.status,
                v["message"]
                    .as_str()
                    .unwrap_or_else(|| std::str::from_utf8(&a.body).unwrap_or(""))
            ),
            data: Value::Null,
        });
    }
    Ok(v)
}

/// A REST tool answer: `{"result"}` on 200, else `{"error","message","data"}`.
pub fn tool_answer(server: &str, a: &Answer) -> Result<Value> {
    let v: Value = serde_json::from_slice(&a.body).map_err(|e| {
        Error::Protocol(format!(
            "server {server}: HTTP {}, undecodable body ({e}): {}",
            a.status,
            String::from_utf8_lossy(&a.body[..a.body.len().min(200)])
        ))
    })?;
    if a.status == 200 {
        return Ok(v.get("result").cloned().unwrap_or(Value::Null));
    }
    let code = v["error"].as_str().unwrap_or("server_error").to_string();
    let message = v["message"]
        .as_str()
        .map(String::from)
        .unwrap_or_else(|| format!("HTTP {}", a.status));
    // The agent's "forbidden: x" keeps its prefix out of the relayed text.
    let message = match code.as_str() {
        "forbidden" => message
            .strip_prefix("forbidden: ")
            .unwrap_or(&message)
            .to_string(),
        _ => message,
    };
    if code == "forbidden" {
        return Err(Error::Forbidden(message));
    }
    Err(Error::Remote {
        code,
        message,
        data: v.get("data").cloned().unwrap_or(Value::Null),
    })
}

/// A terminal or SSH session on an agent, bridged as if it were local.
struct RemotePty {
    ws: tungstenite::WebSocket<Tls>,
    done: bool,
    target: String,
}

fn would_block(e: &tungstenite::Error) -> bool {
    matches!(e, tungstenite::Error::Io(i) if matches!(i.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut))
}

impl RemotePty {
    fn blocking(&mut self) {
        let s = &self.ws.get_ref().sock;
        let _ = s.set_nonblocking(false);
        let _ = s.set_read_timeout(Some(Duration::from_secs(10)));
    }

    fn send(&mut self, m: tungstenite::Message) -> Result<()> {
        self.blocking();
        self.ws.send(m).map_err(|e| Error::WebSocket(e.to_string()))
    }
}

impl Pty for RemotePty {
    fn input(&mut self, data: &[u8]) -> Result<()> {
        self.send(tungstenite::Message::binary(data.to_vec()))
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        let _ = self.send(tungstenite::Message::text(
            json!({"type": "resize", "cols": cols, "rows": rows}).to_string(),
        ));
    }

    fn output(&mut self, wait: Duration) -> PtyOutput {
        if self.done {
            return PtyOutput::Exit(None);
        }
        {
            let s = &self.ws.get_ref().sock;
            if wait.is_zero() {
                let _ = s.set_nonblocking(true);
            } else {
                let _ = s.set_nonblocking(false);
                let _ = s.set_read_timeout(Some(wait));
            }
        }
        match self.ws.read() {
            Ok(tungstenite::Message::Binary(b)) => PtyOutput::Data(b.to_vec()),
            Ok(tungstenite::Message::Text(t)) => {
                let v: Value = serde_json::from_str(t.as_str()).unwrap_or(Value::Null);
                match v["type"].as_str() {
                    Some("exit") => {
                        self.done = true;
                        PtyOutput::Exit(v["code"].as_i64().map(|c| c as i32))
                    }
                    Some("error") => {
                        self.done = true;
                        PtyOutput::Failed(v["message"].as_str().unwrap_or("error").to_string())
                    }
                    // What the agent's sshd accepted, for the control
                    // plane's own checks; never passed on to the client.
                    Some("accepted") => PtyOutput::Note(v),
                    _ => PtyOutput::Idle,
                }
            }
            Ok(tungstenite::Message::Close(_)) => {
                self.done = true;
                PtyOutput::Exit(None)
            }
            Ok(_) => PtyOutput::Idle,
            Err(e) if would_block(&e) => PtyOutput::Idle,
            Err(e) => {
                self.done = true;
                PtyOutput::Failed(format!("the server's terminal broke: {e}"))
            }
        }
    }

    fn close(&mut self) {
        if !self.done {
            self.done = true;
            self.blocking();
            let _ = self.ws.close(None);
            let _ = self.ws.flush();
        }
    }

    fn target(&self) -> Option<String> {
        Some(self.target.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_parse_and_map_errors() {
        let a = parse_answer(b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\n\r\n{\"result\":1}xyz")
            .unwrap();
        assert_eq!(a.status, 200);
        assert_eq!(tool_answer("s", &a).unwrap(), json!(1));
        let a = parse_answer(
            b"HTTP/1.1 403 Forbidden\r\n\r\n{\"error\":\"forbidden\",\"message\":\"forbidden: no access to org b\"}",
        )
        .unwrap();
        match tool_answer("s", &a).unwrap_err() {
            Error::Forbidden(m) => assert_eq!(m, "no access to org b"),
            e => panic!("{e:?}"),
        }
        let a = parse_answer(b"HTTP/1.1 404 Not Found\r\n\r\n{\"error\":\"not_found\",\"message\":\"app x not found\"}").unwrap();
        assert!(tool_answer("s", &a).unwrap_err().is_not_found());
        assert!(parse_answer(b"HTTP/1.1 200 OK\r\nContent-Length: 99\r\n\r\n{}").is_none());
    }
}
