//! The preview proxy's HTTP: one request to a workspace port, and its
//! answer back, with isb's credentials kept out of it both ways.
//!
//! - **To the workspace** go the method, path, query, body and the app's own
//!   headers and cookies. Never: isb's cookies (`isb_preview`,
//!   `isb_session`), Cloudflare Access' assertion and cookie (which isb would
//!   accept as the user), an isb bearer token, hop-by-hop headers, or
//!   forwarding headers the client made up; isb sets `X-Forwarded-*` itself.
//! - **Back to the browser** goes the app's status, headers and body as
//!   they are, never isb's own headers (its UI's Content-Security-Policy
//!   does not apply to the app), with hop-by-hop headers dropped, cookies
//!   named like isb's dropped, and a cookie's `Domain` attribute removed so
//!   it stays on the preview's own host.
//! - One request per connection, as isb's server works (`Connection:
//!   close`); a websocket upgrade (dev servers' hot reload) is piped both
//!   ways once the app answers 101.

use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown as NetShutdown, SocketAddr, TcpStream};
use std::time::Duration;

use crate::server::http::{Duplex, Peer, Request, Response};

/// The cookie that carries a preview session.
pub(super) const COOKIE: &str = "isb_preview";
/// Cookies never passed to the app nor accepted from it.
const ISB_COOKIES: &[&str] = &[COOKIE, "isb_session", "CF_Authorization"];

const CONNECT: Duration = Duration::from_secs(5);
/// How long the app may take to start answering (a dev server compiling).
const FIRST_BYTE: Duration = Duration::from_secs(120);
/// How long a streamed body may go quiet.
const IDLE: Duration = Duration::from_secs(300);
const MAX_HEAD: usize = 64 * 1024;

/// Request headers never forwarded: hop-by-hop, framing (isb sets it), and
/// forwarding headers isb writes itself.
fn dropped_request_header(k: &str) -> bool {
    let k = k.to_ascii_lowercase();
    matches!(
        k.as_str(),
        "connection"
            | "keep-alive"
            | "proxy-connection"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "expect"
            | "content-length"
            | "forwarded"
            | "x-forwarded-for"
            | "x-forwarded-proto"
            | "x-forwarded-host"
    ) || k.starts_with("cf-access-")
        || k.starts_with("x-isb-")
}

/// A `Cookie` header without isb's cookies; `None` when nothing is left.
pub(super) fn clean_cookie(v: &str) -> Option<String> {
    let kept: Vec<&str> = v
        .split(';')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .filter(|p| {
            let name = p.split('=').next().unwrap_or("").trim();
            !ISB_COOKIES.iter().any(|c| c.eq_ignore_ascii_case(name))
        })
        .collect();
    (!kept.is_empty()).then(|| kept.join("; "))
}

/// Whether the request asks for a websocket.
pub(super) fn wants_websocket(req: &Request) -> bool {
    let has = |h: &str, t: &str| {
        req.header(h)
            .is_some_and(|v| v.split(',').any(|x| x.trim().eq_ignore_ascii_case(t)))
    };
    req.method == "GET" && has("upgrade", "websocket") && has("connection", "upgrade")
}

/// The request as the workspace receives it.
pub(super) fn request_head(req: &Request) -> String {
    let ws = wants_websocket(req);
    let target = match &req.query {
        Some(q) => format!("{}?{q}", req.path),
        None => req.path.clone(),
    };
    let mut h = format!("{} {target} HTTP/1.1\r\n", req.method);
    for (k, v) in &req.headers {
        if dropped_request_header(k) || k.contains(['\r', '\n']) || v.contains(['\r', '\n']) {
            continue;
        }
        let v = if k.eq_ignore_ascii_case("cookie") {
            match clean_cookie(v) {
                Some(c) => c,
                None => continue,
            }
        } else if k.eq_ignore_ascii_case("authorization")
            && v.trim().to_ascii_lowercase().starts_with("bearer isb_")
        {
            continue;
        } else {
            v.clone()
        };
        h.push_str(&format!("{k}: {v}\r\n"));
    }
    let client = match &req.peer {
        Peer::Tcp(a) if a.ip().is_loopback() => req
            .header("cf-connecting-ip")
            .filter(|s| s.parse::<std::net::IpAddr>().is_ok())
            .map(String::from)
            .unwrap_or_else(|| a.ip().to_string()),
        Peer::Tcp(a) => a.ip().to_string(),
        Peer::Unix { .. } => "127.0.0.1".into(),
    };
    let proto = match req.header("x-forwarded-proto") {
        Some(p) if p.eq_ignore_ascii_case("https") => "https",
        _ => "http",
    };
    h.push_str(&format!(
        "X-Forwarded-For: {client}\r\nX-Forwarded-Proto: {proto}\r\n"
    ));
    if let Some(host) = req.header("host").filter(|v| !v.contains(['\r', '\n'])) {
        h.push_str(&format!("X-Forwarded-Host: {host}\r\n"));
    }
    if ws {
        h.push_str("Connection: Upgrade\r\nUpgrade: websocket\r\n");
    } else {
        h.push_str("Connection: close\r\n");
        if !req.body.is_empty() || matches!(req.method.as_str(), "POST" | "PUT" | "PATCH") {
            h.push_str(&format!("Content-Length: {}\r\n", req.body.len()));
        }
    }
    h.push_str("\r\n");
    h
}

/// A `Set-Cookie` as the browser gets it: `None` for a cookie named like
/// one of isb's, else without a `Domain` attribute (host-only).
pub(super) fn clean_set_cookie(v: &str) -> Option<String> {
    let mut parts = v.split(';');
    let first = parts.next()?.trim();
    let name = first.split('=').next().unwrap_or("").trim();
    if ISB_COOKIES.iter().any(|c| c.eq_ignore_ascii_case(name)) {
        return None;
    }
    let mut out = first.to_string();
    for p in parts {
        let p = p.trim();
        if p.is_empty() || p.to_ascii_lowercase().starts_with("domain") {
            continue;
        }
        out.push_str("; ");
        out.push_str(p);
    }
    Some(out)
}

/// How the app's body is delimited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Framing {
    None,
    Length(u64),
    Chunked,
    Close,
}

/// The app's answer head: status, the headers the browser gets, framing.
#[derive(Debug)]
pub(super) struct Head {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub framing: Framing,
    /// Bytes after the head that were already read.
    pub rest: Vec<u8>,
}

/// Parse a response head from `buf`; `Ok(None)` while it is incomplete.
pub(super) fn parse_head(buf: &[u8], head_request: bool) -> Result<Option<Head>, String> {
    let mut hs = [httparse::EMPTY_HEADER; 100];
    let mut r = httparse::Response::new(&mut hs);
    let n = match r.parse(buf) {
        Ok(httparse::Status::Complete(n)) => n,
        Ok(httparse::Status::Partial) => return Ok(None),
        Err(e) => return Err(format!("bad response from the app: {e}")),
    };
    let status = r.code.unwrap_or(502);
    let mut headers = Vec::new();
    let (mut chunked, mut length) = (false, None);
    for h in r.headers.iter() {
        let v = String::from_utf8_lossy(h.value).trim().to_string();
        let k = h.name.to_ascii_lowercase();
        match k.as_str() {
            "transfer-encoding" => chunked = v.to_ascii_lowercase().contains("chunked"),
            "content-length" => length = v.parse::<u64>().ok(),
            "connection" | "keep-alive" | "proxy-connection" | "te" | "trailer" | "upgrade" => {}
            "set-cookie" => {
                if let Some(c) = clean_set_cookie(&v) {
                    headers.push((h.name.to_string(), c));
                }
            }
            _ => headers.push((h.name.to_string(), v)),
        }
    }
    let framing = if head_request || (100..200).contains(&status) || matches!(status, 204 | 304) {
        Framing::None
    } else if chunked {
        Framing::Chunked
    } else if let Some(l) = length {
        Framing::Length(l)
    } else {
        Framing::Close
    };
    Ok(Some(Head {
        status,
        headers,
        framing,
        rest: buf[n..].to_vec(),
    }))
}

fn read_head(up: &mut TcpStream, head_request: bool) -> Result<Head, String> {
    let mut buf = Vec::with_capacity(8192);
    let mut chunk = [0u8; 8192];
    loop {
        if let Some(h) = parse_head(&buf, head_request)? {
            // An informational answer (103 Early Hints) precedes the real one.
            if (102..200).contains(&h.status) {
                buf = h.rest;
                continue;
            }
            return Ok(h);
        }
        if buf.len() > MAX_HEAD {
            return Err("the app's response headers are too large".into());
        }
        match up.read(&mut chunk) {
            Ok(0) => return Err("the app closed the connection without answering".into()),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(format!("the app did not answer: {e}")),
        }
    }
}

/// A reader over the already-read bytes, then the stream.
struct Prefixed<R> {
    pre: std::io::Cursor<Vec<u8>>,
    inner: R,
}

impl<R: Read> Read for Prefixed<R> {
    fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
        let n = self.pre.read(b)?;
        if n > 0 || b.is_empty() {
            return Ok(n);
        }
        self.inner.read(b)
    }
}

fn read_line<R: Read>(r: &mut R) -> std::io::Result<String> {
    let mut line = Vec::new();
    let mut b = [0u8; 1];
    loop {
        if r.read(&mut b)? == 0 {
            return Err(ErrorKind::UnexpectedEof.into());
        }
        if b[0] == b'\n' {
            break;
        }
        line.push(b[0]);
        if line.len() > 4096 {
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                "chunk line too long",
            ));
        }
    }
    Ok(String::from_utf8_lossy(&line).trim().to_string())
}

/// Copy a body as `framing` delimits it, decoding chunks (the browser's
/// copy is delimited by the connection closing).
pub(super) fn copy_body<R: Read>(
    r: &mut R,
    w: &mut dyn Write,
    framing: Framing,
) -> std::io::Result<()> {
    let mut buf = [0u8; 16384];
    match framing {
        Framing::None => {}
        Framing::Length(n) => {
            std::io::copy(&mut r.by_ref().take(n), w)?;
        }
        Framing::Close => loop {
            let n = r.read(&mut buf)?;
            if n == 0 {
                break;
            }
            w.write_all(&buf[..n])?;
            w.flush()?;
        },
        Framing::Chunked => loop {
            let line = read_line(r)?;
            let size = u64::from_str_radix(line.split(';').next().unwrap_or("").trim(), 16)
                .map_err(|_| std::io::Error::new(ErrorKind::InvalidData, "bad chunk size"))?;
            if size == 0 {
                break;
            }
            std::io::copy(&mut r.by_ref().take(size), w)?;
            w.flush()?;
            read_line(r)?;
        },
    }
    w.flush()
}

/// Pipe a websocket both ways until either side closes.
fn pipe(client: &mut dyn Duplex, mut up: TcpStream, early: Vec<u8>) {
    if !early.is_empty()
        && client
            .write_all(&early)
            .and_then(|_| client.flush())
            .is_err()
    {
        return;
    }
    let tick = Some(Duration::from_millis(20));
    let _ = client.set_read_timeout(tick);
    let _ = up.set_read_timeout(tick);
    let quiet =
        |e: &std::io::Error| matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut);
    let mut buf = [0u8; 16384];
    loop {
        match client.read(&mut buf) {
            Ok(0) => break,
            Ok(n) if up.write_all(&buf[..n]).is_err() => break,
            Ok(_) => {}
            Err(e) if quiet(&e) || e.kind() == ErrorKind::Interrupted => {}
            Err(_) => break,
        }
        match up.read(&mut buf) {
            Ok(0) => break,
            Ok(n)
                if client
                    .write_all(&buf[..n])
                    .and_then(|_| client.flush())
                    .is_err() =>
            {
                break;
            }
            Ok(_) => {}
            Err(e) if quiet(&e) || e.kind() == ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    let _ = up.shutdown(NetShutdown::Both);
}

/// Forward `req` to `upstream` and answer with what the app says.
pub(super) fn forward(req: &Request, upstream: SocketAddr) -> Result<Response, String> {
    let mut up = TcpStream::connect_timeout(&upstream, CONNECT)
        .map_err(|e| format!("nothing answers on {upstream}: {e}"))?;
    let _ = up.set_nodelay(true);
    let _ = up.set_write_timeout(Some(Duration::from_secs(30)));
    let _ = up.set_read_timeout(Some(FIRST_BYTE));
    up.write_all(request_head(req).as_bytes())
        .and_then(|_| up.write_all(&req.body))
        .map_err(|e| format!("sending to {upstream}: {e}"))?;
    let head = read_head(&mut up, req.method == "HEAD")?;
    let _ = up.set_read_timeout(Some(IDLE));
    if head.status == 101 && wants_websocket(req) {
        let mut r = Response::upgrade(
            "websocket",
            Box::new(move |client: &mut dyn Duplex| pipe(client, up, head.rest)),
        );
        for (k, v) in head.headers {
            if k.to_ascii_lowercase().starts_with("sec-websocket-") {
                r = r.header(k, v);
            }
        }
        return Ok(r);
    }
    let (framing, rest) = (head.framing, head.rest);
    let mut r = Response::stream(
        head.status,
        "",
        Box::new(move |w: &mut dyn Write| {
            let mut body = Prefixed {
                pre: std::io::Cursor::new(rest),
                inner: up,
            };
            copy_body(&mut body, w, framing)
        }),
    );
    r.headers = head.headers;
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(headers: &[(&str, &str)]) -> Request {
        Request {
            method: "GET".into(),
            path: "/app".into(),
            query: Some("x=1".into()),
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            body: vec![],
            peer: Peer::Tcp("127.0.0.1:5000".parse().unwrap()),
        }
    }

    #[test]
    fn isb_credentials_never_reach_the_app() {
        let r = req(&[
            ("Host", "3000-workspace-acme.localhost:8192"),
            (
                "Cookie",
                "isb_preview=s3cret; theme=dark; isb_session=sess; CF_Authorization=jwt",
            ),
            ("Cf-Access-Jwt-Assertion", "eyJ.jwt"),
            ("Authorization", "Bearer isb_tok_abc"),
            ("X-Isb-Csrf", "1"),
            ("X-Forwarded-For", "6.6.6.6"),
            ("Connection", "keep-alive"),
            ("Accept", "text/html"),
        ]);
        let h = request_head(&r);
        assert!(h.starts_with("GET /app?x=1 HTTP/1.1\r\n"), "{h}");
        assert!(h.contains("Cookie: theme=dark\r\n"), "{h}");
        for leak in [
            "s3cret",
            "sess",
            "jwt",
            "isb_tok",
            "X-Isb",
            "6.6.6.6",
            "keep-alive",
        ] {
            assert!(!h.contains(leak), "{leak} leaked: {h}");
        }
        assert!(h.contains("Accept: text/html\r\n"));
        assert!(h.contains("X-Forwarded-For: 127.0.0.1\r\n"));
        assert!(h.contains("X-Forwarded-Host: 3000-workspace-acme.localhost:8192\r\n"));
        assert!(h.contains("Connection: close\r\n"));
        // The app's own credentials pass.
        let own = request_head(&req(&[("Authorization", "Basic dTpw")]));
        assert!(own.contains("Authorization: Basic dTpw\r\n"));
        // Only isb's cookies: no Cookie header at all.
        let only = request_head(&req(&[("Cookie", "isb_preview=a")]));
        assert!(!only.to_ascii_lowercase().contains("cookie"));
    }

    #[test]
    fn websocket_upgrades_are_forwarded_as_upgrades() {
        let r = req(&[
            ("Upgrade", "websocket"),
            ("Connection", "keep-alive, Upgrade"),
            ("Sec-WebSocket-Key", "k"),
            ("Sec-WebSocket-Protocol", "vite-hmr"),
        ]);
        assert!(wants_websocket(&r));
        let h = request_head(&r);
        assert!(
            h.contains("Connection: Upgrade\r\nUpgrade: websocket\r\n"),
            "{h}"
        );
        assert!(h.contains("Sec-WebSocket-Protocol: vite-hmr\r\n"));
        assert!(!wants_websocket(&req(&[("Upgrade", "websocket")])));
    }

    #[test]
    fn the_app_cannot_set_isb_cookies_or_cookies_for_other_hosts() {
        assert_eq!(clean_set_cookie("isb_preview=x; Path=/"), None);
        assert_eq!(clean_set_cookie("ISB_SESSION=x"), None);
        assert_eq!(
            clean_set_cookie("sid=1; Domain=localhost; Path=/; HttpOnly").as_deref(),
            Some("sid=1; Path=/; HttpOnly")
        );
        let head = b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nSet-Cookie: isb_session=evil\r\nSet-Cookie: a=b; domain=.example.com\r\nConnection: keep-alive\r\nContent-Security-Policy: default-src *\r\nContent-Length: 2\r\n\r\nhi";
        let h = parse_head(head, false).unwrap().unwrap();
        assert_eq!(h.status, 200);
        assert_eq!(h.framing, Framing::Length(2));
        assert_eq!(h.rest, b"hi");
        let names: Vec<String> = h.headers.iter().map(|(k, v)| format!("{k}: {v}")).collect();
        assert_eq!(
            names,
            vec![
                "Content-Type: text/html",
                "Set-Cookie: a=b",
                // The app's own policy, not isb's UI's.
                "Content-Security-Policy: default-src *",
            ]
        );
        assert!(parse_head(b"HTTP/1.1 200 OK\r\n", false).unwrap().is_none());
        let h = parse_head(b"HTTP/1.1 304 Not Modified\r\n\r\n", false)
            .unwrap()
            .unwrap();
        assert_eq!(h.framing, Framing::None);
        let h = parse_head(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\n", true)
            .unwrap()
            .unwrap();
        assert_eq!(h.framing, Framing::None);
    }

    #[test]
    fn chunked_bodies_are_decoded() {
        let mut r: &[u8] = b"5\r\nhello\r\n6;ext=1\r\n world\r\n0\r\n\r\n";
        let mut out = Vec::new();
        copy_body(&mut r, &mut out, Framing::Chunked).unwrap();
        assert_eq!(out, b"hello world");
        let mut r: &[u8] = b"abcdef";
        let mut out = Vec::new();
        copy_body(&mut r, &mut out, Framing::Length(3)).unwrap();
        assert_eq!(out, b"abc");
        let mut bad: &[u8] = b"zz\r\n";
        assert!(copy_body(&mut bad, &mut Vec::new(), Framing::Chunked).is_err());
    }

    #[test]
    fn a_request_goes_through_and_its_answer_comes_back() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let app = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut got = Vec::new();
            let mut b = [0u8; 4096];
            while !got.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = s.read(&mut b).unwrap();
                got.extend_from_slice(&b[..n]);
            }
            s.write_all(b"HTTP/1.1 201 Created\r\nTransfer-Encoding: chunked\r\nX-App: 1\r\n\r\n3\r\nabc\r\n0\r\n\r\n")
                .unwrap();
            String::from_utf8(got).unwrap()
        });
        let r = forward(&req(&[("Cookie", "isb_preview=x; a=1")]), addr).unwrap();
        assert_eq!(r.status, 201);
        assert_eq!(r.get_header("x-app"), Some("1"));
        assert!(r.get_header("transfer-encoding").is_none());
        let f = r.stream.unwrap().lock().unwrap().take().unwrap();
        let mut body = Vec::new();
        f(&mut body).unwrap();
        assert_eq!(body, b"abc");
        let seen = app.join().unwrap();
        assert!(
            seen.contains("Cookie: a=1\r\n") && !seen.contains("isb_preview"),
            "{seen}"
        );
        // Nothing listening: said so, not hung.
        let free = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        assert!(forward(&req(&[]), free).is_err());
    }
}
