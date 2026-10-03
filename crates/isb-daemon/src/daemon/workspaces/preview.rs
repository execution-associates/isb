//! Previews served by isb itself (docs/concepts/workspaces.md#previews-through-isb):
//! a published workspace port at a URL of isb's, behind isb's sign-in, for
//! hosts with no domain or ingress.
//!
//! The security design:
//!
//! - **An origin of its own per preview.** A preview runs whatever the
//!   workspace serves, so it never shares isb's origin: on isb's origin its
//!   scripts could call isb's API with the viewer's session (a same-origin
//!   request carries the cookie and may set `X-Isb-Csrf`). Each port gets the
//!   host `<port>-<workspace>-<org>` under the preview domain: `--preview-domain`,
//!   else, when isb is reached on loopback, `localhost`, whose subdomains
//!   browsers resolve to loopback on their own. Previews of different ports,
//!   workspaces and orgs are then different origins from each other and from
//!   isb, and `x.localhost` is not even the same site as `localhost` or
//!   `127.0.0.1`, so a preview cannot set cookies for isb either.
//! - **isb's session never reaches it.** The session cookie is host-only, so
//!   the browser does not send it to a preview host; the proxy also drops it,
//!   and Cloudflare Access' assertion, before anything reaches the app.
//! - **A one-time link, then a cookie for that preview alone.** The Ports
//!   tab asks `workspace_port_open` (members and up, audited) for a link
//!   carrying a random token, good once and for 60 seconds, bound to that
//!   preview's host and the caller. Redeeming it sets `isb_preview`:
//!   random, HttpOnly, `SameSite=Strict`, host-only, 8 hours, and answers
//!   with a page that refreshes to the app, so the token never reaches the
//!   app or a `Referer`. Every request then needs that cookie; the caller's
//!   membership is checked again every minute, and a port that is no longer
//!   published stops answering at once.
//! - **Nothing of isb's UI applies.** Preview hosts are matched by `Host`
//!   ahead of everything else on the listener, so isb's pages, API and
//!   headers (its Content-Security-Policy among them) are never served there.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};

use super::proxy::{self, COOKIE};
use super::*;
use crate::server::http::{Request, Response};

/// The path on a preview host that redeems a one-time link.
pub(super) const OPEN_PATH: &str = "/__isb_preview";
const TOKEN_TTL: u64 = 60;
const SESSION_TTL: u64 = 8 * 3600;
const RECHECK: u64 = 60;

/// Where a preview host's requests go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Target {
    pub org: OrgId,
    pub ws: String,
    pub port: u16,
    pub ip: Option<IpAddr>,
}

/// Who opened a preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Who {
    /// A signed-in user, by id: their membership is checked again.
    User(i64),
    /// The unix socket or a superadmin: trusted until the session ends.
    Superadmin(String),
}

#[derive(Debug, Clone)]
struct Grant {
    label: String,
    who: Who,
    expires: u64,
    checked: u64,
}

/// `--preview-domain`: `[http(s)://]DOMAIN[:PORT]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewBase {
    scheme: String,
    domain: String,
    port: Option<u16>,
}

impl PreviewBase {
    pub fn parse(s: &str) -> Result<PreviewBase> {
        let s = s.trim().trim_end_matches('/').to_ascii_lowercase();
        let (scheme, rest) = match s.split_once("://") {
            Some((sc @ ("http" | "https"), r)) => (sc.to_string(), r.to_string()),
            Some(_) => return Err(Error::invalid("--preview-domain: http:// or https:// only")),
            None => ("https".to_string(), s.clone()),
        };
        let (domain, port) = match rest.rsplit_once(':') {
            Some((d, p)) => (
                d.to_string(),
                Some(
                    p.parse::<u16>()
                        .map_err(|_| Error::invalid("--preview-domain: bad port"))?,
                ),
            ),
            None => (rest, None),
        };
        let ok = domain == "localhost"
            || crate::ingress::domain::check_host(&domain).is_ok() && !domain.starts_with("*.");
        if !ok || domain.contains('/') {
            return Err(Error::invalid(format!(
                "--preview-domain {s:?}: a domain whose subdomains reach this listener, e.g. preview.example.com"
            )));
        }
        Ok(PreviewBase {
            scheme,
            domain,
            port,
        })
    }

    pub(super) fn url(&self, label: &str) -> String {
        match self.port {
            Some(p) => format!("{}://{label}.{}:{p}", self.scheme, self.domain),
            None => format!("{}://{label}.{}", self.scheme, self.domain),
        }
    }
}

/// The preview host's first label for a port: `<port>-<workspace>-<org>`,
/// or `<port>-<hash>` when that is longer than a DNS label.
pub(super) fn label(org: &OrgId, ws: &str, port: u16) -> String {
    let l = format!("{port}-{ws}-{org}");
    if l.len() <= 63 {
        return l;
    }
    let mut h: u32 = 0x811c9dc5;
    for b in format!("{org}/{ws}").bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x01000193);
    }
    format!("{port}-{h:08x}")
}

/// The preview label a `Host` names: one label under the preview domain,
/// or under `localhost`. `None` for every other host (isb's own).
pub(super) fn label_of_host(host: &str, base: Option<&PreviewBase>) -> Option<String> {
    let host = host.trim().to_ascii_lowercase();
    let name = match host.rsplit_once(':') {
        Some((n, p)) if p.bytes().all(|b| b.is_ascii_digit()) && !n.ends_with(']') => n,
        _ => host.as_str(),
    };
    let first = base
        .and_then(|b| name.strip_suffix(&format!(".{}", b.domain)))
        .or_else(|| name.strip_suffix(".localhost"))?;
    let ok = !first.is_empty()
        && first.len() <= 63
        && first
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && first.starts_with(|c: char| c.is_ascii_digit());
    ok.then(|| first.to_string())
}

/// The base URL a preview gets: the preview domain's, else, when the
/// caller's own isb URL (`origin`) is on loopback, `localhost`'s.
pub(super) fn base_for(base: Option<&PreviewBase>, origin: Option<&str>) -> Result<PreviewBase> {
    if let Some(b) = base {
        return Ok(b.clone());
    }
    let none = || {
        Error::invalid(
            "previews through isb need their own origin: run isb serve with --preview-domain DOMAIN (a wildcard name that reaches this listener), or reach isb on localhost (docs/concepts/workspaces.md#previews-through-isb)",
        )
    };
    let o = origin.ok_or_else(none)?.trim().trim_end_matches('/');
    let (scheme, rest) = o.split_once("://").ok_or_else(none)?;
    let (host, port) = match rest.rsplit_once(':') {
        Some((h, p)) if !h.is_empty() && !p.contains(']') => (h, p.parse::<u16>().ok()),
        _ => (rest, None),
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback());
    if !loopback || !matches!(scheme, "http" | "https") {
        return Err(none());
    }
    Ok(PreviewBase {
        scheme: scheme.to_string(),
        domain: "localhost".into(),
        port,
    })
}

fn random_token() -> String {
    use ring::rand::SecureRandom;
    let mut b = [0u8; 32];
    let _ = ring::rand::SystemRandom::new().fill(&mut b);
    hex(&b)
}

/// The previews: where each host goes, open links, sessions.
#[derive(Default)]
pub struct Previews {
    base: OnceLock<Option<PreviewBase>>,
    index: Mutex<HashMap<String, Target>>,
    tokens: Mutex<HashMap<String, Grant>>,
    sessions: Mutex<HashMap<String, Grant>>,
}

fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Previews {
    /// `--preview-domain`, once at startup.
    pub fn set_base(&self, b: Option<PreviewBase>) {
        let _ = self.base.set(b);
    }

    pub(super) fn base(&self) -> Option<&PreviewBase> {
        self.base.get().and_then(Option::as_ref)
    }

    pub(super) fn set_index(&self, index: HashMap<String, Target>) {
        *locked(&self.index) = index;
    }

    pub(super) fn target(&self, label: &str) -> Option<Target> {
        locked(&self.index).get(label).cloned()
    }

    /// A one-time link token for `label`.
    pub(super) fn mint(&self, label: &str, who: Who, now: u64) -> String {
        let t = random_token();
        let mut m = locked(&self.tokens);
        m.retain(|_, g| g.expires > now);
        m.insert(
            t.clone(),
            Grant {
                label: label.into(),
                who,
                expires: now + TOKEN_TTL,
                checked: now,
            },
        );
        t
    }

    /// Spend a link token on `label`'s host: a new session id, once.
    pub(super) fn redeem(&self, token: &str, label: &str, now: u64) -> Option<String> {
        let g = locked(&self.tokens).remove(token)?;
        if g.expires <= now || g.label != label {
            return None;
        }
        let sid = random_token();
        let mut s = locked(&self.sessions);
        s.retain(|_, g| g.expires > now);
        s.insert(
            sid.clone(),
            Grant {
                expires: now + SESSION_TTL,
                ..g
            },
        );
        Some(sid)
    }

    /// The session `sid` on `label`'s host, if it is one, with whether its
    /// caller is due to be checked again.
    fn session(&self, sid: &str, label: &str, now: u64) -> Option<(Who, bool)> {
        let s = locked(&self.sessions);
        let g = s.get(sid).filter(|g| g.label == label && g.expires > now)?;
        Some((g.who.clone(), now >= g.checked + RECHECK))
    }

    fn checked(&self, sid: &str, now: u64) {
        if let Some(g) = locked(&self.sessions).get_mut(sid) {
            g.checked = now;
        }
    }

    fn end(&self, sid: &str) {
        locked(&self.sessions).remove(sid);
    }
}

/// Whether `who` may still reach `org`'s workspace ports: an enabled user
/// who is a platform admin or the org's member (or above).
fn still_allowed(d: &Daemon, org: &OrgId, who: &Who) -> bool {
    let Who::User(id) = who else { return true };
    let Ok(u) = d.users.user(*id) else {
        return false;
    };
    !u.disabled
        && (u.platform_admin
            || d.users.memberships(*id).is_ok_and(|ms| {
                ms.iter()
                    .any(|m| &m.org == org && m.role >= crate::auth::Role::Member)
            }))
}

/// A page of the preview's own (no isb UI): plain, no scripts, no referrer.
fn page(status: u16, msg: &str) -> Response {
    let esc = msg
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    Response::new(status)
        .header("Content-Type", "text/html; charset=utf-8")
        .header("Content-Security-Policy", "default-src 'none'")
        .header("Referrer-Policy", "no-referrer")
        .header("Cache-Control", "no-store")
        .header("X-Frame-Options", "DENY")
        .body(format!(
            "<!doctype html><title>isb preview</title><p>{esc}</p>\n"
        ))
}

/// A query parameter, percent-decoded.
fn query_param(q: Option<&str>, key: &str) -> Option<String> {
    let v = q?.split('&').find_map(|kv| {
        kv.split_once('=')
            .filter(|(k, _)| *k == key)
            .map(|(_, v)| v)
    })?;
    let b = v.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let h = std::str::from_utf8(&b[i + 1..i + 3]).ok()?;
                out.push(u8::from_str_radix(h, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// Where to go after the link is spent: a path on the preview's own host.
pub(super) fn safe_next(next: Option<&str>) -> String {
    match next {
        Some(n)
            if n.starts_with('/')
                && !n.starts_with("//")
                && n.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"/-._~?=&%:+,#".contains(&b)) =>
        {
            n.to_string()
        }
        _ => "/".into(),
    }
}

fn cookie_value(r: &Request, name: &str) -> Option<String> {
    r.headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("cookie"))
        .flat_map(|(_, v)| v.split(';'))
        .find_map(|p| {
            let (k, v) = p.trim().split_once('=')?;
            (k == name).then(|| v.to_string())
        })
}

/// Spend a link: the session cookie, and a page that refreshes to the app.
fn open(p: &Previews, r: &Request, label: &str) -> Response {
    let token = query_param(r.query.as_deref(), "token");
    let Some(sid) = token.and_then(|t| p.redeem(&t, label, now())) else {
        return page(
            403,
            "This preview link was used already or has expired. Open the port again from the workspace's Ports tab.",
        );
    };
    let next = safe_next(query_param(r.query.as_deref(), "next").as_deref());
    let secure = r
        .header("x-forwarded-proto")
        .is_some_and(|v| v.eq_ignore_ascii_case("https"));
    page(200, "Opening the preview...")
        .header("Refresh", format!("0;url={next}"))
        .header(
            "Set-Cookie",
            format!(
                "{COOKIE}={sid}; Path=/; HttpOnly; SameSite=Strict; Max-Age={SESSION_TTL}{}",
                if secure { "; Secure" } else { "" }
            ),
        )
}

/// One request on a preview host.
fn handle(d: &Daemon, r: &Request, label: &str) -> Response {
    let p = &d.workspaces.previews;
    if r.path == OPEN_PATH {
        return open(p, r, label);
    }
    let sid = cookie_value(r, COOKIE);
    let Some((who, due)) = sid.as_deref().and_then(|s| p.session(s, label, now())) else {
        return page(
            401,
            "Open this preview from the workspace's Ports tab in isb: it needs a link from there.",
        );
    };
    let Some(t) = p.target(label) else {
        return page(404, "This port is no longer published.");
    };
    let sid = sid.unwrap_or_default();
    if due {
        if !still_allowed(d, &t.org, &who) {
            p.end(&sid);
            return page(403, "You no longer have access to this workspace.");
        }
        p.checked(&sid, now());
    }
    let Some(ip) = t.ip else {
        return page(502, &format!("Workspace {} is not running.", t.ws));
    };
    match proxy::forward(r, SocketAddr::new(ip, t.port)) {
        Ok(resp) => resp,
        Err(e) => page(
            502,
            &format!(
                "Port {} of workspace {}: {e}. Is the server running, listening on 0.0.0.0 (not 127.0.0.1)?",
                t.port, t.ws
            ),
        ),
    }
}

/// The listener's preview routes: requests for a preview host, by `Host`.
pub(in crate::daemon) fn route(d: Arc<Daemon>) -> crate::server::Routes {
    Arc::new(move |r: &Request| {
        let label = label_of_host(r.header("host")?, d.workspaces.previews.base())?;
        Some(handle(&d, r, &label))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::http::Peer;

    #[test]
    fn each_port_has_a_host_of_its_own() {
        let acme = OrgId::new("acme").unwrap();
        assert_eq!(label(&acme, "workspace", 3000), "3000-workspace-acme");
        let long = OrgId::new("a".repeat(31)).unwrap();
        let l = label(&long, &"w".repeat(30), 3000);
        assert!(l.len() <= 63 && l.starts_with("3000-"), "{l}");
        let b = PreviewBase::parse("preview.example.com").unwrap();
        assert_eq!(
            b.url("3000-workspace-acme"),
            "https://3000-workspace-acme.preview.example.com"
        );
        assert_eq!(
            label_of_host("3000-workspace-acme.preview.example.com", Some(&b)).as_deref(),
            Some("3000-workspace-acme")
        );
        assert_eq!(
            label_of_host("3000-workspace-acme.localhost:8192", None).as_deref(),
            Some("3000-workspace-acme")
        );
        // isb's own hosts, and anything deeper or odd, are not previews.
        for h in [
            "localhost:8192",
            "127.0.0.1:8192",
            "isb.example.com",
            "preview.example.com",
            "a.b.preview.example.com",
            "web.localhost",
            "3000-x.evil.com",
            ".localhost",
        ] {
            assert_eq!(label_of_host(h, Some(&b)), None, "{h}");
        }
        assert!(PreviewBase::parse("ftp://x.example.com").is_err());
        assert!(PreviewBase::parse("*.example.com").is_err());
        let local = PreviewBase::parse("http://localhost:8192").unwrap();
        assert_eq!(local.url("1-w-a"), "http://1-w-a.localhost:8192");
    }

    #[test]
    fn without_a_preview_domain_only_a_loopback_isb_gets_previews() {
        let b = base_for(None, Some("http://127.0.0.1:8192")).unwrap();
        assert_eq!(
            b.url("3000-workspace-acme"),
            "http://3000-workspace-acme.localhost:8192"
        );
        let b = base_for(None, Some("http://localhost:9000/")).unwrap();
        assert_eq!(b.url("x"), "http://x.localhost:9000");
        assert!(base_for(None, Some("http://[::1]:8192")).is_ok());
        for o in [
            None,
            Some("https://isb.example.com"),
            Some("http://10.0.0.5:8192"),
            Some("javascript:alert(1)"),
        ] {
            assert!(base_for(None, o).is_err(), "{o:?}");
        }
        let set = PreviewBase::parse("https://preview.example.com").unwrap();
        assert_eq!(
            base_for(Some(&set), Some("https://isb.example.com")).unwrap(),
            set
        );
    }

    #[test]
    fn a_link_is_spent_once_on_its_own_host_and_expires() {
        let p = Previews::default();
        let t = p.mint("3000-workspace-acme", Who::User(7), 1000);
        assert_eq!(t.len(), 64);
        // Another preview's host cannot spend it, and that burns it.
        assert!(p.redeem(&t, "3000-workspace-beta", 1001).is_none());
        assert!(p.redeem(&t, "3000-workspace-acme", 1001).is_none());
        let t = p.mint("3000-workspace-acme", Who::User(7), 1000);
        assert!(
            p.redeem(&t, "3000-workspace-acme", 1000 + TOKEN_TTL)
                .is_none(),
            "expired"
        );
        let t = p.mint("3000-workspace-acme", Who::User(7), 1000);
        let sid = p.redeem(&t, "3000-workspace-acme", 1010).unwrap();
        assert!(
            p.redeem(&t, "3000-workspace-acme", 1011).is_none(),
            "used once"
        );
        // The session works on its host only, and is re-checked after a minute.
        assert_eq!(
            p.session(&sid, "3000-workspace-acme", 1020),
            Some((Who::User(7), false))
        );
        assert_eq!(
            p.session(&sid, "3000-workspace-acme", 1000 + RECHECK),
            Some((Who::User(7), true))
        );
        assert_eq!(p.session(&sid, "5173-workspace-acme", 1020), None);
        assert_eq!(
            p.session(&sid, "3000-workspace-acme", 1010 + SESSION_TTL),
            None
        );
        p.end(&sid);
        assert_eq!(p.session(&sid, "3000-workspace-acme", 1020), None);
    }

    #[test]
    fn the_link_page_sets_a_strict_host_only_cookie_and_leaks_no_token() {
        let p = Previews::default();
        let t = p.mint(
            "3000-workspace-acme",
            Who::Superadmin("local".into()),
            now(),
        );
        let r = Request {
            method: "GET".into(),
            path: OPEN_PATH.into(),
            query: Some(format!("token={t}&next=%2Fapp%3Fx%3D1")),
            headers: vec![("Host".into(), "3000-workspace-acme.localhost:8192".into())],
            body: vec![],
            peer: Peer::Tcp("127.0.0.1:1".parse().unwrap()),
        };
        let resp = open(&p, &r, "3000-workspace-acme");
        assert_eq!(resp.status, 200);
        let c = resp.get_header("set-cookie").unwrap();
        assert!(c.starts_with("isb_preview="), "{c}");
        for want in ["HttpOnly", "SameSite=Strict", "Path=/"] {
            assert!(c.contains(want), "{c}");
        }
        assert!(!c.to_ascii_lowercase().contains("domain="), "{c}");
        assert_eq!(resp.get_header("refresh"), Some("0;url=/app?x=1"));
        assert_eq!(resp.get_header("referrer-policy"), Some("no-referrer"));
        assert_eq!(
            resp.get_header("content-security-policy"),
            Some("default-src 'none'")
        );
        assert!(!String::from_utf8_lossy(&resp.body).contains(&t));
        // Spent: a second visit is refused.
        assert_eq!(open(&p, &r, "3000-workspace-acme").status, 403);
        for bad in [
            "//evil.com",
            "https://evil.com",
            "/a\"onload",
            "javascript:x",
        ] {
            assert_eq!(safe_next(Some(bad)), "/", "{bad}");
        }
    }
}
