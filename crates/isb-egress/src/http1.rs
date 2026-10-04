//! HTTP/1.1 as far as the proxy needs it: heads, body framing and a
//! buffered connection, so requests can be rewritten and responses
//! scrubbed without a full HTTP stack.

use std::io::{self, Read, Write};

/// Longest request or response head.
pub const MAX_HEAD: usize = 64 * 1024;
const MAX_LINE: usize = 8 * 1024;

/// A parsed head: the first line (without its line break) and the headers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    pub first: Vec<u8>,
    pub headers: Vec<(String, Vec<u8>)>,
}

impl Head {
    /// The first header named `name` (case-insensitive).
    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_slice())
    }

    pub fn get_str(&self, name: &str) -> Option<&str> {
        self.get(name).and_then(|v| std::str::from_utf8(v).ok())
    }

    pub fn remove(&mut self, name: &str) {
        self.headers.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
    }

    pub fn set(&mut self, name: &str, value: &[u8]) {
        self.remove(name);
        self.headers.push((name.to_string(), value.to_vec()));
    }

    /// Whether a comma-separated header lists `token`.
    pub fn has_token(&self, name: &str, token: &str) -> bool {
        self.headers
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case(name))
            .filter_map(|(_, v)| std::str::from_utf8(v).ok())
            .any(|v| v.split(',').any(|t| t.trim().eq_ignore_ascii_case(token)))
    }

    /// The head as bytes, ready to send.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = self.first.clone();
        out.extend_from_slice(b"\r\n");
        for (n, v) in &self.headers {
            out.extend_from_slice(n.as_bytes());
            out.extend_from_slice(b": ");
            out.extend_from_slice(v);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"\r\n");
        out
    }
}

/// A request head, split.
#[derive(Debug, Clone)]
pub struct Request {
    pub head: Head,
    pub method: String,
    pub target: Vec<u8>,
    /// The minor version: 0 or 1.
    pub version: u8,
}

impl Request {
    /// The request line, from the parts (after the target changed).
    pub fn rebuild_first(&mut self) {
        let mut f = format!("{} ", self.method).into_bytes();
        f.extend_from_slice(&self.target);
        f.extend_from_slice(format!(" HTTP/1.{}", self.version).as_bytes());
        self.head.first = f;
    }
}

/// A response head, split.
#[derive(Debug, Clone)]
pub struct Response {
    pub head: Head,
    pub status: u16,
}

fn headers_of(h: &[httparse::Header<'_>]) -> Vec<(String, Vec<u8>)> {
    h.iter()
        .map(|h| (h.name.to_string(), h.value.to_vec()))
        .collect()
}

/// Parse a request head (the bytes up to and including the blank line).
pub fn parse_request(raw: &[u8]) -> Result<Request, String> {
    let mut hs = [httparse::EMPTY_HEADER; 100];
    let mut r = httparse::Request::new(&mut hs);
    match r.parse(raw) {
        Ok(httparse::Status::Complete(_)) => {}
        Ok(httparse::Status::Partial) => return Err("incomplete request head".into()),
        Err(e) => return Err(format!("bad request head: {e}")),
    }
    let (method, target) = (r.method.unwrap_or(""), r.path.unwrap_or(""));
    let version = r.version.unwrap_or(1);
    let first = format!("{method} {target} HTTP/1.{version}").into_bytes();
    Ok(Request {
        method: method.to_string(),
        target: target.as_bytes().to_vec(),
        version,
        head: Head {
            first,
            headers: headers_of(r.headers),
        },
    })
}

/// Parse a response head.
pub fn parse_response(raw: &[u8]) -> Result<Response, String> {
    let mut hs = [httparse::EMPTY_HEADER; 100];
    let mut r = httparse::Response::new(&mut hs);
    match r.parse(raw) {
        Ok(httparse::Status::Complete(_)) => {}
        Ok(httparse::Status::Partial) => return Err("incomplete response head".into()),
        Err(e) => return Err(format!("bad response head: {e}")),
    }
    let status = r.code.unwrap_or(502);
    let version = r.version.unwrap_or(1);
    let first = format!("HTTP/1.{version} {status} {}", r.reason.unwrap_or("")).into_bytes();
    Ok(Response {
        status,
        head: Head {
            first,
            headers: headers_of(r.headers),
        },
    })
}

/// How a message's body is framed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Body {
    None,
    Length(u64),
    Chunked,
    /// A response body that ends when the server closes.
    UntilClose,
}

/// The framing of a message, refusing the ambiguous ones that smuggle
/// requests (both `Content-Length` and chunked, or two lengths that differ).
pub fn framing(head: &Head, response: bool) -> Result<Body, String> {
    let chunked = head.has_token("transfer-encoding", "chunked");
    let lengths: Vec<&str> = head
        .headers
        .iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case("content-length"))
        .filter_map(|(_, v)| std::str::from_utf8(v).ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .collect();
    if chunked && !lengths.is_empty() {
        return Err("both Transfer-Encoding and Content-Length".into());
    }
    if chunked {
        return Ok(Body::Chunked);
    }
    if !lengths.is_empty() {
        let mut n = None;
        for l in lengths {
            let v: u64 = l.parse().map_err(|_| format!("bad Content-Length {l:?}"))?;
            if n.is_some_and(|p| p != v) {
                return Err("conflicting Content-Length headers".into());
            }
            n = Some(v);
        }
        return Ok(Body::Length(n.unwrap_or(0)));
    }
    Ok(if response { Body::UntilClose } else { Body::None })
}

/// A connection with a read-ahead buffer.
pub struct Conn<S> {
    pub s: S,
    buf: Vec<u8>,
}

impl<S> Conn<S> {
    pub fn new(s: S) -> Conn<S> {
        Conn { s, buf: Vec::new() }
    }

    /// Bytes read but not yet used.
    pub fn take_buffered(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.buf)
    }
}

fn find_end(b: &[u8]) -> Option<usize> {
    b.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

impl<S: Read> Conn<S> {
    fn fill(&mut self) -> io::Result<usize> {
        let mut chunk = [0u8; 16 * 1024];
        let n = self.s.read(&mut chunk)?;
        self.buf.extend_from_slice(&chunk[..n]);
        Ok(n)
    }

    /// The next head, with its blank line; `None` at a clean end of stream.
    pub fn read_head(&mut self) -> io::Result<Option<Vec<u8>>> {
        let mut scanned: usize = 0;
        loop {
            if let Some(end) = find_end(&self.buf[scanned.saturating_sub(3)..]) {
                let end = end + scanned.saturating_sub(3);
                return Ok(Some(self.buf.drain(..end).collect()));
            }
            scanned = self.buf.len();
            if self.buf.len() > MAX_HEAD {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "head too large"));
            }
            if self.fill()? == 0 {
                return if self.buf.is_empty() {
                    Ok(None)
                } else {
                    Err(io::ErrorKind::UnexpectedEof.into())
                };
            }
        }
    }

    /// One line, with its `\r\n` (at most 8 KiB).
    pub fn read_line(&mut self) -> io::Result<Vec<u8>> {
        loop {
            if let Some(i) = self.buf.iter().position(|b| *b == b'\n') {
                return Ok(self.buf.drain(..=i).collect());
            }
            if self.buf.len() > MAX_LINE {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
            }
            if self.fill()? == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
        }
    }

    /// Up to `max` bytes (at least one unless the stream ended).
    pub fn read_some(&mut self, max: usize) -> io::Result<Vec<u8>> {
        if self.buf.is_empty() && self.fill()? == 0 {
            return Ok(Vec::new());
        }
        let n = self.buf.len().min(max);
        Ok(self.buf.drain(..n).collect())
    }

    /// Exactly `n` bytes into `w`.
    pub fn copy_exact<W: Write>(&mut self, mut n: u64, w: &mut W) -> io::Result<()> {
        while n > 0 {
            let b = self.read_some(usize::try_from(n.min(64 * 1024)).unwrap_or(64 * 1024))?;
            if b.is_empty() {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            w.write_all(&b)?;
            n -= b.len() as u64;
        }
        Ok(())
    }

    /// Exactly `n` bytes, collected.
    pub fn read_exact_vec(&mut self, n: usize) -> io::Result<Vec<u8>> {
        let mut v = Vec::with_capacity(n);
        self.copy_exact(n as u64, &mut v)?;
        Ok(v)
    }
}

/// The size on a chunk-size line (`1a;ext=1\r\n`).
pub fn chunk_size(line: &[u8]) -> io::Result<u64> {
    let s = std::str::from_utf8(line)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad chunk size"))?;
    let hex = s.split(';').next().unwrap_or("").trim();
    u64::from_str_radix(hex, 16)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad chunk size"))
}

impl<S: Read> Conn<S> {
    /// A chunked body, copied through unchanged (framing included).
    pub fn copy_chunked<W: Write>(&mut self, w: &mut W) -> io::Result<()> {
        loop {
            let line = self.read_line()?;
            let size = chunk_size(&line)?;
            w.write_all(&line)?;
            if size == 0 {
                loop {
                    let t = self.read_line()?;
                    w.write_all(&t)?;
                    if t == b"\r\n" || t == b"\n" {
                        return Ok(());
                    }
                }
            }
            self.copy_exact(size + 2, w)?;
        }
    }
}

/// One chunk, as it goes on the wire.
pub fn chunk(data: &[u8]) -> Vec<u8> {
    let mut v = format!("{:x}\r\n", data.len()).into_bytes();
    v.extend_from_slice(data);
    v.extend_from_slice(b"\r\n");
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn req(text: &str) -> Request {
        parse_request(text.as_bytes()).unwrap()
    }

    #[test]
    fn heads_roundtrip() {
        let r = req("POST /v1/x?y=1 HTTP/1.1\r\nHost: a.example.com\r\nX-A: b\r\n\r\n");
        assert_eq!(r.method, "POST");
        assert_eq!(r.target, b"/v1/x?y=1");
        assert_eq!(r.head.get_str("host"), Some("a.example.com"));
        assert_eq!(
            r.head.to_bytes(),
            b"POST /v1/x?y=1 HTTP/1.1\r\nHost: a.example.com\r\nX-A: b\r\n\r\n"
        );
        let s = parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n").unwrap();
        assert_eq!(s.status, 200);
        assert_eq!(framing(&s.head, true).unwrap(), Body::Length(2));
    }

    #[test]
    fn framing_rules() {
        let h = |t: &str| parse_request(t.as_bytes()).unwrap().head;
        assert_eq!(framing(&h("GET / HTTP/1.1\r\n\r\n"), false).unwrap(), Body::None);
        assert_eq!(
            framing(&h("POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n"), false).unwrap(),
            Body::Chunked
        );
        assert!(framing(
            &h("POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\nContent-Length: 3\r\n\r\n"),
            false
        )
        .is_err());
        assert!(framing(
            &h("POST / HTTP/1.1\r\nContent-Length: 3\r\nContent-Length: 4\r\n\r\n"),
            false
        )
        .is_err());
        assert_eq!(
            framing(&h("POST / HTTP/1.1\r\nContent-Length: 3\r\nContent-Length: 3\r\n\r\n"), false).unwrap(),
            Body::Length(3)
        );
        assert!(framing(&h("POST / HTTP/1.1\r\nContent-Length: x\r\n\r\n"), false).is_err());
        assert_eq!(framing(&h("X / HTTP/1.1\r\n\r\n"), true).unwrap(), Body::UntilClose);
    }

    #[test]
    fn conn_reads_heads_lines_and_bodies_across_small_reads() {
        struct Slow(Cursor<Vec<u8>>);
        impl Read for Slow {
            fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
                let n = b.len().min(3);
                self.0.read(&mut b[..n])
            }
        }
        let data = b"GET / HTTP/1.1\r\nHost: x\r\n\r\nhello5\r\nabcde\r\n0\r\n\r\n".to_vec();
        let mut c = Conn::new(Slow(Cursor::new(data)));
        let head = c.read_head().unwrap().unwrap();
        assert!(head.ends_with(b"\r\n\r\n"));
        let mut body = Vec::new();
        c.copy_exact(5, &mut body).unwrap();
        assert_eq!(body, b"hello");
        let mut chunks = Vec::new();
        c.copy_chunked(&mut chunks).unwrap();
        assert_eq!(chunks, b"5\r\nabcde\r\n0\r\n\r\n");
        assert!(c.read_head().unwrap().is_none());
    }

    #[test]
    fn an_oversized_head_is_refused() {
        let mut data = b"GET / HTTP/1.1\r\n".to_vec();
        data.extend(std::iter::repeat_n(b'a', MAX_HEAD + 10));
        let mut c = Conn::new(Cursor::new(data));
        assert!(c.read_head().is_err());
    }

    #[test]
    fn chunk_helpers() {
        assert_eq!(chunk(b"abc"), b"3\r\nabc\r\n");
        assert_eq!(chunk_size(b"1A;x=y\r\n").unwrap(), 26);
        assert!(chunk_size(b"zz\r\n").is_err());
    }

    #[test]
    fn header_edits() {
        let mut r = req("GET / HTTP/1.1\r\nHost: a\r\nConnection: keep-alive, Upgrade\r\n\r\n");
        assert!(r.head.has_token("connection", "upgrade"));
        r.head.set("host", b"b");
        assert_eq!(r.head.get_str("Host"), Some("b"));
        r.head.remove("connection");
        assert!(r.head.get("connection").is_none());
    }
}
