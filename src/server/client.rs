//! Calling `isb serve` tools from the CLI over its unix socket.
//!
//! One `tools/call` per connection with no `initialize`: the server is
//! stateless, and a handshake would only double the round trips.

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::error::{Error, Result};

const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

/// Call a tool and return its result. A tool that fails comes back as
/// [`Error::Remote`] carrying the server-side error's code and data; a
/// protocol error (unknown tool, bad arguments) as [`Error::Invalid`] or
/// [`Error::Protocol`].
pub fn call_tool(socket: &Path, name: &str, args: Value, timeout: Duration) -> Result<Value> {
    let r = rpc(
        socket,
        "tools/call",
        json!({"name": name, "arguments": args}),
        timeout,
    )?;
    unwrap_result(r)
}

/// A `tools/call` result as the tool's value, or its error.
fn unwrap_result(r: Value) -> Result<Value> {
    let structured = r.get("structuredContent").cloned();
    if r.get("isError").and_then(Value::as_bool) == Some(true) {
        let s = structured.unwrap_or(Value::Null);
        let text = r
            .pointer("/content/0/text")
            .and_then(Value::as_str)
            .unwrap_or("tool failed");
        return Err(Error::Remote {
            code: s
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or("tool_error")
                .to_string(),
            message: s
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or(text)
                .to_string(),
            data: s.get("data").cloned().unwrap_or(Value::Null),
        });
    }
    // Undo the server's wrapping of non-object results.
    Ok(match structured {
        Some(Value::Object(mut o)) if o.len() == 1 && o.contains_key("result") => {
            o.remove("result").unwrap_or(Value::Null)
        }
        Some(v) => v,
        None => r
            .pointer("/content/0/text")
            .and_then(Value::as_str)
            .map(|t| serde_json::from_str(t).unwrap_or_else(|_| json!(t)))
            .unwrap_or(Value::Null),
    })
}

/// The tools the socket's listener exposes, as `tools/list` describes them.
pub fn list_tools(socket: &Path, timeout: Duration) -> Result<Vec<Value>> {
    let r = rpc(socket, "tools/list", json!({}), timeout)?;
    match r.get("tools") {
        Some(Value::Array(a)) => Ok(a.clone()),
        _ => Err(Error::Protocol("tools/list: no tools array".into())),
    }
}

fn rpc(socket: &Path, method: &str, params: Value, timeout: Duration) -> Result<Value> {
    let body = serde_json::to_vec(
        &json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}),
    )?;
    let stream = UnixStream::connect(socket).map_err(|e| {
        std::io::Error::new(
            e.kind(),
            format!("cannot connect to isb serve at {}: {e}", socket.display()),
        )
    })?;
    let what = format!("{method} on {}", socket.display());
    let (status, bytes) = exchange(Stream::Unix(stream), "POST", "/mcp", &body, timeout, &what)?;
    let v: Value = serde_json::from_slice(&bytes).map_err(|e| {
        Error::Protocol(format!(
            "{what}: HTTP {status}, undecodable body ({e}): {}",
            String::from_utf8_lossy(&bytes[..bytes.len().min(200)])
        ))
    })?;
    if let Some(err) = v.get("error") {
        let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
        let msg = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("error")
            .to_string();
        return Err(if code == -32602 {
            Error::Invalid(msg)
        } else {
            Error::Protocol(format!("{method}: {msg} ({code})"))
        });
    }
    if status != 200 {
        return Err(Error::Protocol(format!("{what}: HTTP {status}")));
    }
    v.get("result")
        .cloned()
        .ok_or_else(|| Error::Protocol(format!("{what}: response has no result")))
}

/// `GET /healthz` on a TCP address: the status and the decoded body.
pub fn healthz(addr: &str, timeout: Duration) -> Result<(u16, Value)> {
    let target = addr
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| Error::invalid(format!("{addr} does not resolve")))?;
    let s = TcpStream::connect_timeout(&target, timeout)?;
    let (status, body) = exchange(
        Stream::Tcp(s),
        "GET",
        "/healthz",
        b"",
        timeout,
        &format!("GET http://{addr}/healthz"),
    )?;
    Ok((status, serde_json::from_slice(&body).unwrap_or(Value::Null)))
}

enum Stream {
    Tcp(TcpStream),
    Unix(UnixStream),
}

impl Stream {
    fn set_timeouts(&self, d: Duration) -> std::io::Result<()> {
        match self {
            Stream::Tcp(s) => {
                s.set_read_timeout(Some(d))?;
                s.set_write_timeout(Some(d))
            }
            Stream::Unix(s) => {
                s.set_read_timeout(Some(d))?;
                s.set_write_timeout(Some(d))
            }
        }
    }

    fn io(&mut self) -> &mut dyn ReadWrite {
        match self {
            Stream::Tcp(s) => s,
            Stream::Unix(s) => s,
        }
    }
}

trait ReadWrite: Read + Write {}
impl<T: Read + Write> ReadWrite for T {}

/// One request on a fresh connection, bounded by `timeout` overall. The
/// server closes after answering, so the response ends at EOF.
fn exchange(
    s: Stream,
    method: &str,
    path: &str,
    body: &[u8],
    timeout: Duration,
    what: &str,
) -> Result<(u16, Vec<u8>)> {
    exchange_with(s, method, path, body, timeout, what, &[])
}

/// [`exchange`] with extra request headers.
fn exchange_with(
    mut s: Stream,
    method: &str,
    path: &str,
    body: &[u8],
    timeout: Duration,
    what: &str,
    extra: &[(&str, &str)],
) -> Result<(u16, Vec<u8>)> {
    let started = Instant::now();
    let timed_out = || {
        Error::Io(std::io::Error::new(
            ErrorKind::TimedOut,
            format!("isb serve did not answer {what} within {timeout:?}"),
        ))
    };
    let to_err = |e: std::io::Error| {
        if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) {
            timed_out()
        } else {
            Error::Io(e)
        }
    };
    s.set_timeouts(timeout)?;
    let mut more = String::new();
    for (k, v) in extra {
        more.push_str(&format!("{k}: {v}\r\n"));
    }
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nUser-Agent: isb/{}\r\n\
         Content-Type: application/json\r\nAccept: application/json, text/event-stream\r\n\
         MCP-Protocol-Version: {}\r\nContent-Length: {}\r\n{more}Connection: close\r\n\r\n",
        env!("CARGO_PKG_VERSION"),
        super::mcp::PROTOCOL_VERSIONS[0],
        body.len()
    );
    s.io().write_all(head.as_bytes()).map_err(to_err)?;
    s.io().write_all(body).map_err(to_err)?;
    s.io().flush().map_err(to_err)?;

    let mut buf = Vec::with_capacity(8192);
    let mut chunk = [0u8; 16384];
    loop {
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(timed_out());
        }
        // macOS refuses setsockopt (EINVAL) once the server has closed; the
        // read cannot block then, so the previous timeout is as good.
        let _ = s.set_timeouts(remaining);
        match s.io().read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(to_err(e)),
        }
        if buf.len() > MAX_RESPONSE_BYTES {
            return Err(Error::Protocol(format!("{what}: response too large")));
        }
    }
    let mut hs = [httparse::EMPTY_HEADER; 64];
    let mut r = httparse::Response::new(&mut hs);
    let n = match r.parse(&buf) {
        Ok(httparse::Status::Complete(n)) => n,
        Ok(httparse::Status::Partial) => {
            return Err(Error::Protocol(format!("{what}: truncated response")));
        }
        Err(e) => return Err(Error::Protocol(format!("{what}: bad response: {e}"))),
    };
    let status = r.code.unwrap_or(0);
    let length = r
        .headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case("content-length"))
        .and_then(|h| {
            std::str::from_utf8(h.value)
                .ok()?
                .trim()
                .parse::<usize>()
                .ok()
        });
    let mut body = buf.split_off(n);
    if let Some(len) = length {
        if body.len() < len {
            return Err(Error::Protocol(format!("{what}: truncated response")));
        }
        body.truncate(len);
    }
    Ok((status, body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::{Listener, Registry, Shutdown, Tool, ToolPolicy, serve_until};
    use std::sync::Arc;

    #[test]
    fn end_to_end_over_unix_socket() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("run/isb.sock");
        let mut reg = Registry::new();
        reg.register(Tool::new(
            "add",
            "Add two numbers",
            json!({"type": "object"}),
            |a, c| {
                let x = a["a"].as_i64().unwrap_or(0) + a["b"].as_i64().unwrap_or(0);
                Ok(json!({"sum": x, "trusted": c.is_trusted()}))
            },
        ))
        .unwrap();
        reg.register(Tool::new("len", "", json!({}), |a, _| {
            Ok(json!(a["s"].as_str().unwrap_or("").len()))
        }))
        .unwrap();
        reg.register(Tool::new("gone", "", json!({}), |_, _| {
            Err(Error::NotFound("stack web".into()))
        }))
        .unwrap();
        reg.register(Tool::new("hidden", "", json!({}), |_, _| Ok(json!({}))))
            .unwrap();

        let shutdown = Shutdown::new();
        let stop = shutdown.clone();
        let s = sock.clone();
        let server = std::thread::spawn(move || {
            serve_until(
                vec![Listener::unix(s).policy(ToolPolicy::from_lists("", "hid*"))],
                reg,
                Arc::new(|| (true, json!({"ok": true}))),
                stop,
            )
        });
        let t0 = Instant::now();
        while !sock.exists() && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        let to = Duration::from_secs(10);

        let v = call_tool(&sock, "add", json!({"a": 2, "b": 3}), to).unwrap();
        assert_eq!(v, json!({"sum": 5, "trusted": true}));
        // A non-object result is unwrapped again.
        assert_eq!(
            call_tool(&sock, "len", json!({"s": "abcd"}), to).unwrap(),
            json!(4)
        );

        let e = call_tool(&sock, "gone", json!({}), to).unwrap_err();
        assert!(e.is_not_found(), "{e:?}");
        assert_eq!(e.to_string(), "stack web not found");

        let e = call_tool(&sock, "hidden", json!({}), to).unwrap_err();
        assert!(
            matches!(e, Error::Invalid(ref m) if m.contains("unknown tool")),
            "{e:?}"
        );

        let names: Vec<String> = list_tools(&sock, to)
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names, ["add", "len", "gone"]);

        // Concurrent callers each get their own connection and answer.
        let workers: Vec<_> = (0..16)
            .map(|i| {
                let s = sock.clone();
                std::thread::spawn(move || {
                    call_tool(&s, "add", json!({"a": i, "b": 1}), to).unwrap()["sum"]
                        .as_i64()
                        .unwrap()
                })
            })
            .collect();
        for (i, w) in workers.into_iter().enumerate() {
            assert_eq!(w.join().unwrap(), i as i64 + 1);
        }

        shutdown.trigger();
        server.join().unwrap().unwrap();
        assert!(!sock.exists(), "socket removed on shutdown");
        let e = call_tool(&sock, "add", json!({}), to).unwrap_err();
        assert!(e.to_string().contains("cannot connect to isb serve"), "{e}");
    }

    #[test]
    fn healthz_over_tcp() {
        let shutdown = Shutdown::new();
        // Find a free port, then serve on it.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let addr = format!("127.0.0.1:{port}");
        let (a, stop) = (addr.clone(), shutdown.clone());
        let server = std::thread::spawn(move || {
            serve_until(
                vec![Listener::tcp(a).allow_unauthenticated(true)],
                Registry::new(),
                Arc::new(|| (false, json!({"ok": false, "why": "starting"}))),
                stop,
            )
        });
        let t0 = Instant::now();
        let (status, body) = loop {
            match healthz(&addr, Duration::from_secs(2)) {
                Ok(r) => break r,
                Err(_) if t0.elapsed() < Duration::from_secs(5) => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                Err(e) => panic!("{e}"),
            }
        };
        assert_eq!(status, 503);
        assert_eq!(body["why"], "starting");
        shutdown.trigger();
        server.join().unwrap().unwrap();
    }
}
