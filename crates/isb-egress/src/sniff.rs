//! Reading the destination name off the first bytes of a connection: the
//! SNI of a TLS ClientHello, or the `Host` header of an HTTP request.
//!
//! The proxy never sees the destination address the guest meant (the guest
//! connects to the proxy), so the name the client says is the destination.

use std::io::Read;
use std::net::TcpStream;
use std::time::{Duration, Instant};

/// What the first bytes of a connection are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// A TLS ClientHello, with the server name when it carries one.
    Tls(Option<String>),
    /// An HTTP/1.x request head, with its `Host` (no port) when it has one.
    Http(Option<String>),
    /// Neither: a protocol this proxy cannot read a name from.
    Other,
}

/// Result of looking at the bytes so far.
#[derive(Debug, PartialEq, Eq)]
pub enum Peek {
    Need,
    Done(Kind),
}

/// Most bytes read while looking for a name.
pub const MAX_PEEK: usize = 64 * 1024;

fn be16(b: &[u8]) -> usize {
    usize::from(u16::from_be_bytes([b[0], b[1]]))
}

/// Classify `buf`, the start of a connection.
pub fn classify(buf: &[u8]) -> Peek {
    match buf.first() {
        None => Peek::Need,
        Some(0x16) => tls(buf),
        Some(c) if c.is_ascii_uppercase() => http(buf),
        Some(_) => Peek::Done(Kind::Other),
    }
}

/// The handshake bytes of the TLS records at the start of `buf` (a
/// ClientHello can span records), or `Need` if a record is incomplete.
fn handshake_bytes(buf: &[u8]) -> Result<Vec<u8>, ()> {
    let mut hs = Vec::new();
    let mut i = 0;
    loop {
        if buf.len() < i + 5 {
            return Err(());
        }
        let len = be16(&buf[i + 3..]);
        if buf[i] != 0x16 || buf[i + 1] != 3 || len > 16 * 1024 + 256 {
            return Ok(hs); // not TLS after all: the caller decides what is missing
        }
        if buf.len() < i + 5 + len {
            return Err(());
        }
        hs.extend_from_slice(&buf[i + 5..i + 5 + len]);
        i += 5 + len;
        if hs.len() >= 4 {
            let need =
                4 + (usize::from(hs[1]) << 16 | usize::from(hs[2]) << 8 | usize::from(hs[3]));
            if hs.len() >= need {
                return Ok(hs);
            }
        }
    }
}

fn tls(buf: &[u8]) -> Peek {
    let Ok(hs) = handshake_bytes(buf) else {
        return Peek::Need;
    };
    if hs.len() < 4 || hs[0] != 1 {
        return Peek::Done(Kind::Other);
    }
    Peek::Done(Kind::Tls(parse_sni(&hs[4..])))
}

/// A cursor over a byte slice that fails instead of panicking.
struct Cur<'a>(&'a [u8]);

impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.0.len() < n {
            return None;
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Some(a)
    }
    fn u8(&mut self) -> Option<usize> {
        self.take(1).map(|b| usize::from(b[0]))
    }
    fn u16(&mut self) -> Option<usize> {
        self.take(2).map(be16)
    }
}

/// The server name in a ClientHello body (after the 4-byte handshake header).
fn parse_sni(body: &[u8]) -> Option<String> {
    let mut c = Cur(body);
    c.take(2 + 32)?; // version, random
    let sid = c.u8()?;
    c.take(sid)?;
    let suites = c.u16()?;
    c.take(suites)?;
    let comp = c.u8()?;
    c.take(comp)?;
    let ext_len = c.u16()?;
    let mut exts = Cur(c.take(ext_len)?);
    while let (Some(ty), Some(len)) = (exts.u16(), exts.u16()) {
        let data = exts.take(len)?;
        if ty != 0 {
            continue;
        }
        let mut l = Cur(data);
        let list_len = l.u16()?;
        let mut list = Cur(l.take(list_len)?);
        while let (Some(name_type), Some(n)) = (list.u8(), list.u16()) {
            let name = list.take(n)?;
            if name_type == 0 {
                return std::str::from_utf8(name)
                    .ok()
                    .map(|s| s.trim_end_matches('.').to_ascii_lowercase())
                    .filter(|s| !s.is_empty());
            }
        }
    }
    None
}

fn http(buf: &[u8]) -> Peek {
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut req = httparse::Request::new(&mut headers);
    match req.parse(buf) {
        Ok(httparse::Status::Complete(_)) => {
            let host = req
                .headers
                .iter()
                .find(|h| h.name.eq_ignore_ascii_case("host"))
                .and_then(|h| std::str::from_utf8(h.value).ok())
                .map(host_only);
            Peek::Done(Kind::Http(host))
        }
        Ok(httparse::Status::Partial) => Peek::Need,
        Err(_) => Peek::Done(Kind::Other),
    }
}

/// `Host: api.example.com:8443` -> `api.example.com` (lower-cased).
pub fn host_only(v: &str) -> String {
    let v = v.trim();
    let h = if let Some(rest) = v.strip_prefix('[') {
        // `[::1]:80`
        return match rest.split_once(']') {
            Some((ip, _)) => format!("[{ip}]"),
            None => v.to_ascii_lowercase(),
        };
    } else {
        match v.rsplit_once(':') {
            Some((h, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => h,
            _ => v,
        }
    };
    h.trim_end_matches('.').to_ascii_lowercase()
}

/// Read from `s` until the connection is classified, or give up: `first`
/// bounds the wait for the first byte (a server-first protocol sends none),
/// `total` the wait for the rest of a name.
pub fn read_kind(
    s: &mut TcpStream,
    first: Duration,
    total: Duration,
) -> std::io::Result<(Vec<u8>, Kind)> {
    let start = Instant::now();
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 4096];
    loop {
        match classify(&buf) {
            Peek::Done(k) => return Ok((buf, k)),
            Peek::Need if buf.len() >= MAX_PEEK => return Ok((buf, Kind::Other)),
            Peek::Need => {}
        }
        let limit = if buf.is_empty() { first } else { total };
        let left = limit.saturating_sub(start.elapsed());
        if left.is_zero() {
            return Ok((buf, Kind::Other));
        }
        s.set_read_timeout(Some(left))?;
        match s.read(&mut chunk) {
            Ok(0) => return Ok((buf, Kind::Other)),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Ok((buf, Kind::Other));
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::Arc;

    /// A real ClientHello, as rustls writes one, for `host`.
    pub(crate) fn client_hello(host: &str) -> Vec<u8> {
        let roots = rustls::RootCertStore::empty();
        let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
        let name = rustls::pki_types::ServerName::try_from(host.to_string()).unwrap();
        let mut conn = rustls::ClientConnection::new(Arc::new(cfg), name).unwrap();
        let mut out = Vec::new();
        conn.write_tls(&mut out).unwrap();
        out
    }

    #[test]
    fn sni_is_read_from_a_real_client_hello() {
        let hello = client_hello("API.Example.com");
        assert_eq!(
            classify(&hello),
            Peek::Done(Kind::Tls(Some("api.example.com".into())))
        );
    }

    #[test]
    fn a_truncated_hello_needs_more_and_never_panics() {
        let hello = client_hello("api.example.com");
        for n in 0..hello.len() {
            match classify(&hello[..n]) {
                Peek::Need | Peek::Done(_) => {}
            }
        }
        assert_eq!(classify(&hello[..hello.len() - 1]), Peek::Need);
        assert_eq!(classify(&hello[..3]), Peek::Need);
    }

    #[test]
    fn an_ip_literal_hello_has_no_name() {
        let hello = client_hello("192.0.2.1");
        assert_eq!(classify(&hello), Peek::Done(Kind::Tls(None)));
    }

    #[test]
    fn a_hello_split_over_two_records_is_joined() {
        let hello = client_hello("split.example.com");
        let body = &hello[5..];
        let (a, b) = body.split_at(20);
        let mut two = Vec::new();
        for part in [a, b] {
            two.extend_from_slice(&[0x16, 3, 1]);
            two.extend_from_slice(&(part.len() as u16).to_be_bytes());
            two.extend_from_slice(part);
        }
        assert_eq!(
            classify(&two),
            Peek::Done(Kind::Tls(Some("split.example.com".into())))
        );
    }

    #[test]
    fn http_host_is_read_without_its_port() {
        let r = b"GET /x HTTP/1.1\r\nHost: Api.Example.com:8080\r\nAccept: */*\r\n\r\n";
        assert_eq!(
            classify(r),
            Peek::Done(Kind::Http(Some("api.example.com".into())))
        );
        assert_eq!(classify(b"GET /x HTTP/1.1\r\nHost: a"), Peek::Need);
        let none = b"GET / HTTP/1.0\r\n\r\n";
        assert_eq!(classify(none), Peek::Done(Kind::Http(None)));
    }

    #[test]
    fn other_protocols_are_other() {
        assert_eq!(
            classify(&[0, 0, 0, 8, 4, 210, 22, 47]),
            Peek::Done(Kind::Other)
        );
        assert_eq!(classify(b"SSH-2.0-x\r\n"), Peek::Done(Kind::Other));
        assert_eq!(
            classify(b"\x16\x03\x01\x00\x05\x02\x00\x00\x01\x00"),
            Peek::Done(Kind::Other)
        );
        assert_eq!(classify(b""), Peek::Need);
    }

    #[test]
    fn host_only_handles_ports_brackets_and_dots() {
        assert_eq!(host_only("Example.COM."), "example.com");
        assert_eq!(host_only("example.com:443"), "example.com");
        assert_eq!(host_only("[::1]:80"), "[::1]");
    }

    #[test]
    fn read_kind_times_out_on_a_silent_client() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let mut c = TcpStream::connect(addr).unwrap();
        let (mut s, _) = l.accept().unwrap();
        let (b, k) = read_kind(&mut s, Duration::from_millis(100), Duration::from_secs(1)).unwrap();
        assert!(b.is_empty());
        assert_eq!(k, Kind::Other);
        c.write_all(b"GET / HTTP/1.1\r\nHost: x.example.com\r\n\r\n")
            .unwrap();
        let (_, k) = read_kind(&mut s, Duration::from_secs(1), Duration::from_secs(1)).unwrap();
        assert_eq!(k, Kind::Http(Some("x.example.com".into())));
    }
}
