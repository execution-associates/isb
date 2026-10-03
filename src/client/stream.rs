//! Requests whose bodies are too large to hold in memory: a custom volume's
//! export (a tarball streamed out of incusd) and its import (one streamed
//! in, chunked). Every socket read and write is bounded by an idle timeout
//! rather than an overall one: a large volume takes as long as it takes.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use super::{Client, Envelope, RawResponse, Reply, complete_response, eof_body};
use crate::error::{Error, Result};

/// The body of a streamed response.
enum Framing {
    Length(u64),
    Chunked { left: u64, done: bool },
    Eof,
}

/// A response body read off the socket as the caller pulls it.
pub(crate) struct BodyReader {
    r: BufReader<std::io::Chain<std::io::Cursor<Vec<u8>>, UnixStream>>,
    framing: Framing,
}

fn bad(e: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(format!("bad chunked body: {e}"))
}

impl BodyReader {
    /// The next chunk's size; 0 at the end (trailers consumed).
    fn chunk_size(&mut self) -> std::io::Result<u64> {
        let mut line = String::new();
        self.r.read_line(&mut line)?;
        let hex = line.split(';').next().unwrap_or("").trim();
        let n = u64::from_str_radix(hex, 16).map_err(bad)?;
        if n == 0 {
            // Trailers, up to the empty line.
            loop {
                line.clear();
                if self.r.read_line(&mut line)? == 0 || line.trim().is_empty() {
                    break;
                }
            }
        }
        Ok(n)
    }
}

impl Read for BodyReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        match &mut self.framing {
            Framing::Eof => self.r.read(out),
            Framing::Length(0) => Ok(0),
            Framing::Length(left) => {
                let max = out.len().min(*left as usize);
                let n = self.r.read(&mut out[..max])?;
                if n == 0 {
                    return Err(std::io::ErrorKind::UnexpectedEof.into());
                }
                *left -= n as u64;
                Ok(n)
            }
            Framing::Chunked { done: true, .. } => Ok(0),
            Framing::Chunked { left: 0, .. } => {
                let n = self.chunk_size()?;
                self.framing = Framing::Chunked {
                    left: n,
                    done: n == 0,
                };
                self.read(out)
            }
            Framing::Chunked { left, .. } => {
                let max = out.len().min(*left as usize);
                let n = self.r.read(&mut out[..max])?;
                if n == 0 {
                    return Err(std::io::ErrorKind::UnexpectedEof.into());
                }
                *left -= n as u64;
                if *left == 0 {
                    let mut crlf = [0u8; 2];
                    self.r.read_exact(&mut crlf)?;
                }
                Ok(n)
            }
        }
    }
}

/// Read a response head; returns it parsed and the bytes after it.
fn read_head(stream: &mut UnixStream) -> Result<(u16, Framing, Vec<u8>)> {
    let mut buf = Vec::with_capacity(8192);
    let mut chunk = [0u8; 16384];
    loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Err(Error::Protocol("incusd closed before a response".into()));
        }
        buf.extend_from_slice(&chunk[..n]);
        let mut headers = [httparse::EMPTY_HEADER; 64];
        let mut resp = httparse::Response::new(&mut headers);
        let head = match resp
            .parse(&buf)
            .map_err(|e| Error::Protocol(format!("bad HTTP response: {e}")))?
        {
            httparse::Status::Complete(h) => h,
            httparse::Status::Partial => continue,
        };
        let mut framing = Framing::Eof;
        for h in resp.headers.iter() {
            let v = String::from_utf8_lossy(h.value).trim().to_ascii_lowercase();
            if h.name.eq_ignore_ascii_case("content-length") {
                if let Ok(n) = v.parse() {
                    framing = Framing::Length(n);
                }
            } else if h.name.eq_ignore_ascii_case("transfer-encoding") && v.contains("chunked") {
                framing = Framing::Chunked {
                    left: 0,
                    done: false,
                };
                break;
            }
        }
        return Ok((resp.code.unwrap_or(0), framing, buf[head..].to_vec()));
    }
}

/// The error an incusd response carries.
fn api_error(method: &str, path: &str, status: u16, body: &[u8]) -> Error {
    Error::Api {
        method: method.into(),
        path: path.into(),
        status,
        message: serde_json::from_slice::<Envelope>(body)
            .map(|e| e.error)
            .ok()
            .filter(|e| !e.is_empty())
            .unwrap_or_else(|| format!("HTTP {status}")),
    }
}

/// Decode an envelope into a reply, as [`Client::request`] does.
fn decode(method: &str, path: &str, r: RawResponse) -> Result<Reply> {
    let env: Envelope = serde_json::from_slice(&r.body).map_err(|e| {
        Error::Protocol(format!(
            "{method} {path}: HTTP {}, undecodable body ({e})",
            r.status
        ))
    })?;
    if env.kind == "error" || r.status >= 400 {
        return Err(api_error(method, path, r.status, &r.body));
    }
    Ok(if env.kind == "async" {
        Reply::Async {
            operation: env.operation,
            metadata: env.metadata,
        }
    } else {
        Reply::Sync(env.metadata)
    })
}

impl Client {
    fn open(&self, idle: Duration) -> Result<UnixStream> {
        let s = UnixStream::connect(&self.socket).map_err(|source| Error::Connect {
            socket: self.socket.display().to_string(),
            source,
        })?;
        s.set_read_timeout(Some(idle))?;
        s.set_write_timeout(Some(idle))?;
        Ok(s)
    }

    /// `GET` a raw body (an export) as a stream. A status of 400 or more is
    /// an error with incusd's message.
    pub(crate) fn get_stream(&self, path: &str, idle: Duration) -> Result<BodyReader> {
        let full = self.with_project(path);
        let mut s = self.open(idle)?;
        let head = format!(
            "GET {full} HTTP/1.1\r\nHost: incus\r\nUser-Agent: isb/{}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
            env!("CARGO_PKG_VERSION")
        );
        s.write_all(head.as_bytes())?;
        let (status, framing, rest) = read_head(&mut s)?;
        let mut body = BodyReader {
            r: BufReader::with_capacity(256 << 10, std::io::Cursor::new(rest).chain(s)),
            framing,
        };
        if status >= 400 {
            let mut b = Vec::new();
            let _ = body.by_ref().take(64 << 10).read_to_end(&mut b);
            return Err(api_error("GET", &full, status, &b));
        }
        Ok(body)
    }

    /// `POST` a body read from `body`, chunked (its length is not known
    /// ahead), with `headers`. Returns the reply (usually an operation).
    pub(crate) fn post_stream(
        &self,
        path: &str,
        headers: &[(&str, String)],
        body: &mut dyn Read,
        idle: Duration,
    ) -> Result<Reply> {
        let full = self.with_project(path);
        let mut s = self.open(idle)?;
        let mut head = format!(
            "POST {full} HTTP/1.1\r\nHost: incus\r\nUser-Agent: isb/{}\r\nConnection: close\r\nTransfer-Encoding: chunked\r\n",
            env!("CARGO_PKG_VERSION")
        );
        for (k, v) in headers {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str("\r\n");
        let sent = send_chunked(&mut s, head.as_bytes(), body);
        // incusd may answer (an error) before taking the whole body: read
        // what it said either way.
        let r = read_all(&mut s);
        match (sent, r) {
            (_, Ok(r)) if r.status >= 400 => Err(api_error("POST", &full, r.status, &r.body)),
            (Err(e), _) => Err(e),
            (Ok(()), Ok(r)) => decode("POST", &full, r),
            (Ok(()), Err(e)) => Err(e),
        }
    }
}

fn send_chunked(s: &mut UnixStream, head: &[u8], body: &mut dyn Read) -> Result<()> {
    s.write_all(head)?;
    let mut buf = vec![0u8; 256 << 10];
    loop {
        let n = match body.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(Error::invalid(format!("reading the body: {e}"))),
        };
        s.write_all(format!("{n:x}\r\n").as_bytes())?;
        s.write_all(&buf[..n])?;
        s.write_all(b"\r\n")?;
    }
    s.write_all(b"0\r\n\r\n")?;
    s.flush()?;
    Ok(())
}

/// A whole (small) response.
fn read_all(s: &mut UnixStream) -> Result<RawResponse> {
    let mut buf = Vec::with_capacity(8192);
    let mut chunk = [0u8; 16384];
    loop {
        let n = s.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(done) = complete_response(&buf)? {
            return Ok(done);
        }
    }
    complete_response(&buf)?
        .or_else(|| eof_body(&buf))
        .ok_or_else(|| Error::Protocol("truncated response from incusd".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(raw: &[u8], framing: Framing) -> Vec<u8> {
        let (a, b) = UnixStream::pair().unwrap();
        drop(a);
        let mut r = BodyReader {
            r: BufReader::with_capacity(7, std::io::Cursor::new(raw.to_vec()).chain(b)),
            framing,
        };
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        out
    }

    #[test]
    fn reads_framed_bodies() {
        assert_eq!(body(b"hello world", Framing::Length(5)), b"hello");
        assert_eq!(body(b"hello world", Framing::Eof), b"hello world");
        let chunked = b"5\r\nhello\r\n6;x=y\r\n world\r\n0\r\nX-T: 1\r\n\r\n";
        assert_eq!(
            body(
                chunked,
                Framing::Chunked {
                    left: 0,
                    done: false
                }
            ),
            b"hello world"
        );
    }

    #[test]
    fn sends_chunks() {
        let (mut a, mut b) = UnixStream::pair().unwrap();
        let t = std::thread::spawn(move || {
            let mut got = Vec::new();
            b.read_to_end(&mut got).unwrap();
            got
        });
        send_chunked(&mut a, b"HEAD\r\n\r\n", &mut &b"abc"[..]).unwrap();
        drop(a);
        assert_eq!(t.join().unwrap(), b"HEAD\r\n\r\n3\r\nabc\r\n0\r\n\r\n");
    }
}
