//! Running one check: what a monitor's target is now (an app's or a stack
//! service's, found by reference), the probe, and whether its answer counts
//! as up.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::chain::{self, Hop};
use super::health_path;
use super::probe::{self, HttpAnswer, HttpProbe};
use super::{ACCESS_ID_SECRET, ACCESS_SECRET_SECRET, Kind, Monitor, parse_status};
use crate::app::Apps;
use crate::org::OrgId;
use crate::secrets::Secrets;
use crate::stack::Controller;

/// One check's result.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    /// Unix milliseconds.
    pub at: u64,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// What was checked (a URL without its query, or host:port).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// `public`, or how an app's own endpoint was reached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    /// The HTTPS certificate's expiry, unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cert_expires: Option<u64>,
    /// Why the check went where it did (a domain behind Cloudflare Access).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// A hop-by-hop check's hops ([`chain`]), in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hops: Vec<Hop>,
}

impl Outcome {
    fn fail(at: u64, error: String) -> Outcome {
        Outcome {
            at,
            error: Some(error),
            ..Default::default()
        }
    }
}

/// What a check needs from the daemon.
pub(crate) struct Ctx<'a> {
    pub apps: &'a Apps,
    pub ctl: &'a Controller,
    pub secrets: &'a Secrets,
    pub allow_private: bool,
    pub tls: Arc<rustls::ClientConfig>,
}

/// An app's or stack service's endpoints right now.
struct AppView {
    /// The served domain's URL (with the path to check), unless the domain
    /// cannot reach that path.
    public: Option<String>,
    /// Its own endpoint and how it was found, or why there is none.
    internal: Result<(SocketAddr, String), String>,
    /// The domain's host, for the Host header on the internal endpoint.
    host: Option<String>,
    /// The path on its own endpoint: what the ingress hands the replica.
    path: String,
    /// Where the path came from, or why only its own endpoint is checked.
    note: Option<String>,
    /// The ingress listener the domain's requests come in on.
    origin: Option<String>,
    /// The domain comes in through the org's Cloudflare tunnel.
    tunnel: bool,
    /// The domain is served over HTTPS (at Cloudflare, for a tunnel).
    https: bool,
}

pub(crate) fn loopback_for(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(v) if v.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(v) if v.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    }
}

/// Replace a URL's path (and query) with `path`.
fn with_path(url: &str, path: &str) -> String {
    let after = url.find("://").map(|i| i + 3).unwrap_or(0);
    let end = url[after..]
        .find('/')
        .map(|i| after + i)
        .unwrap_or(url.len());
    format!("{}{path}", &url[..end])
}

/// What an `app` or `service` monitor follows: a service of a stack.
struct Followed {
    /// The qualified stack.
    stack: String,
    service: String,
    /// `app web`, `service wiki/web`: for messages.
    what: String,
    /// `app` or `service`.
    noun: &'static str,
    /// Where its replicas listen: an app's port. A stack service's is the
    /// one its domain routes to.
    port: Option<Result<u16, String>>,
}

fn followed(apps: &Apps, org: &OrgId, m: &Monitor) -> Result<Followed, String> {
    if m.kind == Kind::Service {
        let (st, sv) = (
            m.stack.clone().unwrap_or_default(),
            m.service.clone().unwrap_or_default(),
        );
        return Ok(Followed {
            stack: crate::stack::qualified(org, &st),
            what: format!("service {st}/{sv}"),
            service: sv,
            noun: "service",
            port: None,
        });
    }
    let name = m.app.as_deref().unwrap_or_default();
    let app = apps
        .get(org, name)
        .map_err(|_| format!("app {name} does not exist"))?;
    let stack = app.spec.stack().map_err(|e| e.to_string())?;
    Ok(Followed {
        stack: crate::stack::qualified(org, &stack),
        service: name.to_string(),
        what: format!("app {name}"),
        noun: "app",
        port: Some(
            app.spec
                .port
                .ok_or_else(|| format!("app {name} has no port to check")),
        ),
    })
}

/// Does the app or stack service have a deployment that is live: a healthy
/// replica in rotation? Until it does, its monitor has nothing to check.
pub(crate) fn is_live(apps: &Apps, org: &OrgId, m: &Monitor) -> bool {
    let Ok(f) = followed(apps, org, m) else {
        return false;
    };
    apps.controller()
        .status(&f.stack)
        .ok()
        .and_then(|st| st.services.into_iter().find(|s| s.service == f.service))
        .is_some_and(|s| s.healthy > 0 && s.instances.iter().any(|i| i.in_rotation))
}

/// The paths a check requests, from the monitor's `path`, the service's
/// healthcheck and the picked domain's route (its prefix, whether it strips
/// it, the port it routes to).
fn view_paths(
    ctx: &Ctx,
    m: &Monitor,
    f: &Followed,
    pick: Option<&crate::ingress::DomainStatus>,
) -> health_path::Paths {
    let def = ctx.ctl.definition(&f.stack).ok();
    let spec = def.as_ref().and_then(|d| d.file.services.get(&f.service));
    let route = pick
        .zip(spec)
        .and_then(|(d, s)| health_path::spec_of(&s.domains, &d.host, &d.path));
    let domain = pick.map_or("/", |d| d.path.as_str());
    let strip = route.is_some_and(|r| r.strip_prefix);
    // The port the domain's requests reach, or the app's own.
    let port = route.and_then(|r| r.port).or_else(|| {
        pick.and_then(|d| d.upstreams.first())
            .and_then(|u| u.parse::<SocketAddr>().ok())
            .map(|a| a.port())
            .or_else(|| f.port.as_ref().and_then(|p| p.as_ref().ok().copied()))
    });
    let health = match (&m.path, spec.and_then(|s| s.healthcheck.as_ref()), port) {
        (None, Some(h), Some(port)) => health_path::from_healthcheck(&h.test, port),
        _ => None,
    };
    health_path::choose(m.path.as_deref(), domain, strip, health.as_deref(), f.noun)
}

fn app_view(ctx: &Ctx, org: &OrgId, m: &Monitor) -> Result<(AppView, &'static str), String> {
    let f = followed(ctx.apps, org, m)?;
    let what = &f.what;
    let st = ctx
        .ctl
        .status(&f.stack)
        .map_err(|_| format!("{what} is not deployed"))?;
    let svc = st
        .services
        .into_iter()
        .find(|s| s.service == f.service)
        .ok_or_else(|| format!("{what} is not deployed"))?;
    let served: Vec<_> = svc
        .domains
        .iter()
        .filter(|d| d.url.is_some() && matches!(d.state.as_str(), "serving" | "no-replicas"))
        .collect();
    let pick = match &m.domain {
        Some(h) => Some(
            *served
                .iter()
                .find(|d| d.host == *h)
                .ok_or_else(|| format!("{what} does not serve {h}"))?,
        ),
        None => served.first().copied(),
    };
    let paths = view_paths(ctx, m, &f, pick);
    let public = pick
        .and_then(|d| d.url.as_deref())
        .zip(paths.public.as_deref())
        .map(|(u, p)| with_path(u, p));
    let replica = |ip: IpAddr| {
        svc.instances
            .iter()
            .find(|i| i.in_rotation && i.ip.as_deref() == Some(ip.to_string().as_str()))
            .map(|i| i.name.clone())
    };
    let internal = match (&f.port, svc.ports.first()) {
        (Some(_), Some(p)) => p
            .listen
            .parse::<SocketAddr>()
            .map(|a| {
                (
                    SocketAddr::new(loopback_for(a.ip()), a.port()),
                    format!("published port {}", p.listen),
                )
            })
            .map_err(|_| format!("published port {} is not an address", p.listen)),
        (Some(port), None) => {
            let r = svc
                .instances
                .iter()
                .find(|i| i.in_rotation)
                .and_then(|i| Some((i.name.clone(), i.ip.as_deref()?.parse::<IpAddr>().ok()?)));
            match (port, r) {
                (Err(e), _) => Err(e.clone()),
                (Ok(_), None) => Err(format!("{what} has no replica in rotation")),
                (Ok(p), Some((inst, ip))) => {
                    Ok((SocketAddr::new(ip, *p), format!("replica {inst}")))
                }
            }
        }
        // A stack service: where the ingress sends its domain's requests.
        (None, _) => pick
            .or_else(|| served.first().copied())
            .and_then(|d| d.upstreams.first())
            .and_then(|u| u.parse::<SocketAddr>().ok())
            .map(|a| match replica(a.ip()) {
                Some(inst) => (a, format!("replica {inst}")),
                None => (a, format!("upstream {a}")),
            })
            .ok_or_else(|| format!("{what} has no replica in rotation")),
    };
    Ok((
        AppView {
            public,
            internal,
            host: pick.map(|d| d.host.clone()),
            path: paths.internal,
            note: paths.note,
            origin: pick.and_then(|d| d.origin.clone()),
            tunnel: pick.is_some_and(|d| d.provider == crate::org::INGRESS_CLOUDFLARE_TUNNEL),
            https: pick.is_some_and(|d| d.https),
        },
        f.noun,
    ))
}

/// The monitor's headers, secrets read now; for app and service monitors,
/// the org's Access service token when it sets no Access headers itself.
fn headers(ctx: &Ctx, org: &OrgId, m: &Monitor) -> Result<Vec<(String, String)>, String> {
    let read = |s: &str| -> Result<String, String> {
        let (v, _) = ctx.secrets.get(org, s).map_err(|e| {
            if e.is_not_found() {
                format!("secret {s} is gone")
            } else {
                format!("secret {s}: {e}")
            }
        })?;
        String::from_utf8(v)
            .map(|v| v.trim().to_string())
            .map_err(|_| format!("secret {s} is not UTF-8 text"))
    };
    let mut out = Vec::new();
    for h in &m.headers {
        let v = match (&h.value, &h.secret) {
            (Some(v), _) => v.clone(),
            (None, Some(s)) => read(s)?,
            (None, None) => continue,
        };
        out.push((h.name.clone(), v));
    }
    let has_access = out
        .iter()
        .any(|(k, _)| k.to_ascii_lowercase().starts_with("cf-access-client"));
    if m.follows() && !has_access {
        if let (Ok(id), Ok(secret)) = (read(ACCESS_ID_SECRET), read(ACCESS_SECRET_SECRET)) {
            out.push(("CF-Access-Client-Id".into(), id));
            out.push(("CF-Access-Client-Secret".into(), secret));
        }
    }
    Ok(out)
}

/// Did an answer send us to Cloudflare Access' sign-in?
pub fn access_redirect(a: &HttpAnswer) -> bool {
    (300..400).contains(&a.status)
        && a.location.as_deref().is_some_and(|l| {
            crate::net::parse_url(l).is_ok_and(|t| t.host.ends_with(".cloudflareaccess.com"))
        })
}

/// Did Cloudflare Access stop the request, by redirect or by refusal?
fn access_stopped(a: &HttpAnswer) -> Option<&'static str> {
    if access_redirect(a) {
        Some("redirected to Cloudflare Access sign-in")
    } else if a.access_refused {
        Some("refused by Cloudflare Access")
    } else {
        None
    }
}

/// Judge an HTTP answer by the monitor's expectations.
pub fn judge(m: &Monitor, a: &HttpAnswer) -> Result<(), String> {
    let ranges = parse_status(&m.expected_status).map_err(|e| e.to_string())?;
    if !ranges
        .iter()
        .any(|(lo, hi)| (*lo..=*hi).contains(&a.status))
    {
        let mut e = format!("HTTP {} (expected {})", a.status, m.expected_status);
        if let Some(why) = access_stopped(a) {
            e.push_str(&format!(": {why}"));
        }
        return Err(e);
    }
    if let Some(why) = access_stopped(a) {
        return Err(format!(
            "HTTP {}: {why}; give the monitor a service token (headers CF-Access-Client-Id and CF-Access-Client-Secret from secrets)",
            a.status
        ));
    }
    let body = String::from_utf8_lossy(&a.body);
    if let Some(k) = &m.keyword {
        if !body.contains(k.as_str()) {
            return Err(format!(
                "HTTP {}: the body does not contain {k:?}",
                a.status
            ));
        }
    }
    if let Some(k) = &m.keyword_absent {
        if body.contains(k.as_str()) {
            return Err(format!("HTTP {}: the body contains {k:?}", a.status));
        }
    }
    Ok(())
}

fn http_outcome(m: &Monitor, p: &HttpProbe, at: u64, via: &str) -> Outcome {
    let mut o = match probe::http(p) {
        Ok(a) => {
            let r = judge(m, &a);
            Outcome {
                at,
                ok: r.is_ok(),
                latency_ms: Some(a.latency.as_millis() as u64),
                status: Some(a.status),
                error: r.err(),
                url: Some(a.final_url.clone()),
                cert_expires: a.cert_expires,
                ..Default::default()
            }
        }
        Err(e) => Outcome {
            url: Some(probe::display_url(&p.url)),
            ..Outcome::fail(at, e)
        },
    };
    o.via = Some(via.into());
    o
}

fn probe_for(ctx: &Ctx, m: &Monitor, url: String, hs: Vec<(String, String)>) -> HttpProbe {
    HttpProbe {
        url,
        method: m.method.clone(),
        headers: hs,
        timeout: Duration::from_secs(m.timeout),
        follow_redirects: m.follow_redirects,
        allow_private: ctx.allow_private,
        connect_to: None,
        tls: ctx.tls.clone(),
    }
}

/// Run one check of `m` at `at` (unix milliseconds).
pub(crate) fn run(ctx: &Ctx, org: &OrgId, m: &Monitor, at: u64) -> Outcome {
    match m.kind {
        Kind::Tcp => {
            let (h, p) = (m.host.as_deref().unwrap_or_default(), m.port.unwrap_or(0));
            let r = probe::tcp(h, p, Duration::from_secs(m.timeout), ctx.allow_private);
            Outcome {
                ok: r.is_ok(),
                latency_ms: r.as_ref().ok().map(|d| d.as_millis() as u64),
                error: r.err(),
                url: Some(format!("{h}:{p}")),
                ..Outcome::fail(at, String::new())
            }
        }
        Kind::Http => match headers(ctx, org, m) {
            Ok(hs) => http_outcome(
                m,
                &probe_for(ctx, m, m.url.clone().unwrap_or_default(), hs),
                at,
                "public",
            ),
            Err(e) => Outcome::fail(at, e),
        },
        Kind::App | Kind::Service => run_app(ctx, org, m, at),
    }
}

fn run_app(ctx: &Ctx, org: &OrgId, m: &Monitor, at: u64) -> Outcome {
    let (view, noun) = match app_view(ctx, org, m) {
        Ok(v) => v,
        Err(e) => return Outcome::fail(at, e),
    };
    let hs = match headers(ctx, org, m) {
        Ok(h) => h,
        Err(e) => return Outcome::fail(at, e),
    };
    let note = view.note.clone();
    let Some(url) = &view.public else {
        return Outcome {
            note,
            ..replica(ctx, m, &view, hs, at)
        };
    };
    let o = http_outcome(m, &probe_for(ctx, m, url.clone(), hs.clone()), at, "public");
    let err = o.error.clone().unwrap_or_default();
    let refused = err.starts_with("refusing ");
    let access = err.contains("Cloudflare Access");
    if !refused && !access {
        return Outcome { note, ..o };
    }
    let status = o.status.unwrap_or_default();
    if access && has_access_token(&hs) {
        // The token went with the request, and Access still stopped it.
        let error = format!(
            "HTTP {status}: {} with the service token: allow it in the Access application's policy",
            access_why(&err)
        );
        return Outcome {
            note,
            error: Some(error),
            ..o
        };
    }
    // Users see it through its domain, but this daemon may not check it
    // there: check every hop it can see instead.
    let edge =
        access.then(|| Hop::new("edge", true, format!("HTTP {status}: {}", access_why(&err))));
    let c = Chain {
        view: &view,
        noun,
        edge,
        note,
    };
    hop_by_hop(ctx, org, m, hs, at, c)
}

fn has_access_token(hs: &[(String, String)]) -> bool {
    hs.iter()
        .any(|(k, _)| k.to_ascii_lowercase().starts_with("cf-access-client"))
}

/// How Access stopped a request, from the judged error.
fn access_why(err: &str) -> &'static str {
    if err.contains("refused by Cloudflare Access") {
        "refused by Cloudflare Access"
    } else {
        "redirected to Cloudflare Access sign-in"
    }
}

/// What a hop-by-hop check starts from.
struct Chain<'a> {
    view: &'a AppView,
    noun: &'static str,
    /// Access' answer at the edge; `None` when the domain resolved to an
    /// address the policy refuses.
    edge: Option<Hop>,
    note: Option<String>,
}

fn hop_by_hop(
    ctx: &Ctx,
    org: &OrgId,
    m: &Monitor,
    mut hs: Vec<(String, String)>,
    at: u64,
    c: Chain,
) -> Outcome {
    let view = c.view;
    let behind_access = c.edge.is_some();
    let mut hops: Vec<Hop> = c.edge.into_iter().collect();
    if view.tunnel {
        hops.push(chain::tunnel(ctx, org, Duration::from_secs(m.timeout)));
    }
    // The public path: the ingress strips a route's prefix itself.
    let path = view
        .public
        .as_deref()
        .and_then(|u| crate::net::parse_url(u).ok())
        .map(|t| t.path);
    let target = view.origin.as_deref().zip(view.host.as_deref()).zip(path);
    let last = match target {
        Some(((origin, host), path)) => match chain::via_origin(origin, host, &path) {
            Ok((url, addr)) => {
                // What cloudflared tells the ingress of a visitor's HTTPS.
                if view.tunnel && view.https && url.starts_with("http://") {
                    hs.push(("X-Forwarded-Proto".into(), "https".into()));
                }
                let mut p = probe_for(ctx, m, url, hs);
                p.connect_to = Some(addr);
                p.follow_redirects = false;
                let o = http_outcome(m, &p, at, &format!("ingress {origin}"));
                hops.push(Hop::of("ingress", &o, origin));
                o
            }
            Err(e) => {
                hops.push(Hop::new("ingress", false, e.clone()));
                Outcome::fail(at, e)
            }
        },
        None => {
            let o = replica(ctx, m, view, hs, at);
            let via = o.via.clone().unwrap_or_default();
            hops.push(Hop::of("replica", &o, via.trim_start_matches("internal: ")));
            o
        }
    };
    let names = chain::labels(&hops);
    let mut why = if behind_access {
        format!(
            "checked hop by hop ({names}); the Access policy is not verified: add CF_ACCESS_CLIENT_ID and CF_ACCESS_CLIENT_SECRET secrets for an end-to-end check"
        )
    } else {
        format!(
            "the domain resolves to a private address: checked hop by hop ({names}) (a platform admin can allow private targets)"
        )
    };
    if hops.iter().any(|h| h.hop == "replica") {
        why.push_str(&format!(
            "; the ingress has no listener address for the domain, so the {}'s own endpoint stands in for it",
            c.noun
        ));
    }
    let note = Some(match c.note {
        Some(n) => format!("{n}; {why}"),
        None => why,
    });
    Outcome {
        note,
        ..chain::finish(hops, last)
    }
}

/// The app's own endpoint, the ingress bypassed.
fn replica(ctx: &Ctx, m: &Monitor, view: &AppView, hs: Vec<(String, String)>, at: u64) -> Outcome {
    let (addr, what) = match &view.internal {
        Ok(x) => x.clone(),
        Err(e) => return Outcome::fail(at, e.clone()),
    };
    let host = view.host.clone().unwrap_or_else(|| addr.ip().to_string());
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host
    };
    let mut p = probe_for(
        ctx,
        m,
        format!("http://{host}:{}{}", addr.port(), view.path),
        hs,
    );
    p.connect_to = Some(addr);
    p.follow_redirects = false;
    http_outcome(m, &p, at, &format!("internal: {what}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ans(status: u16, location: Option<&str>, body: &str) -> HttpAnswer {
        HttpAnswer {
            status,
            location: location.map(String::from),
            body: body.as_bytes().to_vec(),
            ..Default::default()
        }
    }

    #[test]
    fn judging_answers() {
        let mut m = Monitor::new("x", Kind::Http);
        assert!(judge(&m, &ans(200, None, "")).is_ok());
        assert!(judge(&m, &ans(301, Some("/a"), "")).is_ok());
        let e = judge(&m, &ans(503, None, "")).unwrap_err();
        assert_eq!(e, "HTTP 503 (expected 200-399)");
        let access = ans(
            302,
            Some("https://team.cloudflareaccess.com/cdn-cgi/access/login/x"),
            "",
        );
        assert!(
            judge(&m, &access)
                .unwrap_err()
                .contains("Cloudflare Access")
        );
        // Access refusing outright: its 403 page, and its OAuth 401.
        let refused = HttpAnswer {
            access_refused: true,
            ..ans(403, None, "")
        };
        assert_eq!(
            judge(&m, &refused).unwrap_err(),
            "HTTP 403 (expected 200-399): refused by Cloudflare Access"
        );
        assert!(
            judge(&m, &ans(403, None, ""))
                .unwrap_err()
                .ends_with("200-399)")
        );
        m.expected_status = "200".into();
        assert!(
            judge(&m, &access)
                .unwrap_err()
                .contains("Cloudflare Access")
        );
        assert!(judge(&m, &ans(204, None, "")).is_err());
        m.keyword = Some("Hostname".into());
        assert!(judge(&m, &ans(200, None, "Hostname: web-1")).is_ok());
        assert!(
            judge(&m, &ans(200, None, "nope"))
                .unwrap_err()
                .contains("does not contain")
        );
        m.keyword = None;
        m.keyword_absent = Some("error".into());
        assert!(
            judge(&m, &ans(200, None, "an error page"))
                .unwrap_err()
                .contains("contains")
        );
    }

    #[test]
    fn paths_and_addresses() {
        assert_eq!(
            with_path("https://a.example.com/x?y", "/healthz"),
            "https://a.example.com/healthz"
        );
        assert_eq!(with_path("http://a:8080", "/"), "http://a:8080/");
        assert_eq!(
            loopback_for("0.0.0.0".parse().unwrap()),
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        );
        assert_eq!(
            loopback_for("10.1.2.3".parse().unwrap()).to_string(),
            "10.1.2.3"
        );
    }
}
