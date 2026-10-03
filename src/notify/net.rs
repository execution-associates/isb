//! Outbound connections for notifications, held to an address policy.
//!
//! A notification target is chosen by an org member, so it must not become a
//! way into the host's own network (SSRF). Every destination is resolved
//! here, every address it resolves to is checked, and the connection is made
//! to one of those checked addresses (never re-resolved, so DNS rebinding
//! cannot swap in another), and the connected peer is checked once more.
//! Redirects are never followed. Loopback, private, link-local, CGNAT and
//! other non-public ranges are refused unless the platform admin allows
//! private targets server-wide.
//!
//! Error messages name the host, never the URL: a webhook URL's path is a
//! credential (Slack, Discord), as is a Telegram bot token.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// How long connecting, and each read or write, may take.
pub const IO_TIMEOUT: Duration = Duration::from_secs(10);
/// The longest a whole HTTP exchange may take.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(30);
/// The most of a response that is read.
const MAX_RESPONSE: usize = 64 * 1024;

/// A failed send: what went wrong, and whether trying again may help.
#[derive(Debug, Clone, PartialEq)]
pub struct SendError {
    pub message: String,
    pub retryable: bool,
}

impl SendError {
    pub fn permanent(m: impl Into<String>) -> SendError {
        SendError {
            message: m.into(),
            retryable: false,
        }
    }
    pub fn transient(m: impl Into<String>) -> SendError {
        SendError {
            message: m.into(),
            retryable: true,
        }
    }
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Where and how outbound connections may go.
#[derive(Clone)]
pub struct Net {
    /// Allow loopback, private and other non-public destinations.
    pub allow_private: bool,
    pub tls: Arc<rustls::ClientConfig>,
}

impl Net {
    /// The public internet only, trusting the Mozilla root set.
    pub fn new(allow_private: bool) -> Net {
        Net {
            allow_private,
            tls: default_tls(),
        }
    }
}

/// A TLS client config on ring with the webpki (Mozilla) roots.
pub fn default_tls() -> Arc<rustls::ClientConfig> {
    static TLS: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    TLS.get_or_init(|| {
        let roots = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        tls_with_roots(roots)
    })
    .clone()
}

/// A TLS client config trusting exactly `roots`.
pub fn tls_with_roots(roots: rustls::RootCertStore) -> Arc<rustls::ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    Arc::new(
        rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("ring supports the default protocol versions")
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

/// A parsed `http(s)://host[:port]/path` URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub https: bool,
    /// Lowercased; an IPv6 literal without its brackets.
    pub host: String,
    pub port: u16,
    /// Path and query, starting with `/`.
    pub path: String,
}

impl Target {
    /// The Host header's value.
    fn host_header(&self) -> String {
        let h = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        let default = if self.https { 443 } else { 80 };
        if self.port == default {
            h
        } else {
            format!("{h}:{}", self.port)
        }
    }
}

/// Parse a webhook URL. The error never repeats the URL.
pub fn parse_url(s: &str) -> Result<Target, String> {
    let s = s.trim();
    let (https, rest) = if let Some(r) = strip_prefix_ci(s, "https://") {
        (true, r)
    } else if let Some(r) = strip_prefix_ci(s, "http://") {
        (false, r)
    } else {
        return Err("the URL must start with http:// or https://".into());
    };
    if s.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("the URL holds whitespace or control characters".into());
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    if authority.contains('@') {
        return Err("credentials in the URL are not supported".into());
    }
    let (host, port) = if let Some(r) = authority.strip_prefix('[') {
        let close = r.find(']').ok_or("an unclosed [ in the URL's host")?;
        let host = &r[..close];
        host.parse::<Ipv6Addr>()
            .map_err(|_| "a bad IPv6 address in the URL")?;
        let after = &r[close + 1..];
        let port = match after.strip_prefix(':') {
            Some(p) => Some(p),
            None if after.is_empty() => None,
            None => return Err("junk after the URL's IPv6 host".into()),
        };
        (host.to_ascii_lowercase(), port)
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h.to_ascii_lowercase(), Some(p)),
            None => (authority.to_ascii_lowercase(), None),
        }
    };
    if host.is_empty() {
        return Err("the URL has no host".into());
    }
    if !host.contains(':')
        && !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
    {
        return Err(format!("{host:?} is not a host name"));
    }
    let port = match port {
        Some(p) => p
            .parse::<u16>()
            .ok()
            .filter(|p| *p != 0)
            .ok_or_else(|| format!("a bad port in the URL for {host}"))?,
        None if https => 443,
        None => 80,
    };
    let tail = tail.split('#').next().unwrap_or_default();
    let path = if tail.starts_with('/') {
        tail.to_string()
    } else {
        format!("/{tail}")
    };
    Ok(Target {
        https,
        host,
        port,
        path,
    })
}

fn strip_prefix_ci<'a>(s: &'a str, p: &str) -> Option<&'a str> {
    (s.len() >= p.len() && s[..p.len()].eq_ignore_ascii_case(p)).then(|| &s[p.len()..])
}

/// Read a host the way `inet_aton` (and so `getaddrinfo`) does: one to four
/// parts, each decimal, octal (leading 0) or hex (0x). `2130706433`,
/// `0x7f000001`, `0177.1` and `127.1` are all 127.0.0.1.
pub fn parse_inet_aton(s: &str) -> Option<Ipv4Addr> {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    let mut nums = Vec::with_capacity(4);
    for p in &parts {
        let (digits, radix) = if let Some(h) = p.strip_prefix("0x").or(p.strip_prefix("0X")) {
            (h, 16)
        } else if p.len() > 1 && p.starts_with('0') {
            (&p[1..], 8)
        } else {
            (*p, 10)
        };
        if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
            return None;
        }
        let n = u64::from_str_radix(digits, radix).ok()?;
        nums.push(n);
    }
    let last = *nums.last()?;
    let lead = &nums[..nums.len() - 1];
    if lead.iter().any(|n| *n > 255) {
        return None;
    }
    let rest_bits = 8 * (4 - lead.len() as u32);
    if rest_bits < 64 && last >= (1u64 << rest_bits) {
        return None;
    }
    let mut v: u64 = 0;
    for (i, n) in lead.iter().enumerate() {
        v |= n << (24 - 8 * i as u32);
    }
    Some(Ipv4Addr::from((v | last) as u32))
}

/// Why an address may not be reached: `Err(reason)`. Some ranges are never
/// reachable (unspecified, multicast, broadcast); the rest of the non-public
/// ranges only with `allow_private`.
pub fn check_ip(ip: IpAddr, allow_private: bool) -> Result<(), String> {
    match classify(ip) {
        Class::Public => Ok(()),
        Class::Never(why) => Err(format!("{ip} is {why}")),
        Class::Private(_) if allow_private => Ok(()),
        Class::Private(why) => Err(format!(
            "{ip} is {why}; private targets are off (a platform admin can allow them)"
        )),
    }
}

enum Class {
    Public,
    Private(&'static str),
    Never(&'static str),
}

fn classify(ip: IpAddr) -> Class {
    match ip {
        IpAddr::V4(v4) => classify_v4(v4),
        IpAddr::V6(v6) => classify_v6(v6),
    }
}

fn in_v4(ip: Ipv4Addr, net: [u8; 4], bits: u32) -> bool {
    let mask = if bits == 0 {
        0
    } else {
        u32::MAX << (32 - bits)
    };
    (u32::from(ip) & mask) == (u32::from(Ipv4Addr::from(net)) & mask)
}

fn classify_v4(ip: Ipv4Addr) -> Class {
    use Class::*;
    let table: &[([u8; 4], u32, Class)] = &[
        ([0, 0, 0, 0], 8, Never("in 0.0.0.0/8 (this network)")),
        ([255, 255, 255, 255], 32, Never("the broadcast address")),
        ([224, 0, 0, 0], 4, Never("multicast")),
        ([127, 0, 0, 0], 8, Private("loopback")),
        ([10, 0, 0, 0], 8, Private("private (10.0.0.0/8)")),
        ([172, 16, 0, 0], 12, Private("private (172.16.0.0/12)")),
        ([192, 168, 0, 0], 16, Private("private (192.168.0.0/16)")),
        (
            [100, 64, 0, 0],
            10,
            Private("shared address space (100.64.0.0/10)"),
        ),
        ([169, 254, 0, 0], 16, Private("link-local")),
        ([192, 0, 0, 0], 24, Private("IETF protocol space")),
        ([192, 0, 2, 0], 24, Private("documentation space")),
        ([198, 51, 100, 0], 24, Private("documentation space")),
        ([203, 0, 113, 0], 24, Private("documentation space")),
        ([198, 18, 0, 0], 15, Private("benchmarking space")),
        ([240, 0, 0, 0], 4, Private("reserved (240.0.0.0/4)")),
    ];
    for (net, bits, class) in table {
        if in_v4(ip, *net, *bits) {
            return match class {
                Never(w) => Never(w),
                Private(w) => Private(w),
                Public => Public,
            };
        }
    }
    Public
}

fn classify_v6(ip: Ipv6Addr) -> Class {
    use Class::*;
    let s = ip.segments();
    let embedded = |hi: usize| {
        Ipv4Addr::new(
            (s[hi] >> 8) as u8,
            s[hi] as u8,
            (s[hi + 1] >> 8) as u8,
            s[hi + 1] as u8,
        )
    };
    if ip.is_unspecified() {
        return Never("unspecified");
    }
    if ip.is_loopback() {
        return Private("loopback");
    }
    if s[0] & 0xff00 == 0xff00 {
        return Never("multicast");
    }
    // ::ffff:a.b.c.d (mapped) and ::a.b.c.d (compatible) reach the IPv4 host.
    if s[..5] == [0; 5] && (s[5] == 0xffff || s[5] == 0) {
        return classify_v4(embedded(6));
    }
    // NAT64: the well-known prefix reaches the embedded IPv4 address.
    if s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        return classify_v4(embedded(6));
    }
    if s[0] == 0x64 && s[1] == 0xff9b && s[2] == 1 {
        return Private("local-use NAT64");
    }
    // 6to4 carries an IPv4 address in bits 16..48.
    if s[0] == 0x2002 {
        return classify_v4(embedded(1));
    }
    if s[0] == 0x2001 && s[1] == 0 {
        return Private("Teredo");
    }
    if s[0] == 0x2001 && s[1] == 0x0db8 {
        return Private("documentation space");
    }
    if s[0] & 0xfe00 == 0xfc00 {
        return Private("unique local (fc00::/7)");
    }
    if s[0] & 0xffc0 == 0xfe80 {
        return Private("link-local");
    }
    if s[0] & 0xffc0 == 0xfec0 {
        return Private("site-local");
    }
    if s[..4] == [0x100, 0, 0, 0] {
        return Private("discard space");
    }
    Public
}

/// Resolve a host and check every address: all must pass, so a name with
/// one public and one private record is refused.
pub fn resolve(host: &str, port: u16, allow_private: bool) -> Result<Vec<SocketAddr>, SendError> {
    let literal = host
        .parse::<IpAddr>()
        .ok()
        .or_else(|| parse_inet_aton(host).map(IpAddr::V4));
    let addrs: Vec<SocketAddr> = match literal {
        Some(ip) => vec![SocketAddr::new(ip, port)],
        None => (host, port)
            .to_socket_addrs()
            .map_err(|e| SendError::transient(format!("cannot resolve {host}: {e}")))?
            .collect(),
    };
    if addrs.is_empty() {
        return Err(SendError::transient(format!("{host} has no addresses")));
    }
    for a in &addrs {
        check_ip(a.ip(), allow_private)
            .map_err(|why| SendError::permanent(format!("refusing {host}: {why}")))?;
    }
    Ok(addrs)
}

/// Connect to one of a host's checked addresses, and check the peer.
pub fn connect(host: &str, port: u16, allow_private: bool) -> Result<TcpStream, SendError> {
    let addrs = resolve(host, port, allow_private)?;
    let mut last = None;
    for a in addrs {
        match TcpStream::connect_timeout(&a, IO_TIMEOUT) {
            Ok(s) => {
                let peer = s
                    .peer_addr()
                    .map_err(|e| SendError::transient(format!("{host}: {e}")))?;
                check_ip(peer.ip(), allow_private)
                    .map_err(|why| SendError::permanent(format!("refusing {host}: {why}")))?;
                s.set_read_timeout(Some(IO_TIMEOUT)).ok();
                s.set_write_timeout(Some(IO_TIMEOUT)).ok();
                return Ok(s);
            }
            Err(e) => last = Some(e),
        }
    }
    Err(SendError::transient(format!(
        "cannot connect to {host}:{port}: {}",
        last.map(|e| e.to_string()).unwrap_or_default()
    )))
}

/// Wrap a connected stream in TLS to `host`.
pub fn tls(
    net: &Net,
    host: &str,
    tcp: TcpStream,
) -> Result<rustls::StreamOwned<rustls::ClientConnection, TcpStream>, SendError> {
    let name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|_| SendError::permanent(format!("{host} is not a TLS server name")))?;
    let conn = rustls::ClientConnection::new(net.tls.clone(), name)
        .map_err(|e| SendError::permanent(format!("TLS to {host}: {e}")))?;
    Ok(rustls::StreamOwned::new(conn, tcp))
}

/// An HTTP POST.
#[derive(Debug, Clone)]
pub struct Request {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// What came back.
#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub retry_after: Option<Duration>,
    /// The start of the body, lossily decoded.
    pub body: String,
}

/// POST once. Never follows a redirect: a 3xx is a failure like a 4xx.
pub fn post(net: &Net, req: &Request) -> Result<Response, SendError> {
    let t = parse_url(&req.url).map_err(SendError::permanent)?;
    let started = Instant::now();
    let tcp = connect(&t.host, t.port, net.allow_private)?;
    let mut head = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: isb/{}\r\nContent-Length: {}\r\nConnection: close\r\n",
        t.path,
        t.host_header(),
        env!("CARGO_PKG_VERSION"),
        req.body.len()
    );
    for (k, v) in &req.headers {
        if k.contains(['\r', '\n', ':']) || v.contains(['\r', '\n']) {
            return Err(SendError::permanent(format!("a bad header {k:?}")));
        }
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    let raw = if t.https {
        let mut s = tls(net, &t.host, tcp)?;
        exchange(&mut s, &t.host, head.as_bytes(), &req.body, started)?
    } else {
        let mut s = tcp;
        exchange(&mut s, &t.host, head.as_bytes(), &req.body, started)?
    };
    parse_response(&raw).map_err(|e| SendError::transient(format!("{}: {e}", t.host)))
}

fn exchange<S: Read + Write>(
    s: &mut S,
    host: &str,
    head: &[u8],
    body: &[u8],
    started: Instant,
) -> Result<Vec<u8>, SendError> {
    let io = |e: std::io::Error| SendError::transient(format!("{host}: {e}"));
    s.write_all(head).map_err(io)?;
    s.write_all(body).map_err(io)?;
    s.flush().map_err(io)?;
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        if started.elapsed() > EXCHANGE_TIMEOUT {
            return Err(SendError::transient(format!(
                "{host}: no complete answer within {EXCHANGE_TIMEOUT:?}"
            )));
        }
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                out.extend_from_slice(&buf[..n]);
                if out.len() >= MAX_RESPONSE || response_complete(&out) {
                    break;
                }
            }
            // A peer that closes without TLS close_notify, after answering.
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof && !out.is_empty() => break,
            Err(e) if !out.is_empty() && header_end(&out).is_some() => {
                let _ = e;
                break;
            }
            Err(e) => return Err(io(e)),
        }
    }
    Ok(out)
}

fn header_end(b: &[u8]) -> Option<usize> {
    b.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

/// Headers and a Content-Length body are in (a server that keeps the
/// connection open despite `Connection: close` does not hold us up).
fn response_complete(b: &[u8]) -> bool {
    let Some(end) = header_end(b) else {
        return false;
    };
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut r = httparse::Response::new(&mut headers);
    if r.parse(b).is_err() {
        return false;
    }
    let len = r
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
    matches!(len, Some(n) if b.len() >= end + n)
}

fn parse_response(raw: &[u8]) -> Result<Response, String> {
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut r = httparse::Response::new(&mut headers);
    let end = match r.parse(raw) {
        Ok(httparse::Status::Complete(n)) => n,
        Ok(httparse::Status::Partial) => return Err("an incomplete HTTP answer".into()),
        Err(e) => return Err(format!("a bad HTTP answer: {e}")),
    };
    let status = r.code.unwrap_or(0);
    let retry_after = r
        .headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case("retry-after"))
        .and_then(|h| {
            std::str::from_utf8(h.value)
                .ok()?
                .trim()
                .parse::<u64>()
                .ok()
        })
        .map(Duration::from_secs);
    let body = &raw[end..];
    let body = String::from_utf8_lossy(&body[..body.len().min(512)]).into_owned();
    Ok(Response {
        status,
        retry_after,
        body,
    })
}

/// HMAC-SHA256 of `body` under `key`, as lowercase hex.
pub fn hmac_sha256_hex(key: &[u8], body: &[u8]) -> String {
    let k = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key);
    ring::hmac::sign(&k, body)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(s: &str) -> bool {
        let ip: IpAddr = s.parse().unwrap();
        check_ip(ip, false).is_err()
    }

    #[test]
    fn urls() {
        let t = parse_url("https://hooks.slack.com/services/T/B/x?y=1#frag").unwrap();
        assert_eq!(
            t,
            Target {
                https: true,
                host: "hooks.slack.com".into(),
                port: 443,
                path: "/services/T/B/x?y=1".into()
            }
        );
        let t = parse_url("HTTP://[::1]:8080").unwrap();
        assert_eq!(
            (t.https, t.host.as_str(), t.port, t.path.as_str()),
            (false, "::1", 8080, "/")
        );
        assert_eq!(t.host_header(), "[::1]:8080");
        assert_eq!(
            parse_url("http://Example.COM:80/a").unwrap().host_header(),
            "example.com"
        );
        for bad in [
            "ftp://x/",
            "file:///etc/passwd",
            "gopher://x",
            "http://user:pw@x/",
            "http:///x",
            "http://x:0/",
            "http://x:99999/",
            "http://[::1/",
            "http://a b/",
            "http://x/\r\nHost: y",
        ] {
            assert!(parse_url(bad).is_err(), "{bad}");
        }
        // Errors never echo the URL (its path can be a credential).
        let e = parse_url("ftp://x/SECRETTOKEN").unwrap_err();
        assert!(!e.contains("SECRETTOKEN"));
    }

    #[test]
    fn non_public_ranges_are_refused() {
        for s in [
            "127.0.0.1",
            "127.255.0.9",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "100.86.22.100",
            "0.0.0.0",
            "0.1.2.3",
            "255.255.255.255",
            "224.0.0.1",
            "240.0.0.1",
            "198.18.0.1",
            "::",
            "::1",
            "fe80::1",
            "fc00::1",
            "fd42:1:2::3",
            "ff02::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::127.0.0.1",
            "64:ff9b::a9fe:a9fe",
            "2002:7f00:1::",
            "2002:c0a8:0101::1",
            "2001:db8::1",
            "fec0::1",
        ] {
            assert!(refused(s), "{s} should be refused");
        }
        for s in [
            "1.1.1.1",
            "8.8.8.8",
            "172.32.0.1",
            "100.128.0.1",
            "2606:4700:4700::1111",
            "::ffff:1.1.1.1",
            "64:ff9b::808:808",
            "2002:0808:0808::1",
        ] {
            assert!(!refused(s), "{s} should pass");
        }
        // The admin switch opens private ranges, never the unusable ones.
        assert!(check_ip("127.0.0.1".parse().unwrap(), true).is_ok());
        assert!(check_ip("::1".parse().unwrap(), true).is_ok());
        assert!(check_ip("0.0.0.0".parse().unwrap(), true).is_err());
        assert!(check_ip("::".parse().unwrap(), true).is_err());
        assert!(check_ip("ff02::1".parse().unwrap(), true).is_err());
    }

    #[test]
    fn numeric_host_forms() {
        let lo = Ipv4Addr::new(127, 0, 0, 1);
        for s in [
            "2130706433",
            "0x7f000001",
            "0X7F000001",
            "017700000001",
            "0177.0.0.1",
            "0x7f.0.0.1",
            "127.1",
            "127.0.1",
            "0x7f.1",
        ] {
            assert_eq!(parse_inet_aton(s), Some(lo), "{s}");
        }
        assert_eq!(
            parse_inet_aton("169.254.43518"),
            Some(Ipv4Addr::new(169, 254, 169, 254))
        );
        for s in [
            "example.com",
            "256.1.1.1",
            "1.2.3.4.5",
            "",
            "0x",
            "09",
            "1.2.3.256",
        ] {
            assert_eq!(parse_inet_aton(s), None, "{s}");
        }
        // Resolution treats them as the address they name, and refuses it.
        for h in [
            "2130706433",
            "0x7f000001",
            "0177.0.0.1",
            "127.1",
            "::ffff:127.0.0.1",
        ] {
            let e = resolve(h, 80, false).unwrap_err();
            assert!(!e.retryable && e.message.contains("loopback"), "{h}: {e}");
        }
        assert!(resolve("2130706433", 80, true).is_ok());
    }

    #[test]
    fn names_resolving_to_private_addresses_are_refused() {
        // `localhost` resolves (without DNS) to loopback: a name is checked
        // by what it resolves to, not by how it looks.
        let e = resolve("localhost", 80, false).unwrap_err();
        assert!(e.message.contains("loopback"), "{e}");
        assert!(!e.retryable);
        // And connect refuses before any packet leaves.
        let e = connect("localhost", 9, false).unwrap_err();
        assert!(e.message.contains("refusing localhost"), "{e}");
    }

    #[test]
    fn posts_without_following_redirects() {
        use std::net::TcpListener;
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut buf = vec![0u8; 4096];
            let mut got = Vec::new();
            while !String::from_utf8_lossy(&got).contains("hello") {
                let n = s.read(&mut buf).unwrap();
                got.extend_from_slice(&buf[..n]);
            }
            s.write_all(
                b"HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/\r\nRetry-After: 7\r\nContent-Length: 2\r\n\r\nno",
            )
            .unwrap();
            String::from_utf8(got).unwrap()
        });
        let net = Net::new(true);
        let r = post(
            &net,
            &Request {
                url: format!("http://127.0.0.1:{port}/hook?a=1"),
                headers: vec![("X-Test".into(), "1".into())],
                body: b"hello".to_vec(),
            },
        )
        .unwrap();
        assert_eq!(r.status, 302);
        assert_eq!(r.retry_after, Some(Duration::from_secs(7)));
        assert_eq!(r.body, "no");
        let req = h.join().unwrap();
        assert!(req.starts_with("POST /hook?a=1 HTTP/1.1\r\n"), "{req}");
        assert!(req.contains(&format!("Host: 127.0.0.1:{port}\r\n")));
        assert!(req.contains("X-Test: 1\r\n"));
        // Without the admin switch the same target is refused up front.
        let e = post(
            &Net::new(false),
            &Request {
                url: format!("http://127.0.0.1:{port}/hook"),
                headers: vec![],
                body: vec![],
            },
        )
        .unwrap_err();
        assert!(e.message.contains("private targets are off"), "{e}");
    }

    #[test]
    fn hmac_matches_rfc_4231() {
        // RFC 4231 test case 2.
        assert_eq!(
            hmac_sha256_hex(b"Jefe", b"what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }
}
