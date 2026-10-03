//! A service's `domains:` entries: validation, generated hostnames, org
//! allowlists, and who gets a hostname when two stacks ask for it.

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::org::OrgId;
use crate::spec::DomainSpec;

/// The `host` that asks for a generated name.
pub const AUTO: &str = "auto";

/// One validated `domains:` entry of one service.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Route {
    pub org: OrgId,
    /// The stack's own name (not qualified).
    pub stack: String,
    pub service: String,
    /// Lowercase; `auto` already resolved.
    pub host: String,
    /// `/` or a prefix without a trailing slash.
    pub path: String,
    /// The replicas' port; `None` for a redirect.
    pub port: Option<u16>,
    pub https: bool,
    pub redirect: Option<String>,
    pub strip_prefix: bool,
    /// Made from another entry's `www_redirect`.
    pub generated: bool,
    /// A generated (`host: auto`) name: outside any allowlist.
    pub auto: bool,
}

impl Route {
    pub fn qualified_stack(&self) -> String {
        crate::stack::qualified(&self.org, &self.stack)
    }

    pub fn is_wildcard(&self) -> bool {
        self.host.starts_with("*.")
    }

    /// Who the route belongs to: `(org, stack, service)`.
    pub fn owner(&self) -> (String, String, String) {
        (
            self.org.to_string(),
            self.stack.clone(),
            self.service.clone(),
        )
    }

    /// The URL it is reached at, given the public ports.
    pub fn url(&self, https_port: Option<u16>, http_port: Option<u16>) -> String {
        let (scheme, port, default) = if self.https {
            ("https", https_port, 443)
        } else {
            ("http", http_port, 80)
        };
        let port = match port {
            Some(p) if p != default => format!(":{p}"),
            _ => String::new(),
        };
        let path = if self.path == "/" { "/" } else { &self.path };
        format!("{scheme}://{}{port}{path}", self.host)
    }
}

/// Check one service's entries without anything host-specific: hostname
/// syntax, paths, ports, redirects, duplicates.
pub fn validate(service: &str, domains: &[DomainSpec]) -> Result<()> {
    let mut seen = BTreeSet::new();
    for d in domains {
        let what = format!("service {service}: domain {:?}", d.host);
        let bad = |why: String| Error::invalid(format!("{what}: {why}"));
        let host = d.host.trim().to_ascii_lowercase();
        if host != AUTO {
            check_host(&host).map_err(bad)?;
        }
        let path = normalize_path(d.path.as_deref()).map_err(bad)?;
        match (&d.redirect, d.port) {
            (Some(r), _) => check_redirect(r).map_err(bad)?,
            (None, None) => return Err(bad("needs port (or redirect)".into())),
            (None, Some(0)) => return Err(bad("port must be 1-65535".into())),
            (None, Some(_)) => {}
        }
        if d.strip_prefix && path == "/" {
            return Err(bad("strip_prefix needs a path".into()));
        }
        if d.www_redirect && (host == AUTO || host.starts_with("*.") || host.starts_with("www.")) {
            return Err(bad(
                "www_redirect goes on the bare name (example.com), not a www, wildcard or auto host"
                    .into(),
            ));
        }
        if !seen.insert((host.clone(), path.clone())) {
            return Err(bad(format!("{host}{path} is listed twice")));
        }
    }
    Ok(())
}

/// A hostname a domain may name: DNS labels, at least two of them, the
/// first optionally `*`. No IP addresses, `localhost` or isb's own `.isb`.
pub fn check_host(host: &str) -> std::result::Result<(), String> {
    if host.is_empty() || host.len() > 253 {
        return Err("a hostname is 1-253 characters".into());
    }
    if host.parse::<IpAddr>().is_ok() {
        return Err("an IP address is not a hostname; use host: auto for a generated name".into());
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 {
        return Err("a hostname needs a domain (app.example.com)".into());
    }
    for (i, l) in labels.iter().enumerate() {
        if i == 0 && *l == "*" {
            if labels.len() < 3 {
                return Err("a wildcard needs a domain under it (*.example.com)".into());
            }
            continue;
        }
        let ok = !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !ok {
            return Err(format!(
                "label {l:?} is not a DNS label ([a-z0-9-], at most 63, no leading or trailing -)"
            ));
        }
    }
    let tld = labels[labels.len() - 1];
    if tld == "isb" || tld == "localhost" || tld == "incus" {
        return Err(format!(".{tld} names are internal"));
    }
    Ok(())
}

/// `None` and `/` are `/`; otherwise an absolute prefix, trailing slash
/// dropped, without query, fragment, wildcards or whitespace.
pub fn normalize_path(p: Option<&str>) -> std::result::Result<String, String> {
    let p = p.unwrap_or("/").trim();
    if p.is_empty() || p == "/" {
        return Ok("/".into());
    }
    if !p.starts_with('/') {
        return Err(format!("path {p:?} must start with /"));
    }
    if p.contains(|c: char| c.is_whitespace() || matches!(c, '?' | '#' | '*' | '%' | '{' | '}')) {
        return Err(format!(
            "path {p:?}: a plain prefix, without ? # * % or braces"
        ));
    }
    if p.contains("//") || p.split('/').any(|s| s == ".." || s == ".") {
        return Err(format!("path {p:?} is not normalized"));
    }
    Ok(p.trim_end_matches('/').to_string())
}

fn check_redirect(r: &str) -> std::result::Result<(), String> {
    let rest = r
        .strip_prefix("https://")
        .or_else(|| r.strip_prefix("http://"))
        .ok_or_else(|| format!("redirect {r:?} must be an http(s):// URL"))?;
    if rest.is_empty()
        || rest.starts_with('/')
        || r.contains(|c: char| c.is_whitespace() || c.is_control() || matches!(c, '{' | '}' | '"'))
    {
        return Err(format!("redirect {r:?} is not a URL"));
    }
    Ok(())
}

/// Whether a redirect target keeps the request's path: it has none of its own.
pub fn redirect_keeps_path(target: &str) -> bool {
    let rest = target
        .strip_prefix("https://")
        .or_else(|| target.strip_prefix("http://"))
        .unwrap_or(target);
    match rest.find('/') {
        None => true,
        Some(i) => &rest[i..] == "/",
    }
}

/// The generated name for `host: auto`:
/// `<service>-<stack>-<org>.<a-b-c-d>.sslip.io`, which sslip.io resolves to
/// `a.b.c.d`. A first label over 63 characters is shortened with a hash.
pub fn auto_host(org: &OrgId, stack: &str, service: &str, ip: IpAddr) -> Result<String> {
    let IpAddr::V4(v4) = ip else {
        return Err(Error::invalid(format!(
            "host: auto needs an IPv4 public address, not {ip}"
        )));
    };
    let mut label = format!(
        "{}-{stack}-{}",
        crate::compose::sanitize_name(service),
        org.as_str()
    );
    if label.len() > 63 {
        let mut h: u32 = 0x811c9dc5;
        for b in label.bytes() {
            h ^= b as u32;
            h = h.wrapping_mul(0x01000193);
        }
        label.truncate(54);
        let label2 = label.trim_end_matches('-').to_string();
        label = format!("{label2}-{h:08x}");
    }
    let o = v4.octets();
    Ok(format!(
        "{label}.{}-{}-{}-{}.sslip.io",
        o[0], o[1], o[2], o[3]
    ))
}

/// Expand one service's entries into routes: `auto` resolved (needs
/// `public_ip`), `www_redirect` adding its redirect route.
pub fn routes_for(
    org: &OrgId,
    stack: &str,
    service: &str,
    domains: &[DomainSpec],
    public_ip: Option<IpAddr>,
) -> Result<Vec<Route>> {
    validate(service, domains)?;
    let mut out = Vec::new();
    for d in domains {
        let mut host = d.host.trim().to_ascii_lowercase();
        let auto = host == AUTO;
        if auto {
            let ip = public_ip.ok_or_else(|| {
                Error::invalid(format!(
                    "service {service}: host: auto needs the server's public address (isb serve --ingress-public-ip)"
                ))
            })?;
            host = auto_host(org, stack, service, ip)?;
        }
        let path = normalize_path(d.path.as_deref()).map_err(Error::invalid)?;
        let https = d.https.unwrap_or(true);
        if d.www_redirect {
            let scheme = if https { "https" } else { "http" };
            out.push(Route {
                org: org.clone(),
                stack: stack.into(),
                service: service.into(),
                host: format!("www.{host}"),
                path: path.clone(),
                port: None,
                https,
                redirect: Some(format!("{scheme}://{host}")),
                strip_prefix: false,
                generated: true,
                auto,
            });
        }
        out.push(Route {
            org: org.clone(),
            stack: stack.into(),
            service: service.into(),
            host,
            path,
            port: if d.redirect.is_some() { None } else { d.port },
            https,
            redirect: d.redirect.clone(),
            strip_prefix: d.strip_prefix,
            generated: false,
            auto,
        });
    }
    Ok(out)
}

/// Whether two host patterns can name the same host. A wildcard covers
/// exactly one label, as in Caddy and TLS.
pub fn overlaps(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let covers = |w: &str, h: &str| -> bool {
        let Some(base) = w.strip_prefix("*.") else {
            return false;
        };
        match h.split_once('.') {
            Some((first, rest)) => rest == base && first != "*",
            None => false,
        }
    };
    covers(a, b) || covers(b, a)
}

/// Is `host` within an org's allowlist? Each entry is a domain suffix:
/// `example.com` allows it and every name under it; `*.example.com` also
/// allows wildcard hosts under `example.com`. An empty list allows any
/// concrete name and no wildcard. Generated (`auto`) names are always
/// allowed, and are not checked here.
pub fn allowed(host: &str, allowlist: &[String]) -> bool {
    let under = |h: &str, suffix: &str| h == suffix || h.ends_with(&format!(".{suffix}"));
    match host.strip_prefix("*.") {
        Some(base) => allowlist
            .iter()
            .filter_map(|e| e.strip_prefix("*."))
            .any(|s| under(base, s)),
        None if allowlist.is_empty() => true,
        None => allowlist
            .iter()
            .map(|e| e.strip_prefix("*.").unwrap_or(e))
            .any(|s| under(host, s)),
    }
}

/// A claim on a hostname and path, kept across daemon restarts so the first
/// claimant keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub host: String,
    pub path: String,
    pub org: String,
    pub stack: String,
    pub service: String,
    /// Unix seconds the claim was first granted.
    pub since: u64,
}

/// A route refused because another one holds its name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Conflict {
    pub route: Route,
    /// Shown to the route's own org: never names another org.
    pub reason: String,
}

/// The outcome of [`resolve`].
#[derive(Debug, Clone, Default)]
pub struct Resolution {
    pub accepted: Vec<Route>,
    pub conflicts: Vec<Conflict>,
    /// The claims to keep: one per accepted route.
    pub claims: Vec<Claim>,
}

/// Decide which routes are served. First claim wins: routes already holding
/// a claim go first (oldest first), then new ones in name order, so the
/// outcome never depends on the order stacks were loaded in.
///
/// - Across orgs a hostname belongs to one org: a route whose host overlaps
///   (equal, or covered by a wildcard) one another org already holds is
///   refused, whatever the paths.
/// - Within an org, one `(host, path)` goes to one service.
pub fn resolve(routes: &[Route], previous: &[Claim], now: u64) -> Resolution {
    let prev: BTreeMap<(String, String, String, String, String), u64> = previous
        .iter()
        .map(|c| {
            (
                (
                    c.host.clone(),
                    c.path.clone(),
                    c.org.clone(),
                    c.stack.clone(),
                    c.service.clone(),
                ),
                c.since,
            )
        })
        .collect();
    let key = |r: &Route| {
        (
            r.host.clone(),
            r.path.clone(),
            r.org.to_string(),
            r.stack.clone(),
            r.service.clone(),
        )
    };
    let mut order: Vec<(u64, &Route)> = routes
        .iter()
        .map(|r| (prev.get(&key(r)).copied().unwrap_or(u64::MAX), r))
        .collect();
    order.sort_by(|a, b| {
        (
            a.0,
            a.1.org.as_str(),
            &a.1.stack,
            &a.1.service,
            &a.1.host,
            &a.1.path,
        )
            .cmp(&(
                b.0,
                b.1.org.as_str(),
                &b.1.stack,
                &b.1.service,
                &b.1.host,
                &b.1.path,
            ))
    });
    let mut out = Resolution::default();
    for (since, r) in order {
        let cross = out
            .accepted
            .iter()
            .any(|a| a.org != r.org && overlaps(&a.host, &r.host));
        if cross {
            out.conflicts.push(Conflict {
                route: r.clone(),
                reason: format!("{} is already served by another org", r.host),
            });
            continue;
        }
        if let Some(a) = out
            .accepted
            .iter()
            .find(|a| a.org == r.org && a.host == r.host && a.path == r.path)
        {
            out.conflicts.push(Conflict {
                route: r.clone(),
                reason: format!(
                    "{}{} is already served by {}/{}",
                    r.host,
                    if r.path == "/" { "" } else { &r.path },
                    a.stack,
                    a.service
                ),
            });
            continue;
        }
        out.claims.push(Claim {
            host: r.host.clone(),
            path: r.path.clone(),
            org: r.org.to_string(),
            stack: r.stack.clone(),
            service: r.service.clone(),
            since: if since == u64::MAX { now } else { since },
        });
        out.accepted.push(r.clone());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: &str) -> Vec<DomainSpec> {
        serde_yaml_ng::from_str(y).unwrap()
    }

    fn org(s: &str) -> OrgId {
        OrgId::new(s).unwrap()
    }

    fn route(o: &str, stack: &str, svc: &str, host: &str, path: &str) -> Route {
        Route {
            org: org(o),
            stack: stack.into(),
            service: svc.into(),
            host: host.into(),
            path: path.into(),
            port: Some(80),
            https: true,
            redirect: None,
            strip_prefix: false,
            generated: false,
            auto: false,
        }
    }

    #[test]
    fn parses_the_contract_shape() {
        let v = d(
            "- {host: app.example.com, port: 8080}\n- {host: Example.com, path: /api/, port: 3000, https: false, strip_prefix: true}\n- {host: www.example.com, redirect: 'https://example.com'}\n",
        );
        validate("web", &v).unwrap();
        assert_eq!(v[1].https, Some(false));
        let r = routes_for(&org("acme"), "shop", "web", &v, None).unwrap();
        assert_eq!(r[1].host, "example.com");
        assert_eq!(r[1].path, "/api");
        assert!(!r[1].https);
        assert!(r[0].https, "https defaults to true");
        assert_eq!(r[0].path, "/", "path defaults to /");
        assert_eq!(r[2].port, None);
        // Unknown keys are refused, as everywhere in the spec.
        assert!(
            serde_yaml_ng::from_str::<Vec<DomainSpec>>("- {host: a.b, port: 1, tls: x}").is_err()
        );
    }

    #[test]
    fn refuses_bad_entries() {
        for (y, why) in [
            ("- {host: example.com}", "needs port"),
            ("- {host: localhost, port: 1}", "needs a domain"),
            ("- {host: a.isb, port: 1}", "internal"),
            ("- {host: 10.0.0.1, port: 1}", "IP address"),
            ("- {host: a_b.com, port: 1}", "DNS label"),
            ("- {host: -a.com, port: 1}", "DNS label"),
            ("- {host: '*.com', port: 1}", "wildcard needs"),
            ("- {host: a.*.com, port: 1}", "DNS label"),
            ("- {host: a.com, port: 0}", "1-65535"),
            ("- {host: a.com, path: api, port: 1}", "start with /"),
            ("- {host: a.com, path: '/a?b', port: 1}", "plain prefix"),
            ("- {host: a.com, path: '/a/../b', port: 1}", "normalized"),
            (
                "- {host: a.com, port: 1, strip_prefix: true}",
                "needs a path",
            ),
            ("- {host: a.com, redirect: example.com}", "http(s)://"),
            (
                "- {host: www.a.com, port: 1, www_redirect: true}",
                "bare name",
            ),
            (
                "- {host: a.com, port: 1}\n- {host: A.com, path: /, port: 2}",
                "twice",
            ),
        ] {
            let e = validate("web", &d(y)).unwrap_err().to_string();
            assert!(e.contains(why), "{y}: {e}");
        }
        validate("web", &d("- {host: '*.apps.example.com', port: 1}")).unwrap();
        validate("web", &d("- {host: auto, port: 1}")).unwrap();
    }

    #[test]
    fn generates_sslip_names() {
        let ip: IpAddr = "203.0.113.7".parse().unwrap();
        assert_eq!(
            auto_host(&org("acme"), "shop", "web", ip).unwrap(),
            "web-shop-acme.203-0-113-7.sslip.io"
        );
        assert_eq!(
            auto_host(&OrgId::default_org(), "shop", "my_api", ip).unwrap(),
            "my-api-shop-default.203-0-113-7.sslip.io"
        );
        let long = auto_host(&org("acme"), &"s".repeat(30), &"v".repeat(40), ip).unwrap();
        let first = long.split('.').next().unwrap();
        assert!(first.len() <= 63, "{first}");
        check_host(&long).unwrap();
        // Deterministic.
        assert_eq!(
            long,
            auto_host(&org("acme"), &"s".repeat(30), &"v".repeat(40), ip).unwrap()
        );
        assert!(auto_host(&org("acme"), "s", "w", "::1".parse().unwrap()).is_err());
        let r = routes_for(
            &org("acme"),
            "shop",
            "web",
            &d("- {host: auto, port: 80}"),
            Some(ip),
        )
        .unwrap();
        assert_eq!(r[0].host, "web-shop-acme.203-0-113-7.sslip.io");
        let e = routes_for(
            &org("acme"),
            "shop",
            "web",
            &d("- {host: auto, port: 80}"),
            None,
        )
        .unwrap_err();
        assert!(e.to_string().contains("--ingress-public-ip"), "{e}");
    }

    #[test]
    fn www_redirect_adds_a_route() {
        let r = routes_for(
            &org("acme"),
            "shop",
            "web",
            &d("- {host: example.com, port: 80, www_redirect: true}"),
            None,
        )
        .unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].host, "www.example.com");
        assert_eq!(r[0].redirect.as_deref(), Some("https://example.com"));
        assert!(r[0].generated);
        assert!(redirect_keeps_path("https://example.com"));
        assert!(redirect_keeps_path("https://example.com/"));
        assert!(!redirect_keeps_path("https://example.com/landing"));
    }

    #[test]
    fn urls() {
        let mut r = route("acme", "s", "w", "a.example.com", "/");
        assert_eq!(r.url(Some(443), Some(80)), "https://a.example.com/");
        assert_eq!(r.url(Some(8443), Some(80)), "https://a.example.com:8443/");
        r.https = false;
        r.path = "/api".into();
        assert_eq!(
            r.url(Some(443), Some(18080)),
            "http://a.example.com:18080/api"
        );
    }

    #[test]
    fn wildcards_and_allowlists() {
        assert!(overlaps("*.example.com", "a.example.com"));
        assert!(overlaps("a.example.com", "*.example.com"));
        assert!(!overlaps("*.example.com", "a.b.example.com"));
        assert!(!overlaps("*.example.com", "example.com"));
        assert!(!overlaps("a.example.com", "b.example.com"));

        let none: Vec<String> = vec![];
        assert!(allowed("anything.example.org", &none));
        assert!(
            !allowed("*.example.org", &none),
            "wildcards need the allowlist"
        );
        let list = vec!["example.com".to_string(), "*.apps.example.net".to_string()];
        assert!(allowed("example.com", &list));
        assert!(allowed("a.b.example.com", &list));
        assert!(!allowed("badexample.com", &list));
        assert!(!allowed("example.org", &list));
        assert!(!allowed("*.example.com", &list), "no wildcard entry for it");
        assert!(allowed("x.apps.example.net", &list));
        assert!(allowed("*.apps.example.net", &list));
        assert!(allowed("*.team.apps.example.net", &list));
        assert!(!allowed("*.example.net", &list));
    }

    #[test]
    fn first_claim_wins_across_orgs() {
        // b claimed first (persisted): it keeps the name although a sorts first.
        let ra = route("a", "s", "w", "app.example.com", "/");
        let rb = route("b", "s", "w", "app.example.com", "/api");
        let prev = vec![Claim {
            host: "app.example.com".into(),
            path: "/api".into(),
            org: "b".into(),
            stack: "s".into(),
            service: "w".into(),
            since: 100,
        }];
        let res = resolve(&[ra.clone(), rb.clone()], &prev, 200);
        assert_eq!(res.accepted, vec![rb.clone()]);
        assert_eq!(res.conflicts.len(), 1);
        assert_eq!(res.conflicts[0].route, ra);
        assert_eq!(
            res.conflicts[0].reason,
            "app.example.com is already served by another org"
        );
        assert_eq!(res.claims[0].since, 100);

        // No history: name order decides, and the claim is stamped now.
        let res = resolve(&[rb.clone(), ra.clone()], &[], 200);
        assert_eq!(res.accepted, vec![ra.clone()]);
        assert_eq!(res.claims[0].since, 200);
        // Same input order-independent.
        let res2 = resolve(&[ra.clone(), rb.clone()], &[], 200);
        assert_eq!(res2.accepted, res.accepted);

        // A wildcard of another org blocks names under it.
        let w = route("a", "s", "w", "*.example.com", "/");
        let c = route("b", "t", "x", "shop.example.com", "/");
        let res = resolve(&[c.clone(), w.clone()], &[], 1);
        assert_eq!(res.accepted, vec![w]);
        assert_eq!(res.conflicts[0].route, c);

        // The holder leaves: the next claimant gets the name.
        let res = resolve(std::slice::from_ref(&ra), &prev, 300);
        assert_eq!(res.accepted, vec![ra]);
    }

    #[test]
    fn within_an_org_paths_split_a_host() {
        let a = route("a", "s", "web", "app.example.com", "/");
        let b = route("a", "s", "api", "app.example.com", "/api");
        let c = route("a", "t", "api2", "app.example.com", "/api");
        let res = resolve(&[a.clone(), b.clone(), c.clone()], &[], 1);
        assert_eq!(res.accepted.len(), 2);
        assert_eq!(res.conflicts.len(), 1);
        assert_eq!(res.conflicts[0].route, c);
        assert!(
            res.conflicts[0].reason.contains("s/api"),
            "{}",
            res.conflicts[0].reason
        );
    }
}
