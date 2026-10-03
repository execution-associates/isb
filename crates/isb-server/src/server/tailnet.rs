//! Tailnet identity, for `isb serve --superadmin-tailnet`: who is at the
//! other end of a TCP connection from a tailnet address, asked of the local
//! tailscaled.
//!
//! - Only the real socket peer counts. Forwarded headers
//!   (`X-Forwarded-For`, `Tailscale-User-Login`) are never read: anything on
//!   the path could write them.
//! - The peer must be a tailnet address (100.64.0.0/10, fd7a:115c:a1e0::/48)
//!   and the request's `Host` one of this server's names (its tailnet
//!   listen addresses, its MagicDNS names, the public URL's host), which
//!   blocks DNS rebinding: a page on another site that resolves its name to
//!   this address still sends its own `Host`.
//! - tailscaled answers `whois` through its LocalAPI (the unix socket on
//!   Linux), else the `tailscale whois --json` CLI (macOS). When neither
//!   answers, nobody is a tailnet superadmin; why is logged once.
//! - A tagged node is its tags (its login is `tagged-devices`); any other
//!   node is its user's login name. The allow list names either.
//! - Answers are cached per peer address for a minute, failures for five
//!   seconds.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::http::{Peer, Request};
use crate::error::{Error, Result};

/// Who tailscaled says is behind an address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Whois {
    /// The user's login name (`someone@example.com`; `tagged-devices` for
    /// a tagged node).
    pub login: String,
    /// The node's MagicDNS name, without the trailing dot.
    pub node: String,
    pub tags: Vec<String>,
}

/// Asks tailscaled about one peer. Injectable, for tests.
pub type WhoisFetcher = Arc<dyn Fn(SocketAddr) -> std::result::Result<Whois, String> + Send + Sync>;

/// tailscaled's LocalAPI socket on Linux.
pub const LOCALAPI_SOCKETS: &[&str] = &[
    "/var/run/tailscale/tailscaled.sock",
    "/run/tailscale/tailscaled.sock",
];

/// The CLI, on PATH or where the macOS app keeps it.
const CLIS: &[&str] = &[
    "tailscale",
    "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
];

const TTL: Duration = Duration::from_secs(60);
const FAIL_TTL: Duration = Duration::from_secs(5);
const MAX_CACHED: usize = 4096;

/// 100.64.0.0/10 (IPv4, also v4-mapped) or fd7a:115c:a1e0::/48.
pub fn is_tailnet_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let o = v.octets();
            o[0] == 100 && (o[1] & 0xc0) == 64
        }
        IpAddr::V6(v) => match v.to_ipv4_mapped() {
            Some(v4) => is_tailnet_ip(IpAddr::V4(v4)),
            None => {
                let s = v.segments();
                s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0
            }
        },
    }
}

/// `--superadmin-tailnet`: login names and node tags.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AllowList {
    pub logins: Vec<String>,
    pub tags: Vec<String>,
}

impl AllowList {
    /// Comma-separated `login@domain` and `tag:name` entries; at least one.
    pub fn parse(list: &str) -> Result<AllowList> {
        let mut a = AllowList::default();
        for e in list.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            if let Some(t) = e.strip_prefix("tag:") {
                if t.is_empty()
                    || !t
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
                {
                    return Err(Error::invalid(format!(
                        "--superadmin-tailnet: bad tag {e:?}"
                    )));
                }
                a.tags.push(e.to_ascii_lowercase());
            } else if e.contains('@') && !e.contains(char::is_whitespace) {
                a.logins.push(e.to_ascii_lowercase());
            } else {
                return Err(Error::invalid(format!(
                    "--superadmin-tailnet: {e:?} is neither a login name (someone@example.com) nor a tag (tag:name)"
                )));
            }
        }
        if a.logins.is_empty() && a.tags.is_empty() {
            return Err(Error::invalid(
                "--superadmin-tailnet needs at least one login name or tag",
            ));
        }
        Ok(a)
    }

    /// A tagged node by its tags only; any other by its user's login.
    pub fn admits(&self, w: &Whois) -> bool {
        if w.tags.is_empty() {
            self.logins.iter().any(|l| l.eq_ignore_ascii_case(&w.login))
        } else {
            w.tags
                .iter()
                .any(|t| self.tags.iter().any(|a| a.eq_ignore_ascii_case(t)))
        }
    }

    /// Every entry, as given (lowercased).
    pub fn entries(&self) -> Vec<String> {
        self.logins.iter().chain(&self.tags).cloned().collect()
    }
}

/// The check `isb serve --superadmin-tailnet` runs on every TCP request.
pub struct Tailnet {
    allow: AllowList,
    fetch: WhoisFetcher,
    /// Lowercased hostnames (no port) a request's `Host` may name.
    hosts: Vec<String>,
    cache: Mutex<HashMap<IpAddr, (Instant, Option<Whois>)>>,
    warned: AtomicBool,
}

impl std::fmt::Debug for Tailnet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tailnet")
            .field("allow", &self.allow)
            .field("hosts", &self.hosts)
            .finish()
    }
}

impl Tailnet {
    pub fn new(allow: AllowList, fetch: WhoisFetcher, hosts: Vec<String>) -> Tailnet {
        let mut hosts: Vec<String> = hosts
            .into_iter()
            .map(|h| host_only(&h))
            .filter(|h| !h.is_empty())
            .collect();
        hosts.sort();
        hosts.dedup();
        Tailnet {
            allow,
            fetch,
            hosts,
            cache: Mutex::new(HashMap::new()),
            warned: AtomicBool::new(false),
        }
    }

    pub fn allow(&self) -> &AllowList {
        &self.allow
    }

    pub fn hosts(&self) -> &[String] {
        &self.hosts
    }

    /// The tailnet identity behind `req` when it is on the allow list:
    /// a TCP peer on a tailnet address, a `Host` naming this server, and a
    /// whois that matches.
    pub fn superadmin(&self, req: &Request) -> Option<Whois> {
        let Peer::Tcp(peer) = &req.peer else {
            return None;
        };
        if !is_tailnet_ip(peer.ip()) {
            return None;
        }
        let host = req.header("host").map(host_only).unwrap_or_default();
        if !self.hosts.contains(&host) {
            eprintln!(
                "isb serve: tailnet peer {peer} sent Host {host:?}, not one of this server's names; not a superadmin"
            );
            return None;
        }
        let w = self.whois(*peer)?;
        if self.allow.admits(&w) { Some(w) } else { None }
    }

    fn whois(&self, peer: SocketAddr) -> Option<Whois> {
        let now = Instant::now();
        if let Some((at, w)) = self.lock().get(&peer.ip()) {
            let ttl = if w.is_some() { TTL } else { FAIL_TTL };
            if now.duration_since(*at) < ttl {
                return w.clone();
            }
        }
        let got = match (self.fetch)(peer) {
            Ok(w) => Some(w),
            Err(e) => {
                if !self.warned.swap(true, Ordering::Relaxed) {
                    eprintln!(
                        "isb serve: tailnet superadmins are unavailable until tailscaled answers: {e}"
                    );
                }
                None
            }
        };
        let mut c = self.lock();
        if c.len() >= MAX_CACHED {
            c.clear();
        }
        c.insert(peer.ip(), (now, got.clone()));
        got
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<IpAddr, (Instant, Option<Whois>)>> {
        self.cache.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// `host[:port]` or `[v6]:port` to its lowercased host, brackets kept for
/// v6 so it compares with what a browser sends.
pub fn host_only(h: &str) -> String {
    let h = h.trim().trim_end_matches('.').to_ascii_lowercase();
    if h.starts_with('[') {
        return match h.find(']') {
            Some(i) => h[..=i].to_string(),
            None => h,
        };
    }
    // A bare v6 address (from a listen address) gets brackets.
    if h.matches(':').count() > 1 {
        return format!("[{h}]");
    }
    match h.rsplit_once(':') {
        Some((host, port)) if port.bytes().all(|b| b.is_ascii_digit()) => {
            host.trim_end_matches('.').to_string()
        }
        _ => h,
    }
}

/// Parse tailscale's whois JSON (LocalAPI and CLI share the shape).
pub fn parse_whois(v: &Value) -> std::result::Result<Whois, String> {
    let login = v["UserProfile"]["LoginName"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("whois: no UserProfile.LoginName")?
        .to_string();
    let node = v["Node"]["Name"]
        .as_str()
        .or_else(|| v["Node"]["ComputedName"].as_str())
        .unwrap_or("")
        .trim_end_matches('.')
        .to_string();
    let tags = v["Node"]["Tags"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_ascii_lowercase)
                .collect()
        })
        .unwrap_or_default();
    Ok(Whois { login, node, tags })
}

/// GET a LocalAPI path over tailscaled's unix socket (HTTP/1.0, so the
/// answer ends at EOF), within a few seconds.
fn localapi_get(socket: &str, path: &str) -> std::result::Result<Value, String> {
    use std::os::unix::net::UnixStream;
    let mut s = UnixStream::connect(socket).map_err(|e| format!("{socket}: {e}"))?;
    let t = Some(Duration::from_secs(3));
    let _ = s.set_read_timeout(t);
    let _ = s.set_write_timeout(t);
    write!(
        s,
        "GET {path} HTTP/1.0\r\nHost: local-tailscaled.sock\r\n\r\n"
    )
    .map_err(|e| format!("{socket}: {e}"))?;
    let mut buf = Vec::new();
    s.take(4 << 20)
        .read_to_end(&mut buf)
        .map_err(|e| format!("{socket}: {e}"))?;
    let text = String::from_utf8_lossy(&buf);
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| format!("{socket}: a malformed answer"))?;
    let status = head.split_whitespace().nth(1).unwrap_or("");
    if status != "200" {
        return Err(format!(
            "{socket}: LocalAPI {path} answered {status}: {}",
            body.trim().chars().take(200).collect::<String>()
        ));
    }
    serde_json::from_str(body).map_err(|e| format!("{socket}: {e}"))
}

/// Run the tailscale CLI with a deadline; its stdout as JSON.
fn cli_json(args: &[&str]) -> std::result::Result<Value, String> {
    use std::process::{Command, Stdio};
    let mut last = String::from("no tailscale CLI found");
    for cli in CLIS {
        let child = Command::new(cli)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                last = format!("{cli}: {e}");
                continue;
            }
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("{cli} {}: timed out", args.join(" ")));
                }
            }
        };
        let mut out = Vec::new();
        if let Some(mut o) = child.stdout.take() {
            let _ = o.read_to_end(&mut out);
        }
        if !status.success() {
            let mut err = String::new();
            if let Some(mut e) = child.stderr.take() {
                let _ = e.read_to_string(&mut err);
            }
            return Err(format!("{cli} {}: {}", args.join(" "), err.trim()));
        }
        return serde_json::from_slice(&out).map_err(|e| format!("{cli}: {e}"));
    }
    Err(last)
}

fn localapi_or_cli(path: &str, cli_args: &[&str]) -> std::result::Result<Value, String> {
    let mut errs = Vec::new();
    for s in LOCALAPI_SOCKETS {
        if !std::path::Path::new(s).exists() {
            continue;
        }
        match localapi_get(s, path) {
            Ok(v) => return Ok(v),
            Err(e) => errs.push(e),
        }
    }
    match cli_json(cli_args) {
        Ok(v) => Ok(v),
        Err(e) => {
            errs.push(e);
            Err(errs.join("; "))
        }
    }
}

/// The real thing: tailscaled's LocalAPI, else the CLI.
pub fn system_fetcher() -> WhoisFetcher {
    Arc::new(|peer: SocketAddr| {
        let addr = peer.to_string();
        let v = localapi_or_cli(
            &format!("/localapi/v0/whois?addr={}", urlencode(&addr)),
            &["whois", "--json", &addr],
        )?;
        parse_whois(&v)
    })
}

/// This node's MagicDNS name and short host name, for the `Host` check.
/// Empty when tailscaled does not answer.
pub fn self_names() -> Vec<String> {
    let Ok(v) = localapi_or_cli(
        "/localapi/v0/status?peers=false",
        &["status", "--json", "--peers=false"],
    ) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(d) = v["Self"]["DNSName"].as_str() {
        let d = d.trim_end_matches('.').to_ascii_lowercase();
        if let Some((short, _)) = d.split_once('.') {
            out.push(short.to_string());
        }
        if !d.is_empty() {
            out.push(d);
        }
    }
    if let Some(h) = v["Self"]["HostName"].as_str() {
        out.push(h.to_ascii_lowercase());
    }
    out
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'_' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn req(peer: &str, host: &str, headers: &[(&str, &str)]) -> Request {
        let mut h = vec![("Host".to_string(), host.to_string())];
        h.extend(headers.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        Request {
            method: "GET".into(),
            path: "/api/v1/auth/me".into(),
            query: None,
            headers: h,
            body: Vec::new(),
            peer: Peer::Tcp(peer.parse().unwrap()),
        }
    }

    fn who(login: &str, tags: &[&str]) -> Whois {
        Whois {
            login: login.into(),
            node: "laptop.tail1.ts.net".into(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
        }
    }

    #[test]
    fn tailnet_ranges() {
        for ip in [
            "100.64.0.1",
            "100.127.255.254",
            "100.86.22.100",
            "fd7a:115c:a1e0::1",
            "::ffff:100.100.1.1",
        ] {
            assert!(is_tailnet_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "100.63.255.255",
            "100.128.0.1",
            "127.0.0.1",
            "10.0.0.1",
            "fd7a:115c:a1e1::1",
            "::1",
        ] {
            assert!(!is_tailnet_ip(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn allow_lists() {
        assert!(AllowList::parse("").is_err());
        assert!(AllowList::parse(" , ").is_err());
        assert!(AllowList::parse("bob").is_err());
        assert!(AllowList::parse("tag:").is_err());
        assert!(AllowList::parse("tag:a b").is_err());
        let a = AllowList::parse("Someone@Example.com, tag:agents").unwrap();
        assert_eq!(a.entries(), vec!["someone@example.com", "tag:agents"]);
        assert!(a.admits(&who("someone@example.com", &[])));
        assert!(!a.admits(&who("other@example.com", &[])));
        assert!(a.admits(&who("tagged-devices", &["tag:agents"])));
        assert!(!a.admits(&who("tagged-devices", &["tag:other"])));
        // A tagged node is its tags, never a login.
        let l = AllowList::parse("someone@example.com").unwrap();
        assert!(!l.admits(&who("someone@example.com", &["tag:x"])));
    }

    #[test]
    fn whois_json() {
        let v = serde_json::json!({
            "Node": {"Name": "titan.tail9.ts.net.", "Tags": ["tag:Agents"]},
            "UserProfile": {"LoginName": "tagged-devices"}
        });
        let w = parse_whois(&v).unwrap();
        assert_eq!(w.node, "titan.tail9.ts.net");
        assert_eq!(w.tags, vec!["tag:agents"]);
        assert!(parse_whois(&serde_json::json!({})).is_err());
    }

    #[test]
    fn hosts() {
        assert_eq!(host_only("Titan.tail9.ts.net.:18995"), "titan.tail9.ts.net");
        assert_eq!(host_only("100.86.22.100:18995"), "100.86.22.100");
        assert_eq!(host_only("[fd7a::1]:80"), "[fd7a::1]");
        assert_eq!(host_only("fd7a::1"), "[fd7a::1]");
    }

    #[test]
    fn gate_checks_peer_host_and_list_and_caches() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let fetch: WhoisFetcher = Arc::new(move |p: SocketAddr| {
            c.fetch_add(1, Ordering::SeqCst);
            match p.ip().to_string().as_str() {
                "100.64.0.1" => Ok(who("me@example.com", &[])),
                "100.64.0.2" => Ok(who("other@example.com", &[])),
                _ => Err("tailscaled is not running".into()),
            }
        });
        let t = Tailnet::new(
            AllowList::parse("me@example.com").unwrap(),
            fetch,
            vec!["100.86.22.100:18995".into(), "titan.tail9.ts.net".into()],
        );
        let ok = req("100.64.0.1:5555", "100.86.22.100:18995", &[]);
        assert_eq!(t.superadmin(&ok).unwrap().login, "me@example.com");
        // Cached: a second request does not ask again.
        assert!(
            t.superadmin(&req("100.64.0.1:6666", "titan.tail9.ts.net", &[]))
                .is_some()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // Not on the list.
        assert!(
            t.superadmin(&req("100.64.0.2:1", "100.86.22.100", &[]))
                .is_none()
        );
        // DNS rebinding: another site's name.
        assert!(
            t.superadmin(&req("100.64.0.1:1", "evil.example:18995", &[]))
                .is_none()
        );
        assert!(t.superadmin(&req("100.64.0.1:1", "", &[])).is_none());
        // Not a tailnet peer, whatever the headers claim.
        let spoof = req(
            "127.0.0.1:1",
            "100.86.22.100",
            &[
                ("Tailscale-User-Login", "me@example.com"),
                ("X-Forwarded-For", "100.64.0.1"),
            ],
        );
        assert!(t.superadmin(&spoof).is_none());
        let spoof = req(
            "10.1.2.3:1",
            "100.86.22.100",
            &[
                ("Tailscale-User-Login", "me@example.com"),
                ("X-Forwarded-For", "100.64.0.1"),
            ],
        );
        assert!(t.superadmin(&spoof).is_none());
        // tailscaled unreachable: nobody.
        assert!(
            t.superadmin(&req("100.64.0.9:1", "100.86.22.100", &[]))
                .is_none()
        );
        // The unix socket is not a tailnet peer.
        let mut u = ok.clone();
        u.peer = Peer::Unix { uid: None };
        assert!(t.superadmin(&u).is_none());
    }
}
