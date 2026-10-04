//! A minimal SMTP submission client: one message, plain text, over STARTTLS
//! or implicit TLS (rustls, the same stack as the rest of isb), AUTH PLAIN or
//! LOGIN. Enough for notifications; not a mail library.

use std::io::{Read, Write};
use std::net::TcpStream;

use serde::{Deserialize, Serialize};

use super::net::{Net, SendError};

/// How the connection is secured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SmtpTls {
    /// Plain, then STARTTLS (submission, port 587). Fails if not offered.
    #[default]
    Starttls,
    /// TLS from the first byte (port 465).
    Tls,
    /// No TLS at all; no password is ever sent this way.
    None,
}

impl SmtpTls {
    pub fn default_port(self) -> u16 {
        match self {
            SmtpTls::Starttls => 587,
            SmtpTls::Tls => 465,
            SmtpTls::None => 25,
        }
    }
}

/// One message to send.
pub struct Mail<'a> {
    pub host: &'a str,
    pub port: u16,
    pub tls: SmtpTls,
    pub username: Option<&'a str>,
    pub password: Option<&'a str>,
    pub from: &'a str,
    pub to: &'a [String],
    pub subject: &'a str,
    pub body: &'a str,
    /// Unix seconds, for the Date header.
    pub date: u64,
    pub message_id: &'a str,
}

enum Stream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Read for Stream {
    fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(b),
            Stream::Tls(s) => s.read(b),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(b),
            Stream::Tls(s) => s.write(b),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

struct Session<'a> {
    s: Option<Stream>,
    buf: Vec<u8>,
    host: &'a str,
}

/// A reply: its code and its lines' text.
struct Reply {
    code: u16,
    lines: Vec<String>,
}

impl Session<'_> {
    fn stream(&mut self) -> &mut Stream {
        self.s.as_mut().expect("a stream")
    }

    fn io(&self, e: std::io::Error) -> SendError {
        SendError::transient(format!("SMTP {}: {e}", self.host))
    }

    fn line(&mut self) -> Result<String, SendError> {
        loop {
            if let Some(p) = self.buf.windows(2).position(|w| w == b"\r\n") {
                let l: Vec<u8> = self.buf.drain(..p + 2).collect();
                return Ok(String::from_utf8_lossy(&l[..p]).into_owned());
            }
            if self.buf.len() > 4096 {
                return Err(SendError::transient(format!(
                    "SMTP {}: an overlong reply line",
                    self.host
                )));
            }
            let mut b = [0u8; 1024];
            let n = match self.stream().read(&mut b) {
                Ok(n) => n,
                Err(e) => return Err(self.io(e)),
            };
            if n == 0 {
                return Err(SendError::transient(format!(
                    "SMTP {}: the server closed the connection",
                    self.host
                )));
            }
            self.buf.extend_from_slice(&b[..n]);
        }
    }

    fn reply(&mut self) -> Result<Reply, SendError> {
        let mut lines = Vec::new();
        loop {
            let l = self.line()?;
            let code = l
                .get(..3)
                .and_then(|c| c.parse::<u16>().ok())
                .ok_or_else(|| {
                    SendError::transient(format!("SMTP {}: a bad reply line", self.host))
                })?;
            let more = l.as_bytes().get(3) == Some(&b'-');
            lines.push(l.get(4..).unwrap_or_default().to_string());
            if lines.len() > 100 {
                return Err(SendError::transient(format!(
                    "SMTP {}: an overlong reply",
                    self.host
                )));
            }
            if !more {
                return Ok(Reply { code, lines });
            }
        }
    }

    fn send(&mut self, data: &[u8]) -> Result<(), SendError> {
        let r = self
            .stream()
            .write_all(data)
            .and_then(|_| self.stream().flush());
        r.map_err(|e| self.io(e))
    }

    /// Send a command and expect one of `ok`. `shown` replaces the command
    /// in errors (for credentials).
    fn cmd(&mut self, c: &str, ok: &[u16], shown: Option<&str>) -> Result<Reply, SendError> {
        self.send(format!("{c}\r\n").as_bytes())?;
        self.expect(ok, shown.unwrap_or(c))
    }

    fn expect(&mut self, ok: &[u16], what: &str) -> Result<Reply, SendError> {
        let r = self.reply()?;
        if ok.contains(&r.code) {
            return Ok(r);
        }
        let text = r.lines.join(" ");
        let text: String = text.chars().take(200).collect();
        let m = format!("SMTP {}: {what}: {} {text}", self.host, r.code);
        // 4xx is temporary by definition; 5xx is not.
        Err(if (400..500).contains(&r.code) {
            SendError::transient(m)
        } else {
            SendError::permanent(m)
        })
    }
}

fn ehlo_has(r: &Reply, ext: &str) -> bool {
    r.lines.iter().any(|l| {
        l.split_whitespace()
            .next()
            .is_some_and(|w| w.eq_ignore_ascii_case(ext))
    })
}

fn auth_mechs(r: &Reply) -> Vec<String> {
    r.lines
        .iter()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            w.next()
                .filter(|x| x.eq_ignore_ascii_case("AUTH"))
                .map(|_| w.map(|m| m.to_ascii_uppercase()).collect::<Vec<_>>())
        })
        .flatten()
        .collect()
}

/// A mailbox fit for a header and an envelope: no CR/LF, no angle brackets.
pub fn check_address(a: &str) -> Result<(), String> {
    let ok = a.len() <= 254
        && a.split_once('@')
            .is_some_and(|(l, d)| !l.is_empty() && !d.is_empty() && !d.contains('@'))
        && !a
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || "<>,;\"()[]\\".contains(c));
    if ok {
        Ok(())
    } else {
        Err(format!("{a:?} is not an email address"))
    }
}

/// Send one message.
pub fn send(net: &Net, m: &Mail) -> Result<(), SendError> {
    check_address(m.from).map_err(SendError::permanent)?;
    for t in m.to {
        check_address(t).map_err(SendError::permanent)?;
    }
    if m.to.is_empty() {
        return Err(SendError::permanent("no recipients"));
    }
    let tcp = super::net::connect(m.host, m.port, net.allow_private)?;
    let first = match m.tls {
        SmtpTls::Tls => Stream::Tls(Box::new(super::net::tls(net, m.host, tcp)?)),
        _ => Stream::Plain(tcp),
    };
    let mut s = Session {
        s: Some(first),
        buf: Vec::new(),
        host: m.host,
    };
    s.expect(&[220], "greeting")?;
    let mut ehlo = s.cmd("EHLO isb", &[250], None)?;
    if m.tls == SmtpTls::Starttls {
        if !ehlo_has(&ehlo, "STARTTLS") {
            return Err(SendError::permanent(format!(
                "SMTP {}: the server does not offer STARTTLS",
                m.host
            )));
        }
        s.cmd("STARTTLS", &[220], None)?;
        // Anything the server sent past its 220 would be read as if it came
        // over TLS (the STARTTLS injection attack): refuse.
        if !s.buf.is_empty() {
            return Err(SendError::permanent(format!(
                "SMTP {}: data after STARTTLS's reply",
                m.host
            )));
        }
        let Some(Stream::Plain(tcp)) = s.s.take() else {
            unreachable!("STARTTLS starts from a plain stream")
        };
        s.s = Some(Stream::Tls(Box::new(super::net::tls(net, m.host, tcp)?)));
        ehlo = s.cmd("EHLO isb", &[250], None)?;
    }
    if let (Some(user), Some(pass)) = (m.username, m.password) {
        if m.tls == SmtpTls::None {
            return Err(SendError::permanent(format!(
                "SMTP {}: refusing to send a password without TLS (use starttls or tls)",
                m.host
            )));
        }
        let mechs = auth_mechs(&ehlo);
        if mechs.iter().any(|x| x == "PLAIN") {
            let token = crate::rpc::b64_encode(format!("\0{user}\0{pass}").as_bytes());
            s.cmd(&format!("AUTH PLAIN {token}"), &[235], Some("AUTH PLAIN"))?;
        } else if mechs.iter().any(|x| x == "LOGIN") {
            s.cmd("AUTH LOGIN", &[334], None)?;
            s.cmd(
                &crate::rpc::b64_encode(user.as_bytes()),
                &[334],
                Some("AUTH LOGIN user"),
            )?;
            s.cmd(
                &crate::rpc::b64_encode(pass.as_bytes()),
                &[235],
                Some("AUTH LOGIN password"),
            )?;
        } else {
            return Err(SendError::permanent(format!(
                "SMTP {}: no supported AUTH mechanism (PLAIN, LOGIN) offered",
                m.host
            )));
        }
    }
    s.cmd(&format!("MAIL FROM:<{}>", m.from), &[250], None)?;
    for t in m.to {
        s.cmd(&format!("RCPT TO:<{t}>"), &[250, 251], None)?;
    }
    s.cmd("DATA", &[354], None)?;
    let msg = message(m);
    s.send(&msg)?;
    s.expect(&[250], "message")?;
    let _ = s.cmd("QUIT", &[221], None);
    Ok(())
}

/// The message: headers, then the body dot-stuffed with CRLF line ends,
/// then the terminating `.`.
pub fn message(m: &Mail) -> Vec<u8> {
    let mut out = String::new();
    out.push_str(&format!("From: {}\r\n", m.from));
    out.push_str(&format!("To: {}\r\n", m.to.join(", ")));
    out.push_str(&format!("Subject: {}\r\n", encode_header(m.subject)));
    out.push_str(&format!("Date: {}\r\n", rfc2822(m.date)));
    out.push_str(&format!("Message-ID: <{}>\r\n", m.message_id));
    out.push_str("MIME-Version: 1.0\r\n");
    out.push_str("Content-Type: text/plain; charset=utf-8\r\n");
    out.push_str("Content-Transfer-Encoding: 8bit\r\n");
    out.push_str("Auto-Submitted: auto-generated\r\n\r\n");
    for line in m.body.replace("\r\n", "\n").split('\n') {
        if line.starts_with('.') {
            out.push('.');
        }
        out.push_str(line);
        out.push_str("\r\n");
    }
    out.push_str(".\r\n");
    out.into_bytes()
}

/// A header value: as is when printable ASCII, else an RFC 2047 encoded
/// word. CR and LF never survive (header injection).
fn encode_header(s: &str) -> String {
    let s: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if s.is_ascii() {
        s
    } else {
        format!("=?utf-8?B?{}?=", crate::rpc::b64_encode(s.as_bytes()))
    }
}

/// `Sat, 03 Oct 2026 08:04:13 +0000`.
pub fn rfc2822(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let (y, mo, d) = civil(days);
    // 1970-01-01 was a Thursday.
    let wd = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][(days % 7) as usize];
    let mon = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ][(mo - 1) as usize];
    format!(
        "{wd}, {d:02} {mon} {y} {:02}:{:02}:{:02} +0000",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

/// Days since the epoch to (year, month, day): Howard Hinnant's algorithm.
fn civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn dates() {
        assert_eq!(rfc2822(0), "Thu, 01 Jan 1970 00:00:00 +0000");
        assert_eq!(rfc2822(1_791_014_653), "Sat, 03 Oct 2026 08:04:13 +0000");
        assert_eq!(rfc2822(951_782_400), "Tue, 29 Feb 2000 00:00:00 +0000");
    }

    #[test]
    fn addresses_and_headers() {
        assert!(check_address("ops@example.com").is_ok());
        for bad in [
            "x",
            "@x",
            "x@",
            "a@b@c",
            "a b@c",
            "a@c\r\nRCPT TO:<evil@x>",
            "<a@b>",
        ] {
            assert!(check_address(bad).is_err(), "{bad}");
        }
        assert_eq!(encode_header("a\r\nBcc: x"), "a  Bcc: x");
        assert_eq!(encode_header("é"), "=?utf-8?B?w6k=?=");
    }

    /// A scripted SMTP server: answers each command it is sent with the
    /// next canned reply, and records the conversation.
    fn fake(
        script: Vec<(&'static str, &'static str)>,
        tls: Option<Arc<rustls::ServerConfig>>,
    ) -> (u16, std::thread::JoinHandle<Vec<String>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            s.set_read_timeout(Some(std::time::Duration::from_secs(10)))
                .unwrap();
            let mut log = Vec::new();
            let mut plain = Some(s);
            let mut tls_stream: Option<rustls::StreamOwned<rustls::ServerConnection, TcpStream>> =
                None;
            let w = |data: &[u8], plain: &mut Option<TcpStream>, t: &mut Option<rustls::StreamOwned<rustls::ServerConnection, TcpStream>>| {
                match t {
                    Some(t) => { t.write_all(data).unwrap(); t.flush().unwrap(); }
                    None => plain.as_mut().unwrap().write_all(data).unwrap(),
                }
            };
            w(b"220 fake ESMTP\r\n", &mut plain, &mut tls_stream);
            let mut in_data = false;
            let mut script = script.into_iter();
            loop {
                // Byte by byte, so nothing past the line is consumed (before
                // a TLS handshake, or between loop turns).
                let mut line = String::new();
                let mut n = 0;
                let mut b = [0u8; 1];
                loop {
                    let r = match &mut tls_stream {
                        Some(t) => t.read(&mut b),
                        None => plain.as_mut().unwrap().read(&mut b),
                    };
                    if !matches!(r, Ok(1)) {
                        break;
                    }
                    line.push(b[0] as char);
                    n += 1;
                    if b[0] == b'\n' {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
                let l = line.trim_end_matches(['\r', '\n']).to_string();
                log.push(l.clone());
                if in_data {
                    if l == "." {
                        in_data = false;
                        w(b"250 queued\r\n", &mut plain, &mut tls_stream);
                    }
                    continue;
                }
                let Some((expect, reply)) = script.next() else {
                    break;
                };
                assert!(l.starts_with(expect), "expected {expect}, got {l}");
                w(
                    format!("{reply}\r\n").as_bytes(),
                    &mut plain,
                    &mut tls_stream,
                );
                if expect == "DATA" {
                    in_data = true;
                }
                if expect == "STARTTLS" {
                    let conn = rustls::ServerConnection::new(tls.clone().unwrap()).unwrap();
                    tls_stream = Some(rustls::StreamOwned::new(conn, plain.take().unwrap()));
                }
                if expect == "QUIT" {
                    break;
                }
            }
            log
        });
        (port, h)
    }

    use std::sync::Arc;

    fn mail<'a>(
        port: u16,
        tls: SmtpTls,
        to: &'a [String],
        user: Option<&'a str>,
        pass: Option<&'a str>,
    ) -> Mail<'a> {
        Mail {
            host: "127.0.0.1",
            port,
            tls,
            username: user,
            password: pass,
            from: "isb@example.com",
            to,
            subject: "[isb] deploy.failed",
            body: "line one\n.dot line\nend",
            date: 0,
            message_id: "1@isb",
        }
    }

    #[test]
    fn plain_conversation() {
        let (port, h) = fake(
            vec![
                ("EHLO isb", "250-fake\r\n250 8BITMIME"),
                ("MAIL FROM:<isb@example.com>", "250 ok"),
                ("RCPT TO:<a@example.com>", "250 ok"),
                ("RCPT TO:<b@example.com>", "251 forwarded"),
                ("DATA", "354 go"),
                ("QUIT", "221 bye"),
            ],
            None,
        );
        let to = vec!["a@example.com".to_string(), "b@example.com".to_string()];
        send(&Net::new(true), &mail(port, SmtpTls::None, &to, None, None)).unwrap();
        let log = h.join().unwrap();
        let i = log.iter().position(|l| l == "DATA").unwrap();
        let msg = &log[i + 1..];
        assert!(
            msg.contains(&"Subject: [isb] deploy.failed".to_string()),
            "{msg:?}"
        );
        assert!(msg.contains(&"To: a@example.com, b@example.com".to_string()));
        // Dot-stuffing.
        assert!(msg.contains(&"..dot line".to_string()), "{msg:?}");
        assert_eq!(msg.last().map(String::as_str), Some("QUIT"));
    }

    #[test]
    fn no_password_without_tls_and_errors_hide_it() {
        let (port, h) = fake(vec![("EHLO isb", "250-fake\r\n250 AUTH PLAIN")], None);
        let to = vec!["a@example.com".to_string()];
        let e = send(
            &Net::new(true),
            &mail(port, SmtpTls::None, &to, Some("u"), Some("hunter2")),
        )
        .unwrap_err();
        assert!(e.message.contains("without TLS"), "{e}");
        assert!(!e.message.contains("hunter2"));
        drop(h);
    }

    #[test]
    fn starttls_required_when_asked_for() {
        let (port, _h) = fake(vec![("EHLO isb", "250 fake")], None);
        let to = vec!["a@example.com".to_string()];
        let e = send(
            &Net::new(true),
            &mail(port, SmtpTls::Starttls, &to, None, None),
        )
        .unwrap_err();
        assert!(e.message.contains("does not offer STARTTLS"), "{e}");
    }

    #[test]
    fn rejected_recipient_is_permanent_and_busy_is_transient() {
        let (port, _h) = fake(
            vec![
                ("EHLO isb", "250 fake"),
                ("MAIL FROM", "250 ok"),
                ("RCPT TO", "550 no such user"),
            ],
            None,
        );
        let to = vec!["a@example.com".to_string()];
        let e = send(&Net::new(true), &mail(port, SmtpTls::None, &to, None, None)).unwrap_err();
        assert!(!e.retryable && e.message.contains("550"), "{e}");
        let (port, _h) = fake(vec![("EHLO isb", "421 busy")], None);
        let e = send(&Net::new(true), &mail(port, SmtpTls::None, &to, None, None)).unwrap_err();
        assert!(e.retryable, "{e}");
    }

    #[test]
    fn starttls_and_auth_plain() {
        // A throwaway CA and a certificate for 127.0.0.1.
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let ca = || {
            let mut ca = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
            ca.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
            ca
        };
        let ca_cert = ca().self_signed(&ca_key).unwrap();
        let issuer = rcgen::Issuer::new(ca(), &ca_key);
        let key = rcgen::KeyPair::generate().unwrap();
        let leaf = rcgen::CertificateParams::new(vec!["127.0.0.1".to_string()])
            .unwrap()
            .signed_by(&key, &issuer)
            .unwrap();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let server = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![leaf.der().clone()],
                rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into()),
            )
            .unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(ca_cert.der().clone()).unwrap();
        let net = Net {
            allow_private: true,
            tls: super::super::net::tls_with_roots(roots),
        };
        let (port, h) = fake(
            vec![
                ("EHLO isb", "250-fake\r\n250 STARTTLS"),
                ("STARTTLS", "220 go ahead"),
                ("EHLO isb", "250-fake\r\n250 AUTH LOGIN PLAIN"),
                ("AUTH PLAIN AHUAaHVudGVyMg==", "235 ok"),
                ("MAIL FROM", "250 ok"),
                ("RCPT TO", "250 ok"),
                ("DATA", "354 go"),
                ("QUIT", "221 bye"),
            ],
            Some(Arc::new(server)),
        );
        let to = vec!["a@example.com".to_string()];
        send(
            &net,
            &mail(port, SmtpTls::Starttls, &to, Some("u"), Some("hunter2")),
        )
        .unwrap();
        let log = h.join().unwrap();
        assert!(log.iter().any(|l| l.starts_with("AUTH PLAIN")));
    }
}
