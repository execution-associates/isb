//! A small, synchronous incusd REST client over the local unix socket.
//!
//! Deliberately not the `incus` binary: a CLI client that hangs (one `incus init`
//! sat ~11 minutes with no matching server-side operation) cannot be diagnosed or
//! bounded from outside. Here every request has a socket timeout, and every
//! mutation is a server operation waited on with a deadline, so a stall surfaces
//! as the step that stalled.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::Value;

use crate::error::{Error, Result};

/// Deadlines used by the client. Every request has one; there is no unbounded wait
/// anywhere except the output of `exec`, which by design has no default timeout.
#[derive(Debug, Clone)]
pub struct Timeouts {
    /// Socket timeout for a single request/response exchange.
    pub request: Duration,
    /// Deadline for the instance-create operation (includes unpacking the image).
    pub create: Duration,
    /// Deadline for start/stop/restart operations.
    pub state: Duration,
    /// Deadline for any other operation (config updates, deletes, volume creation).
    pub other: Duration,
    /// After a create stalls past its deadline: how long to let the (usually
    /// non-cancellable) operation settle before cleaning up and retrying.
    pub settle: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Timeouts {
            request: Duration::from_secs(30),
            create: Duration::from_secs(600),
            state: Duration::from_secs(120),
            other: Duration::from_secs(120),
            settle: Duration::from_secs(30),
        }
    }
}

/// Connection to incusd.
#[derive(Debug, Clone)]
pub struct Client {
    socket: PathBuf,
    project: Option<String>,
    pub(crate) timeouts: Timeouts,
}

/// The standard incusd response envelope.
#[derive(Debug, Deserialize)]
struct Envelope {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    error: String,
    #[serde(default)]
    metadata: Value,
    #[serde(default)]
    operation: String,
}

/// What a request returned: sync metadata, or an operation to wait on.
#[derive(Debug)]
pub(crate) enum Reply {
    Sync(Value),
    Async { operation: String, metadata: Value },
}

impl Client {
    /// Locate the socket the way the incus tools do: `$INCUS_SOCKET`, else
    /// `$INCUS_DIR/unix.socket`, else `/var/lib/incus/unix.socket`. On macOS
    /// the last fallback is the default `isb machine`'s forwarded socket,
    /// `~/.isb/machine/isb/incus.sock`.
    pub fn default_socket() -> PathBuf {
        if let Some(s) = std::env::var_os("INCUS_SOCKET").filter(|s| !s.is_empty()) {
            return PathBuf::from(s);
        }
        if let Some(d) = std::env::var_os("INCUS_DIR").filter(|s| !s.is_empty()) {
            return PathBuf::from(d).join("unix.socket");
        }
        #[cfg(target_os = "macos")]
        if let Ok(s) = crate::machine::incus_socket(crate::machine::DEFAULT_NAME) {
            return s;
        }
        PathBuf::from("/var/lib/incus/unix.socket")
    }

    /// Client for the default socket and the default project.
    pub fn new() -> Self {
        Client::with_socket(Client::default_socket())
    }

    pub fn with_socket(socket: impl Into<PathBuf>) -> Self {
        Client {
            socket: socket.into(),
            project: None,
            timeouts: Timeouts::default(),
        }
    }

    /// Use an incus project other than `default`.
    pub fn project(mut self, project: impl Into<String>) -> Self {
        let p = project.into();
        self.project = if p.is_empty() || p == "default" {
            None
        } else {
            Some(p)
        };
        self
    }

    pub fn timeouts(mut self, timeouts: Timeouts) -> Self {
        self.timeouts = timeouts;
        self
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    pub fn project_name(&self) -> &str {
        self.project.as_deref().unwrap_or("default")
    }

    pub fn get_timeouts(&self) -> &Timeouts {
        &self.timeouts
    }

    /// `GET /1.0`: server info. Also a cheap reachability check.
    pub fn server_info(&self) -> Result<Value> {
        self.get("/1.0")
    }

    fn with_project(&self, path: &str) -> String {
        match &self.project {
            None => path.to_string(),
            Some(p) => {
                let sep = if path.contains('?') { '&' } else { '?' };
                format!("{path}{sep}project={}", encode_query(p))
            }
        }
    }

    fn connect(&self, timeout: Duration) -> Result<UnixStream> {
        let stream = UnixStream::connect(&self.socket).map_err(|source| Error::Connect {
            socket: self.socket.display().to_string(),
            source,
        })?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        Ok(stream)
    }

    /// One HTTP/1.1 exchange on a fresh connection, bounded by `timeout` overall.
    fn raw(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        if_match: Option<&str>,
        timeout: Duration,
    ) -> Result<RawResponse> {
        let payload = match body {
            Some(v) => serde_json::to_vec(v)?,
            None => Vec::new(),
        };
        let mut headers: Vec<(&str, String)> = Vec::new();
        if body.is_some() {
            headers.push(("Content-Type", "application/json".into()));
        }
        if let Some(etag) = if_match {
            headers.push(("If-Match", etag.to_string()));
        }
        self.raw_bytes(method, path, &payload, &headers, timeout)
    }

    /// [`Client::raw`] with an arbitrary body and headers (the file API).
    fn raw_bytes(
        &self,
        method: &str,
        path: &str,
        payload: &[u8],
        extra_headers: &[(&str, String)],
        timeout: Duration,
    ) -> Result<RawResponse> {
        let path = self.with_project(path);
        let started = Instant::now();
        let to_err = |e: std::io::Error| -> Error {
            if matches!(
                e.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) {
                Error::RequestTimeout {
                    method: method.to_string(),
                    path: path.clone(),
                    timeout,
                }
            } else {
                Error::Io(e)
            }
        };
        let mut stream = self.connect(timeout)?;
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\nHost: incus\r\nUser-Agent: isb/{}\r\nConnection: close\r\n",
            env!("CARGO_PKG_VERSION")
        );
        for (k, v) in extra_headers {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str(&format!("Content-Length: {}\r\n\r\n", payload.len()));
        stream.write_all(head.as_bytes()).map_err(to_err)?;
        stream.write_all(payload).map_err(to_err)?;
        stream.flush().map_err(to_err)?;

        let mut buf = Vec::with_capacity(8192);
        let mut chunk = [0u8; 16384];
        loop {
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(to_err(std::io::ErrorKind::TimedOut.into()));
            }
            // macOS refuses setsockopt (EINVAL) once incusd has closed; the
            // read cannot block then, so the previous timeout is as good.
            let _ = stream.set_read_timeout(Some(remaining));
            let n = stream.read(&mut chunk).map_err(to_err)?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            // Stop as soon as a complete response is in hand; incusd honours
            // Connection: close, but there is no need to depend on it.
            if let Some(done) = complete_response(&buf)? {
                return Ok(done);
            }
        }
        complete_response(&buf)?
            .or_else(|| eof_body(&buf))
            .ok_or_else(|| Error::Protocol(format!("truncated response to {method} {path}")))
    }

    /// Send a request and decode the incusd envelope.
    pub(crate) fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        timeout: Duration,
    ) -> Result<Reply> {
        self.request_etag(method, path, body, None, timeout)
            .map(|(r, _)| r)
    }

    /// Like [`Client::request`], optionally sending `If-Match`, and returning the
    /// response ETag.
    pub(crate) fn request_etag(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        if_match: Option<&str>,
        timeout: Duration,
    ) -> Result<(Reply, Option<String>)> {
        let RawResponse {
            status,
            body: bytes,
            etag,
        } = self.raw(method, path, body, if_match, timeout)?;
        let env: Envelope = serde_json::from_slice(&bytes).map_err(|e| {
            Error::Protocol(format!(
                "{method} {path}: HTTP {status}, undecodable body ({e}): {}",
                String::from_utf8_lossy(&bytes[..bytes.len().min(200)])
            ))
        })?;
        if env.kind == "error" || status >= 400 {
            return Err(Error::Api {
                method: method.to_string(),
                path: path.to_string(),
                status,
                message: if env.error.is_empty() {
                    format!("HTTP {status}")
                } else {
                    env.error
                },
            });
        }
        if env.kind == "async" {
            return Ok((
                Reply::Async {
                    operation: env.operation,
                    metadata: env.metadata,
                },
                etag,
            ));
        }
        Ok((Reply::Sync(env.metadata), etag))
    }

    /// `GET` returning the body and its ETag.
    pub(crate) fn get_etag(&self, path: &str) -> Result<(Value, Option<String>)> {
        match self.request_etag("GET", path, None, None, self.timeouts.request)? {
            (Reply::Sync(v), e) => Ok((v, e)),
            (Reply::Async { metadata, .. }, e) => Ok((metadata, e)),
        }
    }

    /// A mutation guarded by `If-Match`, waited on like [`Client::mutate`].
    pub(crate) fn mutate_if_match(
        &self,
        method: &str,
        path: &str,
        body: &Value,
        etag: Option<&str>,
        step: &str,
        deadline: Duration,
    ) -> Result<Value> {
        match self.request_etag(method, path, Some(body), etag, self.timeouts.request)? {
            (Reply::Sync(v), _) => Ok(v),
            (Reply::Async { operation, .. }, _) => self.wait_operation(&operation, step, deadline),
        }
    }

    pub(crate) fn get(&self, path: &str) -> Result<Value> {
        match self.request("GET", path, None, self.timeouts.request)? {
            Reply::Sync(v) => Ok(v),
            Reply::Async { metadata, .. } => Ok(metadata),
        }
    }

    /// `GET` returning `None` on 404.
    pub(crate) fn get_opt(&self, path: &str) -> Result<Option<Value>> {
        match self.get(path) {
            Ok(v) => Ok(Some(v)),
            Err(e) if e.is_not_found() => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Perform a mutation and, if it is an operation, wait for it under `deadline`.
    /// On deadline the operation is cancelled (if incus allows it) and
    /// [`Error::OperationTimeout`] names `step`.
    pub(crate) fn mutate(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        step: &str,
        deadline: Duration,
    ) -> Result<Value> {
        match self.request(method, path, body, self.timeouts.request)? {
            Reply::Sync(v) => Ok(v),
            Reply::Async { operation, .. } => self.wait_operation(&operation, step, deadline),
        }
    }

    /// Wait for an operation to finish. Returns its metadata on success.
    pub fn wait_operation(&self, operation: &str, step: &str, deadline: Duration) -> Result<Value> {
        let started = Instant::now();
        loop {
            let remaining = deadline.saturating_sub(started.elapsed());
            // incus takes whole seconds (0 = answer now). Poll in slices of at most
            // 30s so no single request outlives its socket timeout by much, and
            // finish a sub-second remainder with short client-side sleeps.
            let secs = remaining.as_secs().min(30);
            if let Some(v) = self.poll_operation(operation, step, secs)? {
                return Ok(v);
            }
            if started.elapsed() >= deadline {
                let status = self
                    .get(operation)
                    .ok()
                    .and_then(|op| op.get("status").and_then(Value::as_str).map(String::from))
                    .unwrap_or_else(|| "running".into())
                    .to_lowercase();
                let cancelled = self.cancel_operation(operation).is_ok();
                return Err(Error::OperationTimeout {
                    step: step.to_string(),
                    operation: operation.to_string(),
                    status,
                    waited: started.elapsed(),
                    cancelled,
                });
            }
            if secs == 0 {
                std::thread::sleep(remaining.min(Duration::from_millis(50)));
            }
        }
    }

    /// Wait up to `secs` whole seconds (0 = just look) for an operation.
    /// `Ok(Some(metadata))` on success, `Ok(None)` while it is still running.
    pub(crate) fn poll_operation(
        &self,
        operation: &str,
        step: &str,
        secs: u64,
    ) -> Result<Option<Value>> {
        let op = self.get_with_timeout(
            &format!("{operation}/wait?timeout={secs}"),
            Duration::from_secs(secs) + self.timeouts.request,
        )?;
        let code = op.get("status_code").and_then(Value::as_i64).unwrap_or(0);
        match code {
            200 => Ok(Some(op.get("metadata").cloned().unwrap_or(Value::Null))),
            400 | 401 => {
                let err = op
                    .get("err")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .unwrap_or(if code == 401 {
                        "operation cancelled"
                    } else {
                        "operation failed"
                    });
                Err(Error::OperationFailed {
                    step: step.to_string(),
                    message: err.to_string(),
                })
            }
            _ => Ok(None),
        }
    }

    /// Start an operation without waiting for it; returns its path
    /// (`/1.0/operations/<id>`). Low-level: most callers want the sandbox API.
    pub fn start_operation(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<String> {
        match self.request(method, path, body, self.timeouts.request)? {
            Reply::Async { operation, .. } => Ok(operation),
            Reply::Sync(_) => Err(Error::Protocol(format!(
                "{method} {path} did not start an operation"
            ))),
        }
    }

    fn get_with_timeout(&self, path: &str, timeout: Duration) -> Result<Value> {
        match self.request("GET", path, None, timeout)? {
            Reply::Sync(v) => Ok(v),
            Reply::Async { metadata, .. } => Ok(metadata),
        }
    }

    /// `DELETE /1.0/operations/<id>`. Fails for operations incus will not cancel.
    pub fn cancel_operation(&self, operation: &str) -> Result<()> {
        self.request("DELETE", operation, None, self.timeouts.request)?;
        Ok(())
    }

    /// Write a file into an instance (running or stopped), replacing it.
    /// Parent directories must exist; see [`Client::make_dir`].
    pub fn push_file(
        &self,
        instance: &str,
        path: &str,
        data: &[u8],
        uid: u32,
        gid: u32,
        mode: u32,
    ) -> Result<()> {
        self.file_request(instance, path, data, uid, gid, mode, "file")
    }

    /// Create a directory in an instance; an existing one is left as is.
    pub fn make_dir(
        &self,
        instance: &str,
        path: &str,
        uid: u32,
        gid: u32,
        mode: u32,
    ) -> Result<()> {
        match self.file_request(instance, path, &[], uid, gid, mode, "directory") {
            Err(Error::Api {
                status,
                ref message,
                ..
            }) if status == 409 || message.to_ascii_lowercase().contains("exist") => Ok(()),
            r => r,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn file_request(
        &self,
        instance: &str,
        path: &str,
        data: &[u8],
        uid: u32,
        gid: u32,
        mode: u32,
        kind: &str,
    ) -> Result<()> {
        let url = format!(
            "/1.0/instances/{}/files?path={}",
            encode_segment(instance),
            encode_query(path)
        );
        let headers = [
            ("Content-Type", "application/octet-stream".to_string()),
            ("X-Incus-uid", uid.to_string()),
            ("X-Incus-gid", gid.to_string()),
            ("X-Incus-mode", format!("{mode:04o}")),
            ("X-Incus-type", kind.to_string()),
            ("X-Incus-write", "overwrite".to_string()),
        ];
        let r = self.raw_bytes("POST", &url, data, &headers, self.timeouts.request)?;
        if r.status >= 400 {
            let message = serde_json::from_slice::<Envelope>(&r.body)
                .map(|e| e.error)
                .ok()
                .filter(|e| !e.is_empty())
                .unwrap_or_else(|| format!("HTTP {}", r.status));
            return Err(Error::Api {
                method: "POST".into(),
                path: url,
                status: r.status,
                message,
            });
        }
        Ok(())
    }

    /// A `GET` whose answer is not the JSON envelope (`/1.0/metrics`).
    pub(crate) fn get_raw(&self, path: &str) -> Result<Vec<u8>> {
        let r = self.raw_bytes("GET", path, &[], &[], self.timeouts.request)?;
        match r.status {
            200 => Ok(r.body),
            status => Err(Error::Api {
                method: "GET".into(),
                path: path.into(),
                status,
                message: String::from_utf8_lossy(&r.body[..r.body.len().min(200)]).into_owned(),
            }),
        }
    }

    /// An instance's console log as incus keeps it (an OCI app's output).
    pub fn console_log(&self, instance: &str) -> Result<Vec<u8>> {
        let url = format!("/1.0/instances/{}/console", encode_segment(instance));
        let r = self.raw_bytes("GET", &url, &[], &[], self.timeouts.request)?;
        match r.status {
            200 => Ok(r.body),
            404 => Ok(Vec::new()),
            status => Err(Error::Api {
                method: "GET".into(),
                path: url,
                status,
                message: serde_json::from_slice::<Envelope>(&r.body)
                    .map(|e| e.error)
                    .unwrap_or_default(),
            }),
        }
    }

    /// Read a file from an instance; `None` if it does not exist.
    pub fn read_file(&self, instance: &str, path: &str) -> Result<Option<Vec<u8>>> {
        let url = format!(
            "/1.0/instances/{}/files?path={}",
            encode_segment(instance),
            encode_query(path)
        );
        let r = self.raw_bytes("GET", &url, &[], &[], self.timeouts.request)?;
        match r.status {
            200 => Ok(Some(r.body)),
            404 => Ok(None),
            status => Err(Error::Api {
                method: "GET".into(),
                path: url,
                status,
                message: serde_json::from_slice::<Envelope>(&r.body)
                    .map(|e| e.error)
                    .unwrap_or_default(),
            }),
        }
    }

    /// Open one of an operation's websockets (exec stdin/stdout/stderr/control).
    pub(crate) fn websocket(
        &self,
        operation: &str,
        secret: &str,
    ) -> Result<tungstenite::WebSocket<UnixStream>> {
        let path = self.with_project(&format!(
            "{operation}/websocket?secret={}",
            encode_query(secret)
        ));
        let stream = self.connect(self.timeouts.request)?;
        let url = format!("ws://incus{path}");
        let (ws, _resp) = tungstenite::client::client(url.as_str(), stream)
            .map_err(|e| Error::WebSocket(format!("handshake for {operation}: {e}")))?;
        // Exec output has no default timeout: a quiet process is not a stuck one.
        ws.get_ref().set_read_timeout(None)?;
        ws.get_ref()
            .set_write_timeout(Some(self.timeouts.request))?;
        Ok(ws)
    }
}

impl Default for Client {
    fn default() -> Self {
        Client::new()
    }
}

/// Percent-encode a query value (RFC 3986 unreserved set passes through).
pub(crate) fn encode_query(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Percent-encode one path segment.
pub(crate) fn encode_segment(s: &str) -> String {
    encode_query(s)
}

#[derive(Debug, PartialEq)]
struct RawResponse {
    status: u16,
    body: Vec<u8>,
    etag: Option<String>,
}

/// If `buf` holds a complete HTTP response, return it with its body decoded.
fn complete_response(buf: &[u8]) -> Result<Option<RawResponse>> {
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut resp = httparse::Response::new(&mut headers);
    let head_len = match resp
        .parse(buf)
        .map_err(|e| Error::Protocol(format!("bad HTTP response: {e}")))?
    {
        httparse::Status::Complete(n) => n,
        httparse::Status::Partial => return Ok(None),
    };
    let status = resp.code.unwrap_or(0);
    let mut content_length: Option<usize> = None;
    let mut chunked = false;
    let mut etag = None;
    for h in resp.headers.iter() {
        if h.name.eq_ignore_ascii_case("etag") {
            etag = Some(String::from_utf8_lossy(h.value).trim().to_string());
        }
        if h.name.eq_ignore_ascii_case("content-length") {
            content_length = std::str::from_utf8(h.value)
                .ok()
                .and_then(|v| v.trim().parse().ok());
        } else if h.name.eq_ignore_ascii_case("transfer-encoding")
            && String::from_utf8_lossy(h.value)
                .to_ascii_lowercase()
                .contains("chunked")
        {
            chunked = true;
        }
    }
    let body = &buf[head_len..];
    if chunked {
        return Ok(decode_chunked(body).map(|b| RawResponse {
            status,
            body: b,
            etag,
        }));
    }
    match content_length {
        Some(n) if body.len() >= n => Ok(Some(RawResponse {
            status,
            body: body[..n].to_vec(),
            etag,
        })),
        Some(_) => Ok(None),
        // No length and not chunked: body runs to EOF; the caller decides.
        None => Ok(None),
    }
}

/// Decode a chunked body; `None` if it is not complete yet.
fn decode_chunked(mut body: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let line_end = body.windows(2).position(|w| w == b"\r\n")?;
        let size_str = std::str::from_utf8(&body[..line_end]).ok()?;
        let size = usize::from_str_radix(size_str.split(';').next()?.trim(), 16).ok()?;
        body = &body[line_end + 2..];
        if size == 0 {
            return Some(out);
        }
        if body.len() < size + 2 {
            return None;
        }
        out.extend_from_slice(&body[..size]);
        body = &body[size + 2..];
    }
}

/// When a response had no length and was cut by EOF, treat what we have as the body.
fn eof_body(buf: &[u8]) -> Option<RawResponse> {
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut resp = httparse::Response::new(&mut headers);
    match resp.parse(buf).ok()? {
        httparse::Status::Complete(n) => Some(RawResponse {
            status: resp.code?,
            body: buf[n..].to_vec(),
            etag: None,
        }),
        httparse::Status::Partial => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_content_length_response() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nETag: \"abc\"\r\n\r\nhello";
        let r = complete_response(raw).unwrap().unwrap();
        assert_eq!((r.status, r.body.as_slice()), (200, &b"hello"[..]));
        assert_eq!(r.etag.as_deref(), Some("\"abc\""));
        let partial = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhel";
        assert_eq!(complete_response(partial).unwrap(), None);
    }

    #[test]
    fn parses_chunked_response() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
        let r = complete_response(raw).unwrap().unwrap();
        assert_eq!(r.body, b"hello world".to_vec());
        let partial = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhel";
        assert_eq!(complete_response(partial).unwrap(), None);
    }

    #[test]
    fn encodes_query_values() {
        assert_eq!(encode_query("a b/c"), "a%20b%2Fc");
        assert_eq!(encode_query("abc-_.~"), "abc-_.~");
    }
}
