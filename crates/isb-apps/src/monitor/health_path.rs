//! Which path an app or stack service monitor requests, on the domain and
//! on the replica.
//!
//! A monitor without a `path` uses the service's own health path when its
//! compose `healthcheck` requests one over HTTP on the port the domain
//! routes to: `/` of an API often answers 401 or 404 while the service is
//! fine. A route that strips its prefix hands the replica the path without
//! it, so the replica is asked for what it actually receives.

use crate::spec::DomainSpec;

/// The paths one check uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Paths {
    /// On the domain; `None` when the domain cannot reach the path.
    pub public: Option<String>,
    /// On the replica: what the ingress would hand it.
    pub internal: String,
    /// Why the paths are what they are, for the outcome.
    pub note: Option<String>,
}

/// Is `p` under the route prefix `d` (`/api` covers `/api` and `/api/...`)?
fn under(d: &str, p: &str) -> bool {
    let d = d.trim_end_matches('/');
    d.is_empty()
        || p.strip_prefix(d)
            .is_some_and(|r| r.is_empty() || r.starts_with(['/', '?']))
}

/// What the replica receives for public path `p` on a route with prefix `d`.
fn stripped(d: &str, p: &str, strip: bool) -> String {
    let d = d.trim_end_matches('/');
    if !strip || d.is_empty() || !under(d, p) {
        return p.to_string();
    }
    let r = &p[d.len()..];
    if r.starts_with('/') {
        r.to_string()
    } else {
        format!("/{r}")
    }
}

/// Choose the paths: the monitor's own `path` wins, then the service's
/// health path, then the domain's path. `domain` is the route's prefix
/// (`/` without a domain) and whether it strips it.
pub(crate) fn choose(
    explicit: Option<&str>,
    domain: &str,
    strip: bool,
    health: Option<&str>,
    noun: &str,
) -> Paths {
    let d = domain.trim_end_matches('/');
    let (p, note) = match (explicit, health) {
        (Some(p), _) => (p.to_string(), None),
        (None, Some(h)) => {
            let public = if d.is_empty() {
                Some(h.to_string())
            } else if strip {
                Some(format!("{d}{h}"))
            } else if under(d, h) {
                Some(h.to_string())
            } else {
                None
            };
            let note = match &public {
                Some(_) => format!("{h} is the path of the {noun}'s healthcheck"),
                None => format!(
                    "the health path {h} is not under the domain's path {domain}: checked the {noun}'s own endpoint"
                ),
            };
            return Paths {
                public,
                internal: h.to_string(),
                note: Some(note),
            };
        }
        (None, None) => (if d.is_empty() { "/".into() } else { d.into() }, None),
    };
    Paths {
        internal: stripped(d, &p, strip),
        public: Some(p),
        note,
    }
}

/// The domain spec a served domain came from: same path, and the same host
/// (or `auto`, whose name the ingress generates).
pub(crate) fn spec_of<'a>(
    specs: &'a [DomainSpec],
    host: &str,
    path: &str,
) -> Option<&'a DomainSpec> {
    let same_path = |s: &DomainSpec| {
        crate::ingress::domain::normalize_path(s.path.as_deref()).is_ok_and(|p| p == path)
    };
    let candidates = || {
        specs
            .iter()
            .filter(|s| s.redirect.is_none() && same_path(s))
    };
    candidates()
        .find(|s| s.host.trim().eq_ignore_ascii_case(host))
        .or_else(|| candidates().find(|s| s.host.trim() == "auto"))
}

/// Characters that end a URL inside a command line.
fn ends_url(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '\'' | '"' | '`' | ')' | ';' | ',' | '<' | '>' | '|' | '&'
        )
}

const LOCAL: [&str; 3] = ["127.0.0.1", "localhost", "[::1]"];

/// A path a request may carry: absolute, nothing a shell would expand.
fn usable(p: &str) -> Option<String> {
    let p = p.split('#').next().unwrap_or_default();
    let p = if p.is_empty() {
        "/".to_string()
    } else if p.starts_with('?') {
        format!("/{p}")
    } else {
        p.to_string()
    };
    (p.starts_with('/') && !p.contains(['$', '{', '}', '\\'])).then_some(p)
}

/// `http://<local>[:port]<path>` URLs in `s`, as (port, path).
fn local_urls(s: &str) -> Vec<(u16, String)> {
    let mut out = Vec::new();
    for (i, _) in s.match_indices("http://") {
        let rest = &s[i + "http://".len()..];
        let url = &rest[..rest.find(ends_url).unwrap_or(rest.len())];
        let split = url.find(['/', '?', '#']).unwrap_or(url.len());
        let (auth, path) = url.split_at(split);
        let Some(host) = LOCAL.iter().find(|h| auth.starts_with(**h)) else {
            continue;
        };
        let port = match &auth[host.len()..] {
            "" => 80,
            p => match p.strip_prefix(':').and_then(|p| p.parse().ok()) {
                Some(p) => p,
                None => continue,
            },
        };
        if let Some(path) = usable(path) {
            out.push((port, path));
        }
    }
    out
}

/// bash's `/dev/tcp/<local>/<port>` with a hand-written `GET <path> HTTP/`.
fn dev_tcp(s: &str) -> Option<(u16, String)> {
    let i = s.find("/dev/tcp/")?;
    let rest = &s[i + "/dev/tcp/".len()..];
    let (host, rest) = rest.split_once('/')?;
    if !LOCAL.contains(&host) {
        return None;
    }
    let port = rest
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()?;
    let get = s.find("GET ")?;
    let path = s[get + 4..].split(' ').next()?;
    s[get + 4 + path.len()..]
        .starts_with(" HTTP/")
        .then(|| usable(path))
        .flatten()
        .map(|p| (port, p))
}

/// The path a compose healthcheck `test` requests over HTTP from the
/// replica itself on `port`: `[CMD, ...]`, `[CMD-SHELL, line]` or a plain
/// line (already `[CMD-SHELL, line]` once parsed). Anything else, a URL on
/// another port or host, or no URL at all: `None`.
pub(crate) fn from_healthcheck(test: &[String], port: u16) -> Option<String> {
    let args = match test.first().map(String::as_str) {
        Some("CMD" | "CMD-SHELL") => &test[1..],
        _ => return None,
    };
    args.iter().find_map(|a| {
        local_urls(a)
            .into_iter()
            .chain(dev_tcp(a))
            .find(|(p, _)| *p == port)
            .map(|(_, path)| path)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn healthcheck_paths() {
        let wget = t(&[
            "CMD",
            "wget",
            "-q",
            "-O",
            "/dev/null",
            "http://127.0.0.1:8000/healthz",
        ]);
        assert_eq!(from_healthcheck(&wget, 8000).as_deref(), Some("/healthz"));
        // Another port is another listener: not what the domain reaches.
        assert_eq!(from_healthcheck(&wget, 8080), None);
        let curl = t(&[
            "CMD-SHELL",
            "curl -fsS http://localhost:3000/api/health?deep=1 || exit 1",
        ]);
        assert_eq!(
            from_healthcheck(&curl, 3000).as_deref(),
            Some("/api/health?deep=1")
        );
        let spider = t(&["CMD-SHELL", "wget -q --spider 'http://[::1]/ping'"]);
        assert_eq!(from_healthcheck(&spider, 80).as_deref(), Some("/ping"));
        let qo = t(&["CMD", "wget", "-qO-", "http://127.0.0.1:5556"]);
        assert_eq!(from_healthcheck(&qo, 5556).as_deref(), Some("/"));
        let py = t(&[
            "CMD",
            "python3",
            "-c",
            "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8080/healthz', timeout=5)",
        ]);
        assert_eq!(from_healthcheck(&py, 8080).as_deref(), Some("/healthz"));
        let tcp = t(&[
            "CMD",
            "bash",
            "-c",
            "exec 3<>/dev/tcp/127.0.0.1/8080; printf \"GET /api/v1/health HTTP/1.0\\r\\nHost: localhost\\r\\n\\r\\n\" >&3; grep -q healthy <&3",
        ]);
        assert_eq!(
            from_healthcheck(&tcp, 8080).as_deref(),
            Some("/api/v1/health")
        );
        // The first URL on the right port wins.
        let two = t(&[
            "CMD-SHELL",
            "curl -f http://127.0.0.1:9090/metrics && curl -f http://127.0.0.1:8080/ready",
        ]);
        assert_eq!(from_healthcheck(&two, 8080).as_deref(), Some("/ready"));
        for none in [
            t(&["CMD", "redis-cli", "ping"]),
            t(&["CMD", "nc", "-z", "127.0.0.1", "5173"]),
            t(&["NONE"]),
            t(&[]),
            t(&["CMD", "curl", "-f", "http://db:8080/healthz"]),
            t(&["CMD", "curl", "-f", "https://127.0.0.1:8080/healthz"]),
            t(&["CMD-SHELL", "curl -f http://127.0.0.1:$PORT/healthz"]),
            t(&["CMD-SHELL", "curl -f http://127.0.0.1:8080/${HEALTH}"]),
            t(&["CMD-SHELL", "curl -f http://127.0.0.10:8080/healthz"]),
        ] {
            assert_eq!(from_healthcheck(&none, 8080), None, "{none:?}");
        }
    }

    fn c(
        explicit: Option<&str>,
        d: &str,
        strip: bool,
        h: Option<&str>,
    ) -> (Option<String>, String) {
        let p = choose(explicit, d, strip, h, "service");
        (p.public, p.internal)
    }

    fn s(v: &str) -> Option<String> {
        Some(v.into())
    }

    #[test]
    fn choosing_paths() {
        // The domain's root reaches every path.
        assert_eq!(
            c(None, "/", false, Some("/healthz")),
            (s("/healthz"), "/healthz".into())
        );
        // A stripped prefix goes in front.
        assert_eq!(
            c(None, "/api", true, Some("/healthz")),
            (s("/api/healthz"), "/healthz".into())
        );
        // An unstripped prefix the path is under.
        assert_eq!(
            c(None, "/api", false, Some("/api/health")),
            (s("/api/health"), "/api/health".into())
        );
        assert_eq!(
            c(None, "/api", false, Some("/api?x=1")),
            (s("/api?x=1"), "/api?x=1".into())
        );
        // Out of the domain's reach: only the replica.
        let p = choose(None, "/sso", false, Some("/healthz"), "service");
        assert_eq!(p.public, None);
        assert_eq!(p.internal, "/healthz");
        assert_eq!(
            p.note.as_deref(),
            Some(
                "the health path /healthz is not under the domain's path /sso: checked the service's own endpoint"
            )
        );
        assert_eq!(c(None, "/api", false, Some("/apix")).0, None);
        // The note says where a derived path came from.
        assert_eq!(
            choose(None, "/", false, Some("/healthz"), "app")
                .note
                .as_deref(),
            Some("/healthz is the path of the app's healthcheck")
        );
        // The monitor's own path always wins.
        let p = choose(Some("/status"), "/", false, Some("/healthz"), "service");
        assert_eq!(
            (p.public, p.internal, p.note),
            (s("/status"), "/status".into(), None)
        );
        // No health path: the domain's path, as before.
        assert_eq!(c(None, "/", false, None), (s("/"), "/".into()));
        assert_eq!(c(None, "/api", false, None), (s("/api"), "/api".into()));
    }

    #[test]
    fn stripped_prefixes_reach_the_replica_stripped() {
        // What the replica receives once the ingress strips the prefix.
        assert_eq!(c(None, "/api", true, None), (s("/api"), "/".into()));
        assert_eq!(
            c(Some("/api/status"), "/api", true, None),
            (s("/api/status"), "/status".into())
        );
        assert_eq!(
            c(Some("/api?x"), "/api", true, None),
            (s("/api?x"), "/?x".into())
        );
        // A path outside the prefix is not rewritten.
        assert_eq!(
            c(Some("/other"), "/api", true, None),
            (s("/other"), "/other".into())
        );
        // Without strip_prefix, the replica gets the public path.
        assert_eq!(
            c(Some("/api/status"), "/api", false, None),
            (s("/api/status"), "/api/status".into())
        );
    }

    #[test]
    fn domain_specs_by_status() {
        let d = |host: &str, path: Option<&str>, strip: bool| DomainSpec {
            host: host.into(),
            path: path.map(String::from),
            port: Some(8080),
            strip_prefix: strip,
            ..Default::default()
        };
        let specs = vec![
            d("a.example.com", Some("/api/"), true),
            d("A.example.com", None, false),
            d("auto", Some("/x"), false),
        ];
        assert!(
            spec_of(&specs, "a.example.com", "/api")
                .unwrap()
                .strip_prefix
        );
        assert!(!spec_of(&specs, "a.example.com", "/").unwrap().strip_prefix);
        assert_eq!(
            spec_of(&specs, "web-s-o.1.2.3.4.sslip.io", "/x")
                .unwrap()
                .host,
            "auto"
        );
        assert!(spec_of(&specs, "b.example.com", "/").is_none());
    }
}
