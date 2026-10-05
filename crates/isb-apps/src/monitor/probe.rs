//! The probes: one HTTP(S) request or TCP connect, under a deadline, held
//! to [`net`]'s address policy.
//!
//! A URL a member typed is resolved here, every address it resolves to is
//! checked, and the connection goes to a checked address (never re-resolved),
//! exactly as for notification channels. An app's own endpoint (a replica,
//! a published port), which isb found by reference rather than a member
//! typing an address, is dialled directly with `connect_to`.
//!
//! Errors name the host, never the full URL: a query string may hold a
//! token.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::net::{self, Net, Target};

/// The most of a response body read (keywords are looked for in it).
pub const MAX_BODY: usize = 256 * 1024;
/// Redirects followed at most, when following.
pub const MAX_REDIRECTS: usize = 5;

/// One HTTP check.
#[derive(Clone)]
pub struct HttpProbe {
    pub url: String,
    /// `GET` or `HEAD`.
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub timeout: Duration,
    pub follow_redirects: bool,
    pub allow_private: bool,
    /// Dial this address instead of resolving the URL's host: an app's own
    /// endpoint, exempt from the address policy. The URL's host still goes
    /// in the `Host` header.
    pub connect_to: Option<SocketAddr>,
    pub tls: Arc<rustls::ClientConfig>,
}

/// What an HTTP check got back.
#[derive(Debug, Clone, Default)]
pub struct HttpAnswer {
    pub status: u16,
    pub location: Option<String>,
    /// The start of the body (at most [`MAX_BODY`]), de-chunked.
    pub body: Vec<u8>,
    pub latency: Duration,
    /// The server certificate's expiry, unix seconds (HTTPS).
    pub cert_expires: Option<u64>,
    /// The URL that answered, after redirects, without its query.
    pub final_url: String,
    /// Cloudflare Access itself refused the request (401 or 403 with its
    /// headers), rather than the app behind it answering.
    pub access_refused: bool,
}

/// A URL without its query and fragment, for messages.
pub fn display_url(url: &str) -> String {
    let u = url.split(['?', '#']).next().unwrap_or_default();
    u.to_string()
}

/// Request `p.url`, following redirects when asked.
pub fn http(p: &HttpProbe) -> Result<HttpAnswer, String> {
    let deadline = Instant::now() + p.timeout;
    let started = Instant::now();
    let mut url = p.url.clone();
    for hop in 0..=MAX_REDIRECTS {
        let t = net::parse_url(&url).map_err(|e| format!("URL: {e}"))?;
        // A redirect goes wherever it says, under the policy again.
        let connect_to = if hop == 0 { p.connect_to } else { None };
        let mut a = once(p, &t, connect_to, deadline)?;
        a.final_url = display_url(&url);
        let redirect = (300..400).contains(&a.status) && a.status != 304;
        match (&a.location, redirect && p.follow_redirects) {
            (Some(loc), true) if hop < MAX_REDIRECTS => url = join(&t, loc),
            _ => {
                a.latency = started.elapsed();
                return Ok(a);
            }
        }
    }
    Err(format!("more than {MAX_REDIRECTS} redirects"))
}

/// Resolve a redirect's `Location` against the URL that sent it.
pub fn join(base: &Target, loc: &str) -> String {
    let l = loc.trim();
    if l.starts_with("http://") || l.starts_with("https://") {
        return l.to_string();
    }
    let scheme = if base.https { "https" } else { "http" };
    let default = if base.https { 443 } else { 80 };
    let host = if base.host.contains(':') {
        format!("[{}]", base.host)
    } else {
        base.host.clone()
    };
    let authority = if base.port == default {
        host
    } else {
        format!("{host}:{}", base.port)
    };
    if let Some(rest) = l.strip_prefix("//") {
        return format!("{scheme}://{rest}");
    }
    if l.starts_with('/') {
        return format!("{scheme}://{authority}{l}");
    }
    let dir = base.path.split('?').next().unwrap_or("/");
    let dir = &dir[..dir.rfind('/').map(|i| i + 1).unwrap_or(1)];
    format!("{scheme}://{authority}{dir}{l}")
}

fn left(deadline: Instant) -> Result<Duration, String> {
    let d = deadline.saturating_duration_since(Instant::now());
    if d.is_zero() {
        Err("timed out".into())
    } else {
        Ok(d.max(Duration::from_millis(1)))
    }
}

/// Connect to `host:port`, under the policy, within the deadline.
pub fn connect(
    host: &str,
    port: u16,
    allow_private: bool,
    deadline: Instant,
) -> Result<TcpStream, String> {
    let addrs = net::resolve(host, port, allow_private).map_err(|e| e.message)?;
    let mut last = None;
    for a in addrs {
        match dial(a, deadline) {
            Ok(s) => {
                let peer = s.peer_addr().map_err(|e| format!("{host}: {e}"))?;
                net::check_ip(peer.ip(), allow_private)
                    .map_err(|why| format!("refusing {host}: {why}"))?;
                return Ok(s);
            }
            Err(e) => last = Some(e),
        }
    }
    Err(format!(
        "cannot connect to {host}:{port}: {}",
        last.unwrap_or_default()
    ))
}

/// Connect to one address within the deadline.
pub fn dial(a: SocketAddr, deadline: Instant) -> Result<TcpStream, String> {
    let s = TcpStream::connect_timeout(&a, left(deadline)?).map_err(|e| {
        if e.kind() == std::io::ErrorKind::TimedOut || e.kind() == std::io::ErrorKind::WouldBlock {
            "timed out connecting".to_string()
        } else {
            e.to_string()
        }
    })?;
    s.set_nodelay(true).ok();
    Ok(s)
}

/// A TCP check: connect, then hang up. The time it took.
pub fn tcp(
    host: &str,
    port: u16,
    timeout: Duration,
    allow_private: bool,
) -> Result<Duration, String> {
    let started = Instant::now();
    let s = connect(host, port, allow_private, started + timeout)?;
    drop(s);
    Ok(started.elapsed())
}

fn host_header(t: &Target) -> String {
    let h = if t.host.contains(':') {
        format!("[{}]", t.host)
    } else {
        t.host.clone()
    };
    let default = if t.https { 443 } else { 80 };
    if t.port == default {
        h
    } else {
        format!("{h}:{}", t.port)
    }
}

fn request_head(p: &HttpProbe, t: &Target) -> Result<String, String> {
    let mut head = format!(
        "{} {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: isb-monitor/{}\r\nAccept: */*\r\nConnection: close\r\n",
        p.method,
        t.path,
        host_header(t),
        env!("CARGO_PKG_VERSION"),
    );
    for (k, v) in &p.headers {
        if k.contains(['\r', '\n', ':']) || v.contains(['\r', '\n']) {
            return Err(format!("a bad header {k:?}"));
        }
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    Ok(head)
}

/// One request, no redirects.
fn once(
    p: &HttpProbe,
    t: &Target,
    connect_to: Option<SocketAddr>,
    deadline: Instant,
) -> Result<HttpAnswer, String> {
    let head = request_head(p, t)?;
    let tcp = match connect_to {
        Some(a) => dial(a, deadline).map_err(|e| format!("cannot connect to {a}: {e}"))?,
        None => connect(&t.host, t.port, p.allow_private, deadline)?,
    };
    let head_only = p.method == "HEAD";
    if !t.https {
        let sock = s_clone(&tcp)?;
        let mut s = tcp;
        let raw = exchange(&mut s, &sock, head.as_bytes(), deadline, &t.host)?;
        return parse(&raw, head_only);
    }
    let net = Net {
        allow_private: p.allow_private,
        tls: p.tls.clone(),
    };
    let mut s = net::tls(&net, &t.host, tcp).map_err(|e| e.message)?;
    let sock = s_clone(&s.sock)?;
    while s.conn.is_handshaking() {
        sock.set_read_timeout(Some(left(deadline)?)).ok();
        s.conn
            .complete_io(&mut s.sock)
            .map_err(|e| tls_error(&t.host, &e))?;
    }
    let cert_expires = s
        .conn
        .peer_certificates()
        .and_then(|c| c.first())
        .and_then(|c| cert_not_after(c.as_ref()));
    let raw = exchange(&mut s, &sock, head.as_bytes(), deadline, &t.host)?;
    let mut a = parse(&raw, head_only)?;
    a.cert_expires = cert_expires;
    Ok(a)
}

fn s_clone(s: &TcpStream) -> Result<TcpStream, String> {
    s.try_clone().map_err(|e| e.to_string())
}

fn tls_error(host: &str, e: &std::io::Error) -> String {
    let m = e.to_string();
    if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut {
        format!("{host}: timed out in the TLS handshake")
    } else {
        format!("TLS to {host}: {m}")
    }
}

/// Write the request and read the answer until it is complete, the peer
/// closes, the body cap is reached or the deadline passes.
fn exchange<S: Read + Write>(
    s: &mut S,
    sock: &TcpStream,
    head: &[u8],
    deadline: Instant,
    host: &str,
) -> Result<Vec<u8>, String> {
    let io = |e: std::io::Error| {
        if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut {
            format!("{host}: timed out waiting for an answer")
        } else {
            format!("{host}: {e}")
        }
    };
    sock.set_write_timeout(Some(left(deadline)?)).ok();
    s.write_all(head).map_err(io)?;
    s.flush().map_err(io)?;
    let mut out = Vec::new();
    let mut buf = [0u8; 16384];
    loop {
        sock.set_read_timeout(Some(
            left(deadline).map_err(|_| format!("{host}: timed out waiting for an answer"))?,
        ))
        .ok();
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                out.extend_from_slice(&buf[..n]);
                if out.len() >= MAX_BODY + 16384 || complete(&out) {
                    break;
                }
            }
            Err(_) if header_end(&out).is_some() => break,
            Err(e) => return Err(io(e)),
        }
    }
    if out.is_empty() {
        return Err(format!("{host}: closed the connection without answering"));
    }
    Ok(out)
}

fn header_end(b: &[u8]) -> Option<usize> {
    b.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

/// Headers and the whole body (by Content-Length, or a chunked body's
/// last chunk) are in.
fn complete(b: &[u8]) -> bool {
    let Some(end) = header_end(b) else {
        return false;
    };
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut r = httparse::Response::new(&mut headers);
    if r.parse(b).is_err() {
        return false;
    }
    let header = |name: &str| {
        r.headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case(name))
            .and_then(|h| std::str::from_utf8(h.value).ok())
            .map(|v| v.trim().to_ascii_lowercase())
    };
    if header("transfer-encoding").is_some_and(|v| v.contains("chunked")) {
        return b[end..].ends_with(b"0\r\n\r\n");
    }
    match header("content-length").and_then(|v| v.parse::<usize>().ok()) {
        Some(n) => b.len() >= end + n,
        None => false,
    }
}

/// Parse a raw answer: status, `Location`, body (de-chunked, capped).
pub fn parse(raw: &[u8], head_only: bool) -> Result<HttpAnswer, String> {
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut r = httparse::Response::new(&mut headers);
    let end = match r.parse(raw) {
        Ok(httparse::Status::Complete(n)) => n,
        Ok(httparse::Status::Partial) => return Err("an incomplete HTTP answer".into()),
        Err(e) => return Err(format!("not an HTTP answer: {e}")),
    };
    let header = |name: &str| {
        r.headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case(name))
            .and_then(|h| std::str::from_utf8(h.value).ok())
            .map(|v| v.trim().to_string())
    };
    let chunked =
        header("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));
    let raw_body = if head_only { &[][..] } else { &raw[end..] };
    let mut body = if chunked {
        dechunk(raw_body)
    } else {
        raw_body.to_vec()
    };
    body.truncate(MAX_BODY);
    let status = r.code.unwrap_or(0);
    // Access answers a request without a session two ways besides its
    // sign-in redirect: a 403 page carrying its cf-access-* headers, and,
    // for an app it fronts with OAuth, a 401 whose WWW-Authenticate names
    // its protected-resource metadata.
    let access_refused = matches!(status, 401 | 403)
        && (header("cf-access-domain").is_some()
            || header("cf-access-aud").is_some()
            || header("www-authenticate")
                .is_some_and(|v| v.contains("cloudflare-access-protected-resource")));
    Ok(HttpAnswer {
        status,
        location: header("location"),
        body,
        access_refused,
        ..Default::default()
    })
}

/// A chunked body's data, as far as it goes.
pub fn dechunk(mut b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(i) = b.windows(2).position(|w| w == b"\r\n") {
        let size = std::str::from_utf8(&b[..i])
            .ok()
            .and_then(|s| usize::from_str_radix(s.split(';').next()?.trim(), 16).ok());
        let Some(n) = size else { break };
        if n == 0 {
            break;
        }
        let start = i + 2;
        let stop = (start + n).min(b.len());
        out.extend_from_slice(&b[start..stop]);
        if start + n + 2 > b.len() {
            break;
        }
        b = &b[start + n + 2..];
    }
    out
}

/// The `notAfter` of a DER X.509 certificate, unix seconds. Walks the
/// structure by hand: Certificate ::= SEQUENCE { tbsCertificate SEQUENCE
/// { \[0\] version OPTIONAL, serial, signature, issuer, validity SEQUENCE {
/// notBefore, notAfter }, ... }, ... }.
pub fn cert_not_after(der: &[u8]) -> Option<u64> {
    let (_, cert, _) = tlv(der)?;
    let (_, tbs, _) = tlv(cert)?;
    let mut rest = tbs;
    let (tag, _, r) = tlv(rest)?;
    if tag == 0xa0 {
        rest = r; // the version
    }
    for _ in 0..3 {
        rest = tlv(rest)?.2; // serial, signature algorithm, issuer
    }
    let (tag, validity, _) = tlv(rest)?;
    if tag != 0x30 {
        return None;
    }
    let (_, _, r) = tlv(validity)?; // notBefore
    let (tag, t, _) = tlv(r)?;
    let s = std::str::from_utf8(t).ok()?;
    match tag {
        0x17 => asn1_time(s, true),
        0x18 => asn1_time(s, false),
        _ => None,
    }
}

/// One DER element: (tag, contents, what follows).
fn tlv(b: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let tag = *b.first()?;
    let first = *b.get(1)?;
    let (len, hdr) = if first < 0x80 {
        (first as usize, 2)
    } else {
        let n = (first & 0x7f) as usize;
        if n == 0 || n > 4 {
            return None;
        }
        let mut len = 0usize;
        for i in 0..n {
            len = (len << 8) | *b.get(2 + i)? as usize;
        }
        (len, 2 + n)
    };
    let end = hdr.checked_add(len)?;
    (end <= b.len()).then(|| (tag, &b[hdr..end], &b[end..]))
}

/// `YYMMDDHHMMSSZ` (UTCTime) or `YYYYMMDDHHMMSSZ` (GeneralizedTime).
fn asn1_time(s: &str, utc: bool) -> Option<u64> {
    let s = s.strip_suffix('Z')?;
    let (year, rest) = if utc {
        let y: i64 = s.get(..2)?.parse().ok()?;
        (if y >= 50 { 1900 + y } else { 2000 + y }, s.get(2..)?)
    } else {
        (s.get(..4)?.parse().ok()?, s.get(4..)?)
    };
    let n = |i: usize| -> Option<i64> { rest.get(i..i + 2)?.parse().ok() };
    let (mo, d, h, mi, se) = (n(0)?, n(2)?, n(4)?, n(6)?, n(8)?);
    let days = days_from_civil(year, mo, d);
    let t = days * 86400 + h * 3600 + mi * 60 + se;
    u64::try_from(t).ok()
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn probe(url: &str) -> HttpProbe {
        HttpProbe {
            url: url.into(),
            method: "GET".into(),
            headers: vec![("X-Token".into(), "t0k".into())],
            timeout: Duration::from_secs(3),
            follow_redirects: false,
            allow_private: true,
            connect_to: None,
            tls: net::default_tls(),
        }
    }

    /// Answer each connection with the next canned response; return the
    /// requests.
    fn server(answers: Vec<String>) -> (u16, std::thread::JoinHandle<Vec<String>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            let mut got = Vec::new();
            for a in answers {
                let (mut s, _) = l.accept().unwrap();
                let mut buf = vec![0u8; 8192];
                let n = s.read(&mut buf).unwrap();
                got.push(String::from_utf8_lossy(&buf[..n]).to_string());
                s.write_all(a.as_bytes()).unwrap();
            }
            got
        });
        (port, h)
    }

    #[test]
    fn access_refusals_are_told_from_the_app() {
        let refused = |raw: &str| parse(raw.as_bytes(), true).unwrap().access_refused;
        // Access' own 403 page, and its OAuth 401 for an app it fronts.
        assert!(refused(
            "HTTP/1.1 403 Forbidden\r\ncf-access-domain: broker.example.com\r\n\r\n"
        ));
        assert!(refused(
            "HTTP/1.1 401 Unauthorized\r\nwww-authenticate: Bearer realm=\"OAuth\", resource_metadata=\"https://a.example.com/.well-known/cloudflare-access-protected-resource/x\"\r\n\r\n"
        ));
        // The app's own refusals, and Access headers on anything but 401/403.
        assert!(!refused("HTTP/1.1 403 Forbidden\r\n\r\n"));
        assert!(!refused(
            "HTTP/1.1 401 Unauthorized\r\nwww-authenticate: Basic realm=\"x\"\r\n\r\n"
        ));
        assert!(!refused(
            "HTTP/1.1 200 OK\r\ncf-access-domain: broker.example.com\r\n\r\n"
        ));
    }

    #[test]
    fn get_with_headers_and_a_chunked_body() {
        let (port, h) = server(vec![
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n"
                .into(),
        ]);
        let a = http(&probe(&format!("http://127.0.0.1:{port}/health?k=1"))).unwrap();
        assert_eq!(a.status, 200);
        assert_eq!(a.body, b"hello world");
        assert_eq!(a.final_url, format!("http://127.0.0.1:{port}/health"));
        let req = &h.join().unwrap()[0];
        assert!(req.starts_with("GET /health?k=1 HTTP/1.1\r\n"), "{req}");
        assert!(req.contains("X-Token: t0k\r\n"), "{req}");
    }

    #[test]
    fn redirects_are_followed_only_when_asked() {
        let (port, h) = server(vec![
            "HTTP/1.1 302 Found\r\nLocation: /login\r\nContent-Length: 0\r\n\r\n".into(),
            "HTTP/1.1 302 Found\r\nLocation: /login\r\nContent-Length: 0\r\n\r\n".into(),
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".into(),
        ]);
        let url = format!("http://127.0.0.1:{port}/");
        let a = http(&probe(&url)).unwrap();
        assert_eq!((a.status, a.location.as_deref()), (302, Some("/login")));
        let mut p = probe(&url);
        p.follow_redirects = true;
        let a = http(&p).unwrap();
        assert_eq!((a.status, a.body.as_slice()), (200, &b"ok"[..]));
        assert!(a.final_url.ends_with("/login"));
        h.join().unwrap();
    }

    #[test]
    fn the_address_policy_holds_unless_dialled_by_reference() {
        let (port, h) = server(vec!["HTTP/1.1 204 No Content\r\n\r\n".into()]);
        let mut p = probe(&format!("http://127.0.0.1:{port}/"));
        p.allow_private = false;
        let e = http(&p).unwrap_err();
        assert!(e.contains("refusing 127.0.0.1"), "{e}");
        // By reference (an app's own endpoint), the Host stays the name.
        let mut p = probe("http://shop.example.com/");
        p.allow_private = false;
        p.connect_to = Some(format!("127.0.0.1:{port}").parse().unwrap());
        assert_eq!(http(&p).unwrap().status, 204);
        assert!(h.join().unwrap()[0].contains("Host: shop.example.com\r\n"));
        // TCP too.
        assert!(tcp("127.0.0.1", port, Duration::from_secs(1), false).is_err());
    }

    #[test]
    fn timeouts_and_refusals() {
        // Accepts, never answers.
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let mut p = probe(&format!("http://127.0.0.1:{port}/"));
        p.timeout = Duration::from_millis(300);
        let started = Instant::now();
        let e = http(&p).unwrap_err();
        assert!(e.contains("timed out"), "{e}");
        assert!(started.elapsed() < Duration::from_secs(2));
        drop(l);
        let e = http(&probe(&format!("http://127.0.0.1:{port}/"))).unwrap_err();
        assert!(e.contains("cannot connect"), "{e}");
        let e = tcp("127.0.0.1", port, Duration::from_secs(1), true).unwrap_err();
        assert!(e.contains("cannot connect"), "{e}");
    }

    #[test]
    fn redirect_targets() {
        let t = net::parse_url("https://a.example.com:8443/x/y?q").unwrap();
        assert_eq!(join(&t, "/z"), "https://a.example.com:8443/z");
        assert_eq!(join(&t, "z"), "https://a.example.com:8443/x/z");
        assert_eq!(join(&t, "http://b.example/"), "http://b.example/");
        assert_eq!(join(&t, "//c.example/p"), "https://c.example/p");
        assert_eq!(display_url("https://a/b?token=1#f"), "https://a/b");
    }

    #[test]
    fn certificate_expiry() {
        let mut params = rcgen::CertificateParams::new(vec!["shop.example.com".into()]).unwrap();
        params.not_after = rcgen::date_time_ymd(2031, 7, 9);
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = params.self_signed(&key).unwrap();
        let want = days_from_civil(2031, 7, 9) as u64 * 86400;
        assert_eq!(cert_not_after(cert.der().as_ref()), Some(want));
        // GeneralizedTime past 2049.
        params.not_after = rcgen::date_time_ymd(2051, 1, 2);
        let cert = params.self_signed(&key).unwrap();
        assert_eq!(
            cert_not_after(cert.der().as_ref()),
            Some(days_from_civil(2051, 1, 2) as u64 * 86400)
        );
        assert_eq!(cert_not_after(b"\x30\x03\x02\x01\x01"), None);
        assert_eq!(cert_not_after(&[]), None);
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11017);
    }
}
