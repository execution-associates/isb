//! Template logos, served from isb's own origin:
//! `GET /api/v1/templates/<catalog>/<id>/logo`.
//!
//! A template's logo is a third-party https URL (a trademark, so isb links
//! to it and never ships a copy). The page's CSP keeps `img-src` to isb
//! itself, so browsers never reach the logo's host: the daemon fetches it
//! once, keeps it under `<state>/templates/logos/` for a week, and serves
//! it from there.
//!
//! What comes back is third-party data. Only https is fetched, every
//! address is held to the notification SSRF policy (no loopback, private,
//! link-local or other non-public destination, connected to the address
//! that was checked), redirects stay on https and are capped, the body is
//! capped at [`MAX_BYTES`], and only bytes that sniff as PNG, JPEG, GIF,
//! WebP, ICO or SVG are kept and served, with the type isb decided and
//! `nosniff`. An SVG is served under a sandboxing CSP too: through `<img>`
//! it cannot run script anyway, but the URL may be opened directly.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use crate::notify::net;
use crate::server::access::ASSERTION_HEADER;
use crate::server::http::{Peer, Request, Response};
use crate::server::mcp::Authn;
use crate::server::{AccessValidator, Authenticated, Caller};
use crate::template::catalog::Catalogs;

/// The largest logo kept.
pub const MAX_BYTES: usize = 512 * 1024;
/// How long a fetched logo is served before it is fetched again.
const FRESH_FOR: Duration = Duration::from_secs(7 * 86400);
/// How long a failed fetch is remembered (and not retried).
const FAILED_FOR: Duration = Duration::from_secs(600);
/// The longest one fetch may take, redirects included.
const FETCH_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_REDIRECTS: usize = 3;
/// How long the template-to-logo index is reused, and how soon a ref it
/// lacks (a catalog just added) may rebuild it.
const INDEX_FOR: Duration = Duration::from_secs(60);
const INDEX_MISS_AFTER: Duration = Duration::from_secs(5);

/// What a logo is, decided from its bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Png,
    Jpeg,
    Gif,
    Webp,
    Ico,
    Svg,
}

impl Kind {
    pub fn mime(self) -> &'static str {
        match self {
            Kind::Png => "image/png",
            Kind::Jpeg => "image/jpeg",
            Kind::Gif => "image/gif",
            Kind::Webp => "image/webp",
            Kind::Ico => "image/x-icon",
            Kind::Svg => "image/svg+xml",
        }
    }
}

/// An image's type from its first bytes, or `None` for anything else.
pub fn sniff(b: &[u8]) -> Option<Kind> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(Kind::Png);
    }
    if b.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some(Kind::Jpeg);
    }
    if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        return Some(Kind::Gif);
    }
    if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        return Some(Kind::Webp);
    }
    // ICONDIR: reserved 0, type 1, then at least one image.
    if b.len() >= 22 && b[..4] == [0, 0, 1, 0] && (b[4] != 0 || b[5] != 0) {
        return Some(Kind::Ico);
    }
    is_svg(b).then_some(Kind::Svg)
}

/// UTF-8 whose first element is `<svg`, after an optional BOM, XML
/// declaration, comments and an svg doctype without an internal subset.
fn is_svg(b: &[u8]) -> bool {
    let Ok(s) = std::str::from_utf8(b) else {
        return false;
    };
    let mut s = s.strip_prefix('\u{feff}').unwrap_or(s);
    loop {
        s = s.trim_start();
        let skip = |s: &str, open: &str, close: &str| -> Option<usize> {
            s.starts_with(open)
                .then(|| s.find(close).map(|i| i + close.len()))
                .flatten()
        };
        if s.starts_with("<?") {
            match skip(s, "<?", "?>") {
                Some(n) => s = &s[n..],
                None => return false,
            }
        } else if s.starts_with("<!--") {
            match skip(s, "<!--", "-->") {
                Some(n) => s = &s[n..],
                None => return false,
            }
        } else if s.len() >= 9 && s[..9].eq_ignore_ascii_case("<!doctype") {
            let name = s[9..].trim_start();
            let svg = name.len() > 3
                && name[..3].eq_ignore_ascii_case("svg")
                && name[3..].starts_with(|c: char| c.is_ascii_whitespace() || c == '>');
            match s.find('>') {
                Some(n) if svg && !s[..n].contains('[') => s = &s[n + 1..],
                _ => return false,
            }
        } else {
            break;
        }
    }
    s.strip_prefix("<svg")
        .and_then(|r| r.chars().next())
        .is_some_and(|c| c.is_ascii_whitespace() || c == '>' || c == '/')
}

/// A logo as served.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub kind: Kind,
    pub bytes: Vec<u8>,
}

impl Image {
    fn of(bytes: Vec<u8>) -> Result<Image, String> {
        if bytes.len() > MAX_BYTES {
            return Err(format!("over {} KiB", MAX_BYTES / 1024));
        }
        let kind = sniff(&bytes).ok_or("not a PNG, JPEG, GIF, WebP, ICO or SVG image")?;
        Ok(Image { kind, bytes })
    }

    pub fn response(&self) -> Response {
        Response::new(200)
            .header("Content-Type", self.kind.mime())
            .header("X-Content-Type-Options", "nosniff")
            .header("Cache-Control", "private, max-age=86400")
            .header("Cross-Origin-Resource-Policy", "same-origin")
            .header(
                "Content-Security-Policy",
                "default-src 'none'; style-src 'unsafe-inline'; sandbox",
            )
            .body(self.bytes.clone())
    }
}

/// Reads a logo URL; a test can replace it.
pub type FetchFn = Arc<dyn Fn(&str) -> Result<Vec<u8>, String> + Send + Sync>;

/// The logo cache of one daemon.
pub struct Logos {
    dir: PathBuf,
    fetch: FetchFn,
    /// One fetch per URL at a time.
    inflight: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

fn key(url: &str) -> String {
    ring::digest::digest(&ring::digest::SHA256, url.as_bytes())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn age(p: &Path) -> Option<Duration> {
    let m = std::fs::metadata(p).ok()?.modified().ok()?;
    Some(SystemTime::now().duration_since(m).unwrap_or_default())
}

impl Logos {
    pub fn new(state: &Path) -> Logos {
        Logos::with_fetch(state, Arc::new(https_get))
    }

    pub fn with_fetch(state: &Path, fetch: FetchFn) -> Logos {
        Logos {
            dir: state.join("templates").join("logos"),
            fetch,
            inflight: Mutex::new(HashMap::new()),
        }
    }

    /// The cached copy and its age, if it is still an image.
    fn cached(&self, k: &str) -> Option<(Duration, Image)> {
        let p = self.dir.join(k);
        let a = age(&p)?;
        let img = Image::of(std::fs::read(&p).ok()?).ok()?;
        Some((a, img))
    }

    /// The logo at `url`: the cached copy while it is fresh, else fetched
    /// again; a stale copy when that fails. `None` when there is nothing to
    /// serve (a failure is remembered for a few minutes).
    pub fn get(&self, url: &str) -> Option<Image> {
        let k = key(url);
        let failed = self.dir.join(format!("{k}.failed"));
        let lookup = || -> (Option<Image>, bool) {
            let cached = self.cached(&k);
            let fresh = cached.as_ref().is_some_and(|(a, _)| *a < FRESH_FOR)
                || age(&failed).is_some_and(|a| a < FAILED_FOR);
            (cached.map(|(_, i)| i), fresh)
        };
        if let (img, true) = lookup() {
            return img;
        }
        let lock = self
            .inflight
            .lock()
            .unwrap()
            .entry(k.clone())
            .or_default()
            .clone();
        let held = lock.lock().unwrap();
        // Another request may have fetched it while this one waited.
        let (stale, fresh) = lookup();
        let out = if fresh {
            stale
        } else {
            match (self.fetch)(url).and_then(Image::of) {
                Ok(img) => {
                    if let Err(e) = crate::app::write_atomic(&self.dir.join(&k), &img.bytes) {
                        eprintln!("isb serve: template logo cache: {e}");
                    }
                    let _ = std::fs::remove_file(&failed);
                    Some(img)
                }
                Err(e) => {
                    let host = net::parse_url(url).map(|t| t.host).unwrap_or_default();
                    eprintln!("isb serve: template logo from {host}: {e}");
                    if let Err(e) = crate::app::write_atomic(&failed, b"") {
                        eprintln!("isb serve: template logo cache: {e}");
                    }
                    stale
                }
            }
        };
        drop(held);
        self.inflight.lock().unwrap().remove(&k);
        out
    }
}

/// What one HTTP exchange came to.
#[derive(Debug, PartialEq, Eq)]
enum Answer {
    Body(Vec<u8>),
    Redirect(String),
}

/// GET an https URL under the SSRF policy: at most [`MAX_BYTES`], within
/// [`FETCH_TIMEOUT`], following up to [`MAX_REDIRECTS`] redirects that stay
/// on https.
pub fn https_get(url: &str) -> Result<Vec<u8>, String> {
    let started = Instant::now();
    let mut url = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        let t = net::parse_url(&url)?;
        if !t.https {
            return Err("logos are fetched over https only".into());
        }
        // The notification policy, never relaxed for logos.
        let policy = |e: net::SendError| {
            let m = e.message;
            m.split("; private targets are off")
                .next()
                .unwrap_or(&m)
                .to_string()
        };
        let tcp = net::connect(&t.host, t.port, false).map_err(policy)?;
        let mut s = net::tls(&net::Net::new(false), &t.host, tcp).map_err(|e| e.message)?;
        let host = if t.host.contains(':') {
            format!("[{}]", t.host)
        } else {
            t.host.clone()
        };
        let host = if t.port == 443 {
            host
        } else {
            format!("{host}:{}", t.port)
        };
        let head = format!(
            "GET {} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: isb/{}\r\nAccept: image/*\r\nAccept-Encoding: identity\r\nConnection: close\r\n\r\n",
            t.path,
            env!("CARGO_PKG_VERSION")
        );
        let io = |e: std::io::Error| format!("{}: {e}", t.host);
        s.write_all(head.as_bytes()).map_err(io)?;
        s.flush().map_err(io)?;
        let raw = read_capped(&mut s, started)?;
        match parse(&raw)? {
            Answer::Body(b) => return Ok(b),
            Answer::Redirect(loc) => url = redirect(&t, &loc)?,
        }
    }
    Err(format!("more than {MAX_REDIRECTS} redirects"))
}

/// Where a redirect from `from` to `location` goes: https only.
fn redirect(from: &net::Target, location: &str) -> Result<String, String> {
    let l = location.trim();
    let next = if l.starts_with("//") {
        format!("https:{l}")
    } else if l.starts_with('/') {
        let host = if from.host.contains(':') {
            format!("[{}]", from.host)
        } else {
            from.host.clone()
        };
        format!("https://{host}:{}{l}", from.port)
    } else {
        l.to_string()
    };
    if !next
        .get(..8)
        .is_some_and(|p| p.eq_ignore_ascii_case("https://"))
    {
        return Err("a redirect off https".into());
    }
    Ok(next)
}

/// The whole answer, refusing one past the cap.
fn read_capped<S: Read>(s: &mut S, started: Instant) -> Result<Vec<u8>, String> {
    // Room for the headers and chunk framing on top of the body.
    let cap = MAX_BYTES + 64 * 1024;
    let mut out = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        if started.elapsed() > FETCH_TIMEOUT {
            return Err(format!("no answer within {FETCH_TIMEOUT:?}"));
        }
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                out.extend_from_slice(&buf[..n]);
                if out.len() > cap {
                    return Err(format!("over {} KiB", MAX_BYTES / 1024));
                }
                if complete(&out) {
                    break;
                }
            }
            // A peer that closes without TLS close_notify, after answering.
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof && !out.is_empty() => break,
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(out)
}

fn header_end(b: &[u8]) -> Option<usize> {
    b.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

/// Headers and a Content-Length body are in (a server that holds the
/// connection open despite `Connection: close` does not hold us up).
fn complete(b: &[u8]) -> bool {
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

fn parse(raw: &[u8]) -> Result<Answer, String> {
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut r = httparse::Response::new(&mut headers);
    let end = match r.parse(raw) {
        Ok(httparse::Status::Complete(n)) => n,
        Ok(httparse::Status::Partial) => return Err("an incomplete HTTP answer".into()),
        Err(e) => return Err(format!("a bad HTTP answer: {e}")),
    };
    let status = r.code.unwrap_or(0);
    let header = |name: &str| {
        r.headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case(name))
            .and_then(|h| std::str::from_utf8(h.value).ok())
            .map(str::trim)
    };
    if matches!(status, 301 | 302 | 303 | 307 | 308) {
        return header("location")
            .map(|l| Answer::Redirect(l.to_string()))
            .ok_or_else(|| format!("HTTP {status} without a Location"));
    }
    if status != 200 {
        return Err(format!("HTTP {status}"));
    }
    if header("content-encoding").is_some_and(|e| !e.eq_ignore_ascii_case("identity")) {
        return Err("an encoded body".into());
    }
    let body = &raw[end..];
    let body = if header("transfer-encoding").is_some_and(|t| t.eq_ignore_ascii_case("chunked")) {
        dechunk(body)?
    } else {
        match header("content-length").and_then(|l| l.parse::<usize>().ok()) {
            Some(n) if n > MAX_BYTES => return Err(format!("over {} KiB", MAX_BYTES / 1024)),
            Some(n) if body.len() < n => return Err("a truncated body".into()),
            Some(n) => body[..n].to_vec(),
            None => body.to_vec(),
        }
    };
    if body.len() > MAX_BYTES {
        return Err(format!("over {} KiB", MAX_BYTES / 1024));
    }
    Ok(Answer::Body(body))
}

fn dechunk(mut b: &[u8]) -> Result<Vec<u8>, String> {
    let bad = || "a bad chunked body".to_string();
    let mut out = Vec::new();
    loop {
        let line = b.windows(2).position(|w| w == b"\r\n").ok_or_else(bad)?;
        let size = std::str::from_utf8(&b[..line]).map_err(|_| bad())?;
        let size = size.split(';').next().unwrap_or_default().trim();
        let n = usize::from_str_radix(size, 16).map_err(|_| bad())?;
        b = &b[line + 2..];
        if n == 0 {
            return Ok(out);
        }
        if n > MAX_BYTES || b.len() < n + 2 {
            return Err(bad());
        }
        out.extend_from_slice(&b[..n]);
        b = &b[n + 2..];
    }
}

/// May this request see template logos? The same callers as
/// `template_list`.
pub type Admit = Arc<dyn Fn(&Request) -> Result<(), Response> + Send + Sync>;

/// Admit who `template_list` admits: the caller as the tool endpoints
/// resolve it, then the same authorizer.
pub fn admit(authn: Authn, access: Option<Arc<AccessValidator>>, allow_anonymous: bool) -> Admit {
    Arc::new(move |req: &Request| {
        let deny = |status: u16, m: &str| {
            Response::json(
                status,
                &serde_json::json!({"error": "forbidden", "message": m}),
            )
        };
        // The listener already checked an assertion when Access guards it;
        // one sent anyway (a tailnet listener) must still be valid.
        let id = match (&access, req.header(ASSERTION_HEADER)) {
            (Some(v), Some(t)) => Some(
                v.validate(t.trim())
                    .map_err(|_| deny(401, "invalid Cloudflare Access assertion"))?,
            ),
            _ => None,
        };
        let caller = match authn(req, id.as_ref()) {
            Authenticated::User(p) => Caller::User { principal: p },
            Authenticated::Superadmin(s) => {
                if s.source.is_ambient() {
                    crate::server::mcp::ambient_ok(req).map_err(|why| deny(403, why))?;
                }
                Caller::Superadmin(s)
            }
            Authenticated::Refused => return Err(deny(401, "invalid credentials")),
            Authenticated::None => match (id, &req.peer) {
                (Some(id), _) => Caller::Access(id),
                (None, Peer::Unix { uid }) => Caller::Local { uid: *uid },
                (None, Peer::Tcp(addr)) => Caller::Unauthenticated { addr: *addr },
            },
        };
        let read = super::super::audit::Class {
            read_only: true,
            secret_read: false,
        };
        super::super::authorize_class(
            &caller,
            "template_list",
            read,
            serde_json::json!({}),
            None,
            allow_anonymous,
        )
        .map(|_| ())
        .map_err(|e| {
            let status = if matches!(caller, Caller::Unauthenticated { .. }) {
                401
            } else {
                403
            };
            deny(status, &e.to_string())
        })
    })
}

/// Template refs to logo URLs, rebuilt now and then.
type Index = Mutex<Option<(Instant, Arc<HashMap<String, String>>)>>;

fn logo_of(catalogs: &Catalogs, index: &Index, reference: &str) -> Option<String> {
    let mut g = index.lock().unwrap();
    let stale = g.as_ref().is_none_or(|(at, m)| {
        at.elapsed() >= INDEX_FOR
            || (at.elapsed() >= INDEX_MISS_AFTER && !m.contains_key(reference))
    });
    if stale {
        let (all, _) = catalogs.list();
        let m = all
            .into_iter()
            .filter_map(|s| Some((s.reference, s.logo?)))
            .collect();
        *g = Some((Instant::now(), Arc::new(m)));
    }
    g.as_ref()?.1.get(reference).cloned()
}

/// `GET /api/v1/templates/<catalog>/<id>/logo`: the template's logo from
/// isb's cache, or a 404 when it has none or it could not be fetched.
pub fn route(catalogs: Arc<Catalogs>, logos: Arc<Logos>, admit: Admit) -> crate::server::Routes {
    let index: Arc<Index> = Arc::new(Mutex::new(None));
    Arc::new(move |req: &Request| {
        let reference = req
            .path
            .strip_prefix("/api/v1/templates/")?
            .strip_suffix("/logo")?;
        let (catalog, id) = reference.split_once('/')?;
        if catalog.is_empty() || id.is_empty() || id.contains('/') {
            return None;
        }
        if !matches!(req.method.as_str(), "GET" | "HEAD") {
            return Some(Response::text(405, "method not allowed").header("Allow", "GET, HEAD"));
        }
        if let Err(r) = admit(req) {
            return Some(r);
        }
        let none = || Response::text(404, "no logo").header("Cache-Control", "no-store");
        let Some(url) = logo_of(&catalogs, &index, reference) else {
            return Some(none());
        };
        let mut r = match logos.get(&url) {
            Some(img) => img.response(),
            None => none(),
        };
        if req.method == "HEAD" {
            r.body.clear();
        }
        Some(r)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
    const SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1 1"/>"#;

    fn ico() -> Vec<u8> {
        let mut b = vec![0, 0, 1, 0, 1, 0];
        b.extend_from_slice(&[0; 16]);
        b
    }

    #[test]
    fn sniffs_image_types_only() {
        assert_eq!(sniff(PNG), Some(Kind::Png));
        assert_eq!(sniff(b"\xff\xd8\xff\xe0\0\x10JFIF"), Some(Kind::Jpeg));
        assert_eq!(sniff(b"GIF89a\x01\0"), Some(Kind::Gif));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some(Kind::Webp));
        assert_eq!(sniff(&ico()), Some(Kind::Ico));
        assert_eq!(sniff(SVG), Some(Kind::Svg));
        assert_eq!(
            sniff(
                b"\xef\xbb\xbf<?xml version=\"1.0\"?>\n<!-- hi -->\n<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \"x.dtd\">\n<svg width=\"1\">"
            ),
            Some(Kind::Svg)
        );
        for not in [
            &b"<html><svg></svg></html>"[..],
            b"<!doctype html><svg>",
            b"<svgx>",
            b"<!DOCTYPE svg [<!ENTITY x \"y\">]><svg>",
            b"<!-- unclosed <svg>",
            b"{\"not\": \"an image\"}",
            b"RIFF\0\0\0\0WAVE",
            b"\0\0\x01\0\0\0",
            b"",
            b"\xff\xfe<\0s\0v\0g\0",
        ] {
            assert_eq!(sniff(not), None, "{:?}", String::from_utf8_lossy(not));
        }
    }

    #[test]
    fn caps_the_size() {
        let mut big = PNG.to_vec();
        big.resize(MAX_BYTES, 0);
        assert!(Image::of(big.clone()).is_ok());
        big.push(0);
        assert!(Image::of(big).unwrap_err().contains("512 KiB"));
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
            MAX_BYTES + 1
        );
        assert!(parse(head.as_bytes()).unwrap_err().contains("512 KiB"));
        let mut s = std::io::Cursor::new(vec![b'x'; MAX_BYTES + 128 * 1024]);
        assert!(read_capped(&mut s, Instant::now()).is_err());
    }

    #[test]
    fn https_and_public_addresses_only() {
        for (url, why) in [
            ("http://example.com/logo.png", "https only"),
            ("ftp://example.com/logo.png", "http"),
            ("https://127.0.0.1/logo.png", "loopback"),
            ("https://localhost./logo.png", ""),
            ("https://10.1.2.3/logo.png", "private"),
            ("https://169.254.169.254/latest/meta-data", "link-local"),
            ("https://[::1]/logo.png", "loopback"),
            ("https://[fd00::1]/logo.png", "unique local"),
            ("https://0x7f000001/logo.png", "loopback"),
            ("https://user@example.com/logo.png", "credentials"),
        ] {
            let e = https_get(url).unwrap_err();
            assert!(e.contains(why), "{url}: {e}");
        }
        let from = net::parse_url("https://cdn.example.com/a/b.png").unwrap();
        assert_eq!(
            redirect(&from, "/c.png").unwrap(),
            "https://cdn.example.com:443/c.png"
        );
        assert_eq!(
            redirect(&from, "//other.example/x.svg").unwrap(),
            "https://other.example/x.svg"
        );
        assert!(redirect(&from, "http://cdn.example.com/c.png").is_err());
        assert!(redirect(&from, "c.png").is_err());
    }

    #[test]
    fn parses_answers() {
        let ok = b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nabcdef";
        assert_eq!(parse(ok).unwrap(), Answer::Body(b"abc".to_vec()));
        let chunked =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2;x=y\r\nde\r\n0\r\n\r\n";
        assert_eq!(parse(chunked).unwrap(), Answer::Body(b"abcde".to_vec()));
        let moved = b"HTTP/1.1 302 Found\r\nLocation: /x.png\r\n\r\n";
        assert_eq!(parse(moved).unwrap(), Answer::Redirect("/x.png".into()));
        assert!(parse(b"HTTP/1.1 404 Not Found\r\n\r\n").is_err());
        assert!(parse(b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\n\r\nxx").is_err());
        assert!(parse(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nabc").is_err());
        assert!(dechunk(b"zz\r\n").is_err());
        assert!(complete(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nab"));
        assert!(!complete(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\na"));
    }

    fn set_age(p: &Path, d: Duration) {
        let f = std::fs::File::options().write(true).open(p).unwrap();
        f.set_modified(SystemTime::now() - d).unwrap();
    }

    /// A cache whose fetches are counted and answered from `answer`.
    fn logos(dir: &Path, answer: Arc<Mutex<Result<Vec<u8>, String>>>) -> (Logos, Arc<AtomicUsize>) {
        let n = Arc::new(AtomicUsize::new(0));
        let n2 = n.clone();
        let l = Logos::with_fetch(
            dir,
            Arc::new(move |_u: &str| {
                n2.fetch_add(1, Ordering::SeqCst);
                answer.lock().unwrap().clone()
            }),
        );
        (l, n)
    }

    #[test]
    fn caches_and_expires() {
        let dir = tempfile::tempdir().unwrap();
        let answer = Arc::new(Mutex::new(Ok(PNG.to_vec())));
        let (l, n) = logos(dir.path(), answer.clone());
        let url = "https://cdn.example.com/logo.png";
        assert_eq!(l.get(url).unwrap().kind, Kind::Png);
        assert_eq!(l.get(url).unwrap().bytes, PNG);
        assert_eq!(n.load(Ordering::SeqCst), 1, "the second read is a hit");
        let file = dir.path().join("templates/logos").join(key(url));
        assert_eq!(std::fs::read(&file).unwrap(), PNG);
        // A week on, it is fetched again.
        set_age(&file, FRESH_FOR + Duration::from_secs(1));
        *answer.lock().unwrap() = Ok(SVG.to_vec());
        assert_eq!(l.get(url).unwrap().kind, Kind::Svg);
        assert_eq!(n.load(Ordering::SeqCst), 2);
        // A failed refetch serves the stale copy, and is not retried at once.
        set_age(&file, FRESH_FOR + Duration::from_secs(1));
        *answer.lock().unwrap() = Err("HTTP 500".into());
        assert_eq!(l.get(url).unwrap().kind, Kind::Svg);
        assert_eq!(l.get(url).unwrap().kind, Kind::Svg);
        assert_eq!(n.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn remembers_failures_briefly() {
        let dir = tempfile::tempdir().unwrap();
        let answer = Arc::new(Mutex::new(Ok(b"<html>not a logo</html>".to_vec())));
        let (l, n) = logos(dir.path(), answer.clone());
        let url = "https://cdn.example.com/x";
        assert!(l.get(url).is_none());
        assert!(l.get(url).is_none());
        assert_eq!(n.load(Ordering::SeqCst), 1);
        assert!(!dir.path().join("templates/logos").join(key(url)).exists());
        let failed = dir
            .path()
            .join("templates/logos")
            .join(format!("{}.failed", key(url)));
        set_age(&failed, FAILED_FOR + Duration::from_secs(1));
        *answer.lock().unwrap() = Ok(ico());
        assert_eq!(l.get(url).unwrap().kind, Kind::Ico);
        assert_eq!(n.load(Ordering::SeqCst), 2);
        assert!(!failed.exists());
    }

    #[test]
    fn serves_with_strict_headers() {
        let r = Image::of(SVG.to_vec()).unwrap().response();
        assert_eq!(r.status, 200);
        assert_eq!(r.get_header("content-type"), Some("image/svg+xml"));
        assert_eq!(r.get_header("x-content-type-options"), Some("nosniff"));
        assert_eq!(
            r.get_header("cache-control"),
            Some("private, max-age=86400")
        );
        assert_eq!(
            r.get_header("content-security-policy"),
            Some("default-src 'none'; style-src 'unsafe-inline'; sandbox")
        );
        let r = Image::of(PNG.to_vec()).unwrap().response();
        assert_eq!(r.get_header("content-type"), Some("image/png"));
    }

    fn req(method: &str, path: &str) -> Request {
        Request {
            method: method.into(),
            path: path.into(),
            query: None,
            headers: vec![],
            body: vec![],
            peer: Peer::Tcp("127.0.0.1:5000".parse().unwrap()),
        }
    }

    #[test]
    fn route_serves_catalog_logos_to_admitted_callers() {
        let dir = tempfile::tempdir().unwrap();
        let catalogs = Arc::new(Catalogs::with_fetch(
            dir.path(),
            Arc::new(|_: &str| Err(crate::error::Error::invalid("offline"))),
        ));
        let fetched = Arc::new(Mutex::new(Vec::<String>::new()));
        let f2 = fetched.clone();
        let logos = Arc::new(Logos::with_fetch(
            dir.path(),
            Arc::new(move |u: &str| {
                f2.lock().unwrap().push(u.to_string());
                Ok(SVG.to_vec())
            }),
        ));
        let open: Admit = Arc::new(|_| Ok(()));
        let r = route(catalogs.clone(), logos.clone(), open);
        let get = |m: &str, p: &str| r(&req(m, p));
        let ok = get("GET", "/api/v1/templates/builtin/gitea/logo").unwrap();
        assert_eq!(ok.status, 200);
        assert_eq!(ok.get_header("content-type"), Some("image/svg+xml"));
        assert_eq!(ok.body, SVG);
        let gitea = catalogs.get("builtin/gitea").unwrap().summary.logo.unwrap();
        assert_eq!(fetched.lock().unwrap().as_slice(), [gitea]);
        let head = get("HEAD", "/api/v1/templates/builtin/gitea/logo").unwrap();
        assert_eq!((head.status, head.body.len()), (200, 0));
        // whoami has no logo; nothing is fetched for it or an unknown ref.
        assert_eq!(
            get("GET", "/api/v1/templates/builtin/whoami/logo")
                .unwrap()
                .status,
            404
        );
        assert_eq!(
            get("GET", "/api/v1/templates/builtin/nope/logo")
                .unwrap()
                .status,
            404
        );
        assert_eq!(fetched.lock().unwrap().len(), 1);
        assert_eq!(
            get("POST", "/api/v1/templates/builtin/gitea/logo")
                .unwrap()
                .status,
            405
        );
        for other in [
            "/api/v1/templates/gitea/logo",
            "/api/v1/templates/builtin/a/b/logo",
            "/api/v1/templates/builtin/gitea",
            "/api/v1/tools/template_list",
        ] {
            assert!(get("GET", other).is_none(), "{other}");
        }
        // Every built-in logo is an https URL.
        for s in catalogs.list().0 {
            if let Some(l) = s.logo {
                assert!(l.starts_with("https://"), "{}: {l}", s.reference);
            }
        }
        let shut: Admit = Arc::new(|_| Err(Response::text(401, "sign in")));
        let r = route(catalogs, logos, shut);
        assert_eq!(
            r(&req("GET", "/api/v1/templates/builtin/gitea/logo"))
                .unwrap()
                .status,
            401
        );
    }

    #[test]
    fn admits_whom_template_list_admits() {
        let none: Authn = Arc::new(|_, _| Authenticated::None);
        let refused: Authn = Arc::new(|_, _| Authenticated::Refused);
        let r = req("GET", "/api/v1/templates/builtin/gitea/logo");
        assert_eq!(
            admit(none.clone(), None, false)(&r).unwrap_err().status,
            401
        );
        assert!(admit(none, None, true)(&r).is_ok());
        assert_eq!(admit(refused, None, true)(&r).unwrap_err().status, 401);
    }
}
