//! A stand-in incusd for unit tests: answers each request on a unix socket
//! from a fixed table of `METHOD /path` prefixes, so code that talks to
//! incus can be tested without one.

use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::Client;

/// A canned answer: an HTTP status and the envelope's `metadata` (or, for
/// a status of 400 and up, its `error`).
pub(crate) struct Route {
    pub prefix: &'static str,
    pub status: u16,
    pub body: Value,
}

/// Serve `routes` until the test ends. The first route whose prefix starts
/// `METHOD /path?query` answers; anything else is a 404.
pub(crate) fn serve(routes: Vec<Route>) -> (tempfile::TempDir, Client) {
    let dir = tempfile::tempdir().unwrap();
    let sock: PathBuf = dir.path().join("incus.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut s) = conn else { return };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            // The head, then as much body as Content-Length says.
            let head_end = loop {
                let n = s.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break None;
                }
                buf.extend_from_slice(&chunk[..n]);
                if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break Some(i + 4);
                }
            };
            let Some(head_end) = head_end else { continue };
            let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
            let len: usize = head
                .lines()
                .find_map(|l| l.strip_prefix("Content-Length: "))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
            while buf.len() < head_end + len {
                let n = s.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            let line = head.lines().next().unwrap_or("");
            let req = line.rsplit_once(' ').map(|(r, _)| r).unwrap_or(line);
            let (status, env) = match routes.iter().find(|r| req.starts_with(r.prefix)) {
                Some(r) if r.status >= 400 => (
                    r.status,
                    json!({"type": "error", "error": r.body, "error_code": r.status}),
                ),
                Some(r) => (
                    r.status,
                    json!({"type": "sync", "status_code": 200, "metadata": r.body}),
                ),
                None => (
                    404,
                    json!({"type": "error", "error": "not found", "error_code": 404}),
                ),
            };
            let body = env.to_string();
            let _ = write!(
                s,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    let client = Client::with_socket(sock);
    (dir, client)
}
