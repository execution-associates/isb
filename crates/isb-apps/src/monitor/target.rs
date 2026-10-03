//! Running one check: what a monitor's target is now (an app's, found by
//! reference), the probe, and whether its answer counts as up.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

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
    /// Why the check went where it did (an app behind Cloudflare Access).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
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

/// An app's endpoints right now.
struct AppView {
    /// The served domain's URL (with the monitor's path).
    public: Option<String>,
    /// The app's own endpoint and how it was found, or why there is none.
    internal: Result<(SocketAddr, String), String>,
    /// The domain's host, for the Host header on the internal endpoint.
    host: Option<String>,
    path: String,
}

fn loopback_for(ip: IpAddr) -> IpAddr {
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

fn app_view(ctx: &Ctx, org: &OrgId, m: &Monitor) -> Result<AppView, String> {
    let name = m.app.as_deref().unwrap_or_default();
    let app = ctx
        .apps
        .get(org, name)
        .map_err(|_| format!("app {name} does not exist"))?;
    let stack = app.spec.stack().map_err(|e| e.to_string())?;
    let st = ctx
        .ctl
        .status(&crate::stack::qualified(org, &stack))
        .map_err(|_| format!("app {name} is not deployed"))?;
    let svc = st
        .services
        .into_iter()
        .find(|s| s.service == name)
        .ok_or_else(|| format!("app {name} is not deployed"))?;
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
                .ok_or_else(|| format!("app {name} does not serve {h}"))?,
        ),
        None => served.first().copied(),
    };
    let path = m
        .path
        .clone()
        .or_else(|| pick.map(|d| d.path.clone()))
        .unwrap_or_else(|| "/".into());
    let public = pick
        .and_then(|d| d.url.as_deref())
        .map(|u| with_path(u, &path));
    let internal = if let Some(p) = svc.ports.first() {
        p.listen
            .parse::<SocketAddr>()
            .map(|a| {
                (
                    SocketAddr::new(loopback_for(a.ip()), a.port()),
                    format!("published port {}", p.listen),
                )
            })
            .map_err(|_| format!("published port {} is not an address", p.listen))
    } else {
        let port = app
            .spec
            .port
            .ok_or_else(|| format!("app {name} has no port to check"));
        let replica = svc
            .instances
            .iter()
            .find(|i| i.in_rotation)
            .and_then(|i| Some((i.name.clone(), i.ip.as_deref()?.parse::<IpAddr>().ok()?)));
        match (port, replica) {
            (Err(e), _) => Err(e),
            (Ok(_), None) => Err(format!("app {name} has no replica in rotation")),
            (Ok(p), Some((inst, ip))) => Ok((SocketAddr::new(ip, p), format!("replica {inst}"))),
        }
    };
    Ok(AppView {
        public,
        internal,
        host: pick.map(|d| d.host.clone()),
        path,
    })
}

/// The monitor's headers, secrets read now; for app monitors, the org's
/// Access service token when it sets no Access headers itself.
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
    if m.kind == Kind::App && !has_access {
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

/// Judge an HTTP answer by the monitor's expectations.
pub fn judge(m: &Monitor, a: &HttpAnswer) -> Result<(), String> {
    let ranges = parse_status(&m.expected_status).map_err(|e| e.to_string())?;
    if !ranges
        .iter()
        .any(|(lo, hi)| (*lo..=*hi).contains(&a.status))
    {
        let mut e = format!("HTTP {} (expected {})", a.status, m.expected_status);
        if access_redirect(a) {
            e.push_str(": redirected to Cloudflare Access sign-in");
        }
        return Err(e);
    }
    if access_redirect(a) {
        return Err(format!(
            "HTTP {}: redirected to Cloudflare Access sign-in; give the monitor a service token (headers CF-Access-Client-Id and CF-Access-Client-Secret from secrets)",
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
        Kind::App => run_app(ctx, org, m, at),
    }
}

fn run_app(ctx: &Ctx, org: &OrgId, m: &Monitor, at: u64) -> Outcome {
    let view = match app_view(ctx, org, m) {
        Ok(v) => v,
        Err(e) => return Outcome::fail(at, e),
    };
    let hs = match headers(ctx, org, m) {
        Ok(h) => h,
        Err(e) => return Outcome::fail(at, e),
    };
    let mut note = None;
    if let Some(url) = &view.public {
        let o = http_outcome(m, &probe_for(ctx, m, url.clone(), hs.clone()), at, "public");
        let refused = o
            .error
            .as_deref()
            .is_some_and(|e| e.starts_with("refusing "));
        let access = o
            .error
            .as_deref()
            .is_some_and(|e| e.contains("Cloudflare Access"));
        if !refused && !access {
            return o;
        }
        // Users see the app through its domain, but this daemon may not
        // check it there: say why, and check the app's own endpoint.
        note = Some(if access {
            "the domain is behind Cloudflare Access: checked the app's own endpoint instead (add CF_ACCESS_CLIENT_ID and CF_ACCESS_CLIENT_SECRET secrets to check the public URL)".to_string()
        } else {
            "the domain resolves to a private address: checked the app's own endpoint instead (a platform admin can allow private targets)".to_string()
        });
    }
    let (addr, what) = match view.internal {
        Ok(x) => x,
        Err(e) => {
            return Outcome {
                note,
                ..Outcome::fail(at, e)
            };
        }
    };
    let host = view.host.unwrap_or_else(|| addr.ip().to_string());
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
    let mut o = http_outcome(m, &p, at, &format!("internal: {what}"));
    o.note = note;
    o
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
