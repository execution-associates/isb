//! The HTTP/1.1 loop of an intercepted connection: requests from the guest
//! get their placeholders replaced by real values on the way to the
//! approved host, and what comes back is scrubbed of real values before the
//! guest sees it.
//!
//! Limits, by design: HTTP/1.1 only (the proxy offers no `h2`); bodies of
//! requests are passed through unchanged (only the request line and the
//! headers are rewritten); a response that arrives content-encoded cannot be
//! scrubbed (the proxy asks for `identity`); a protocol upgrade
//! (WebSocket) is tunnelled without scrubbing.

use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

use crate::http1::{self, Body, Conn, Request, chunk};
use crate::rewrite::{self, Pair, Scrubber};
use crate::sniff::host_only;

/// Responses up to this size keep their `Content-Length` (they are scrubbed
/// whole); larger ones are streamed and re-chunked.
const BUFFER_LIMIT: u64 = 1024 * 1024;

/// What the proxy knows about one intercepted connection.
pub struct Rewrite {
    /// The host the TLS handshake named: the only `Host` a request may carry.
    pub host: String,
    pub pairs: Vec<Pair>,
}

/// How the loop ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Done {
    Closed,
    /// A `101 Switching Protocols`: the caller tunnels the rest.
    Upgraded,
}

/// What to do after a response.
#[derive(Debug, PartialEq, Eq)]
enum Next {
    KeepAlive,
    Close,
    Upgraded,
}

fn reply<S: Write>(c: &mut Conn<S>, status: &str, why: &str) -> io::Result<()> {
    let body = format!("isb egress: {why}\n");
    write!(
        c.s,
        "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    c.s.flush()
}

/// Serve requests from `client` to `up` until either side is done.
pub fn serve<C: Read + Write, U: Read + Write>(
    client: &mut Conn<C>,
    up: &mut Conn<U>,
    rw: &Rewrite,
) -> io::Result<Done> {
    loop {
        let Some(raw) = client.read_head()? else {
            return Ok(Done::Closed);
        };
        let mut req = match http1::parse_request(&raw) {
            Ok(r) => r,
            Err(e) => {
                reply(client, "400 Bad Request", &e)?;
                return Ok(Done::Closed);
            }
        };
        let body = match http1::framing(&req.head, false) {
            Ok(b) => b,
            Err(e) => {
                reply(client, "400 Bad Request", &e)?;
                return Ok(Done::Closed);
            }
        };
        // Inside the tunnel the Host must be the host the handshake named,
        // or a guest could ask a shared front end for another site.
        if req.head.get_str("host").map(host_only).as_deref() != Some(rw.host.as_str()) {
            reply(
                client,
                "421 Misdirected Request",
                "Host does not match the TLS server name",
            )?;
            return Ok(Done::Closed);
        }
        let close_after_req = req.head.has_token("connection", "close") || req.version == 0;
        let upgrade =
            req.head.has_token("connection", "upgrade") && req.head.get("upgrade").is_some();
        prepare(&mut req, rw);
        if body != Body::None && req.head.has_token("expect", "100-continue") {
            req.head.remove("expect");
            client.s.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
            client.s.flush()?;
        }
        up.s.write_all(&req.head.to_bytes())?;
        match body {
            Body::Length(n) => client.copy_exact(n, &mut up.s)?,
            Body::Chunked => client.copy_chunked(&mut up.s)?,
            Body::None | Body::UntilClose => {}
        }
        up.s.flush()?;
        match respond(client, up, rw, &req.method, upgrade)? {
            Next::Upgraded => return Ok(Done::Upgraded),
            Next::Close => return Ok(Done::Closed),
            Next::KeepAlive if close_after_req => return Ok(Done::Closed),
            Next::KeepAlive => {}
        }
    }
}

/// Rewrite a request in place: the real values in, and `identity` encoding.
fn prepare(req: &mut Request, rw: &Rewrite) {
    for (_, v) in req.head.headers.iter_mut() {
        *v = rewrite::request_header(v, &rw.pairs);
    }
    req.target = rewrite::request_target(&req.target, &rw.pairs);
    req.rebuild_first();
    req.head.set("Accept-Encoding", b"identity");
}

/// Read the response(s) to a request and send them on, scrubbed.
fn respond<C: Read + Write, U: Read + Write>(
    client: &mut Conn<C>,
    up: &mut Conn<U>,
    rw: &Rewrite,
    method: &str,
    upgrade: bool,
) -> io::Result<Next> {
    let patterns = rewrite::scrub_patterns(&rw.pairs);
    loop {
        let Some(raw) = up.read_head()? else {
            reply(client, "502 Bad Gateway", "the host closed the connection")?;
            return Ok(Next::Close);
        };
        let mut resp = match http1::parse_response(&raw) {
            Ok(r) => r,
            Err(e) => {
                reply(client, "502 Bad Gateway", &e)?;
                return Ok(Next::Close);
            }
        };
        for (_, v) in resp.head.headers.iter_mut() {
            *v = rewrite::response_header(v, &patterns);
        }
        let status = resp.status;
        if status == 101 && upgrade {
            client.s.write_all(&resp.head.to_bytes())?;
            client.s.flush()?;
            return Ok(Next::Upgraded);
        }
        let bodiless =
            method == "HEAD" || (100..200).contains(&status) || status == 204 || status == 304;
        if bodiless {
            client.s.write_all(&resp.head.to_bytes())?;
            client.s.flush()?;
            if (100..200).contains(&status) {
                continue;
            }
            return Ok(keep_alive(&resp.head));
        }
        let framing = match http1::framing(&resp.head, true) {
            Ok(f) => f,
            Err(e) => {
                reply(client, "502 Bad Gateway", &e)?;
                return Ok(Next::Close);
            }
        };
        let mut scrub = Scrubber::new(patterns);
        let next = send_body(client, up, &mut resp.head, framing, &mut scrub)?;
        client.s.flush()?;
        return Ok(next);
    }
}

fn keep_alive(h: &http1::Head) -> Next {
    if h.has_token("connection", "close") {
        Next::Close
    } else {
        Next::KeepAlive
    }
}

/// Send a response body on, scrubbed, and the head with it.
fn send_body<C: Read + Write, U: Read + Write>(
    client: &mut Conn<C>,
    up: &mut Conn<U>,
    head: &mut http1::Head,
    framing: Body,
    scrub: &mut Scrubber,
) -> io::Result<Next> {
    match framing {
        Body::None => {
            client.s.write_all(&head.to_bytes())?;
            Ok(keep_alive(head))
        }
        Body::Length(n) if scrub.is_noop() => {
            client.s.write_all(&head.to_bytes())?;
            up.copy_exact(n, &mut client.s)?;
            Ok(keep_alive(head))
        }
        Body::Length(n) if n <= BUFFER_LIMIT => {
            let raw = up.read_exact_vec(usize::try_from(n).unwrap_or(0))?;
            let mut out = scrub.feed(&raw);
            out.extend(scrub.finish());
            head.set("Content-Length", out.len().to_string().as_bytes());
            client.s.write_all(&head.to_bytes())?;
            client.s.write_all(&out)?;
            Ok(keep_alive(head))
        }
        Body::Length(mut n) => {
            head.remove("content-length");
            head.set("Transfer-Encoding", b"chunked");
            client.s.write_all(&head.to_bytes())?;
            while n > 0 {
                let b = up.read_some(usize::try_from(n.min(64 * 1024)).unwrap_or(64 * 1024))?;
                if b.is_empty() {
                    return Err(io::ErrorKind::UnexpectedEof.into());
                }
                n -= b.len() as u64;
                let out = scrub.feed(&b);
                if !out.is_empty() {
                    client.s.write_all(&chunk(&out))?;
                }
            }
            let tail = scrub.finish();
            if !tail.is_empty() {
                client.s.write_all(&chunk(&tail))?;
            }
            client.s.write_all(b"0\r\n\r\n")?;
            Ok(keep_alive(head))
        }
        Body::Chunked => {
            client.s.write_all(&head.to_bytes())?;
            if scrub.is_noop() {
                up.copy_chunked(&mut client.s)?;
                return Ok(keep_alive(head));
            }
            loop {
                let size = http1::chunk_size(&up.read_line()?)?;
                if size == 0 {
                    while up.read_line()? != b"\r\n" {}
                    break;
                }
                let data = up.read_exact_vec(usize::try_from(size).unwrap_or(0))?;
                up.read_exact_vec(2)?;
                let out = scrub.feed(&data);
                if !out.is_empty() {
                    client.s.write_all(&chunk(&out))?;
                }
            }
            let tail = scrub.finish();
            if !tail.is_empty() {
                client.s.write_all(&chunk(&tail))?;
            }
            client.s.write_all(b"0\r\n\r\n")?;
            Ok(keep_alive(head))
        }
        Body::UntilClose => {
            head.set("Connection", b"close");
            client.s.write_all(&head.to_bytes())?;
            loop {
                let b = up.read_some(64 * 1024)?;
                if b.is_empty() {
                    break;
                }
                client.s.write_all(&scrub.feed(&b))?;
            }
            client.s.write_all(&scrub.finish())?;
            Ok(Next::Close)
        }
    }
}

/// Copy both ways between two streams whose reads time out quickly (a few
/// milliseconds), until one side ends or nothing moves for `idle`.
pub fn tunnel<C: Read + Write, U: Read + Write>(
    client: &mut Conn<C>,
    up: &mut Conn<U>,
    idle: Duration,
) -> io::Result<()> {
    let to_up = client.take_buffered();
    if !to_up.is_empty() {
        up.s.write_all(&to_up)?;
    }
    let to_client = up.take_buffered();
    if !to_client.is_empty() {
        client.s.write_all(&to_client)?;
    }
    let mut last = Instant::now();
    let mut buf = [0u8; 16 * 1024];
    loop {
        let mut moved = false;
        for dir in 0..2 {
            let r = if dir == 0 {
                client.s.read(&mut buf)
            } else {
                up.s.read(&mut buf)
            };
            match r {
                Ok(0) => return Ok(()),
                Ok(n) => {
                    moved = true;
                    let w = if dir == 0 {
                        up.s.write_all(&buf[..n])
                    } else {
                        client.s.write_all(&buf[..n])
                    };
                    w?;
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
                Err(e) => return Err(e),
            }
        }
        if moved {
            last = Instant::now();
        } else if last.elapsed() > idle {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct Duplex {
        input: Cursor<Vec<u8>>,
        output: Vec<u8>,
    }

    impl Duplex {
        fn new(input: &[u8]) -> Duplex {
            Duplex {
                input: Cursor::new(input.to_vec()),
                output: Vec::new(),
            }
        }
    }

    impl Read for Duplex {
        fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
            self.input.read(b)
        }
    }

    impl Write for Duplex {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.output.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn rw() -> Rewrite {
        Rewrite {
            host: "api.example.com".into(),
            pairs: vec![Pair {
                placeholder: b"isb_placeholder_abc".to_vec(),
                real: b"REALSECRET".to_vec(),
            }],
        }
    }

    fn run(request: &str, response: &str) -> (String, String, Done) {
        let mut c = Conn::new(Duplex::new(request.as_bytes()));
        let mut u = Conn::new(Duplex::new(response.as_bytes()));
        let done = serve(&mut c, &mut u, &rw()).unwrap();
        (
            String::from_utf8_lossy(&u.s.output).into_owned(),
            String::from_utf8_lossy(&c.s.output).into_owned(),
            done,
        )
    }

    #[test]
    fn the_real_value_goes_up_and_the_placeholder_comes_back() {
        let (up, down, _) = run(
            "GET /v1?k=isb_placeholder_abc HTTP/1.1\r\nHost: api.example.com\r\nAuthorization: Bearer isb_placeholder_abc\r\nAccept-Encoding: gzip\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 29\r\nX-Echo: REALSECRET\r\n\r\n{\"auth\":\"Bearer REALSECRET\"}\n",
        );
        assert!(up.contains("Authorization: Bearer REALSECRET\r\n"), "{up}");
        assert!(up.starts_with("GET /v1?k=REALSECRET HTTP/1.1\r\n"), "{up}");
        assert!(up.contains("Accept-Encoding: identity\r\n"), "{up}");
        assert!(!down.contains("REALSECRET"), "{down}");
        assert!(down.contains("X-Echo: isb_placeholder_abc"), "{down}");
        assert!(
            down.contains("{\"auth\":\"Bearer isb_placeholder_abc\"}"),
            "{down}"
        );
        // The length follows the rewritten body.
        let n = "{\"auth\":\"Bearer isb_placeholder_abc\"}\n".len();
        assert!(down.contains(&format!("Content-Length: {n}\r\n")), "{down}");
    }

    #[test]
    fn a_post_body_is_forwarded_unchanged() {
        let (up, _, _) = run(
            "POST /x HTTP/1.1\r\nHost: api.example.com\r\nContent-Length: 19\r\n\r\nisb_placeholder_abc",
            "HTTP/1.1 204 No Content\r\n\r\n",
        );
        assert!(up.ends_with("\r\n\r\nisb_placeholder_abc"), "{up}");
    }

    #[test]
    fn chunked_bodies_both_ways() {
        let (up, down, _) = run(
            "POST /x HTTP/1.1\r\nHost: api.example.com\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\n",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nREALS\r\n5\r\nECRET\r\n0\r\n\r\n",
        );
        assert!(up.ends_with("3\r\nabc\r\n0\r\n\r\n"), "{up}");
        assert!(
            !down.contains("REALSECRET") && !down.contains("REALS"),
            "{down}"
        );
        assert!(down.contains("isb_placeholder_abc"), "{down}");
        assert!(down.ends_with("0\r\n\r\n"), "{down}");
    }

    #[test]
    fn a_mismatched_host_is_refused() {
        let (up, down, done) = run(
            "GET / HTTP/1.1\r\nHost: other.example.com\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n",
        );
        assert!(up.is_empty(), "nothing was sent upstream: {up}");
        assert!(down.starts_with("HTTP/1.1 421"), "{down}");
        assert_eq!(done, Done::Closed);
    }

    #[test]
    fn smuggling_shapes_are_refused() {
        let (up, down, _) = run(
            "POST / HTTP/1.1\r\nHost: api.example.com\r\nContent-Length: 4\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
            "",
        );
        assert!(up.is_empty());
        assert!(down.starts_with("HTTP/1.1 400"), "{down}");
    }

    #[test]
    fn two_requests_on_one_connection() {
        let (up, down, _) = run(
            "GET /a HTTP/1.1\r\nHost: api.example.com\r\nX: isb_placeholder_abc\r\n\r\nGET /b HTTP/1.1\r\nHost: api.example.com\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\nAHTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\nB",
        );
        assert_eq!(up.matches("GET /").count(), 2, "{up}");
        assert!(
            down.contains("\r\n\r\nA") && down.ends_with("\r\n\r\nB"),
            "{down}"
        );
    }

    #[test]
    fn expect_continue_is_answered_locally() {
        let (up, down, _) = run(
            "POST / HTTP/1.1\r\nHost: api.example.com\r\nExpect: 100-continue\r\nContent-Length: 2\r\n\r\nhi",
            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n",
        );
        assert!(down.starts_with("HTTP/1.1 100 Continue\r\n\r\n"), "{down}");
        assert!(!up.to_ascii_lowercase().contains("expect"), "{up}");
    }

    #[test]
    fn an_upgrade_is_handed_back() {
        let (_, down, done) = run(
            "GET /ws HTTP/1.1\r\nHost: api.example.com\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n",
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n",
        );
        assert_eq!(done, Done::Upgraded);
        assert!(down.starts_with("HTTP/1.1 101"), "{down}");
    }

    #[test]
    fn a_large_body_is_streamed_scrubbed_and_rechunked() {
        let n = (BUFFER_LIMIT + 10) as usize;
        let mut body = vec![b'x'; n];
        body[100..110].copy_from_slice(b"REALSECRET");
        let mut resp = format!("HTTP/1.1 200 OK\r\nContent-Length: {n}\r\n\r\n").into_bytes();
        resp.extend_from_slice(&body);
        let mut c = Conn::new(Duplex::new(
            b"GET / HTTP/1.1\r\nHost: api.example.com\r\n\r\n",
        ));
        let mut u = Conn::new(Duplex::new(&resp));
        serve(&mut c, &mut u, &rw()).unwrap();
        let down = String::from_utf8_lossy(&c.s.output).into_owned();
        assert!(
            down.contains("Transfer-Encoding: chunked"),
            "streamed responses are chunked"
        );
        assert!(!down.contains("REALSECRET"));
        assert!(down.contains("isb_placeholder_abc"));
    }
}
