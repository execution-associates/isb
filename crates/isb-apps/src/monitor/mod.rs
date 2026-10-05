//! Uptime monitors: what users see of an org's apps, checked from outside
//! the app every interval, with history, incidents and notifications.
//!
//! A monitor is an HTTP(S) request (status range, keyword present or
//! absent, headers from secrets, redirects, a certificate expiry warning),
//! a TCP connect, or an **app** monitor that follows an app by reference:
//! its served domain's public URL, or with no domain the app's own
//! endpoint. When the domain sits behind Cloudflare Access without a
//! service token, it is checked hop by hop ([`chain`]): Cloudflare's edge,
//! the org's tunnel, and the ingress. A **service** monitor does the same for one service of a
//! compose stack. Every app with a served domain gets an app monitor of its
//! own (`app-<name>`, `auto`), and every compose stack service with one a
//! service monitor (`stack-<stack>-<service>`, `auto`, see [`auto`]), unless
//! the org opts out or excludes it.
//!
//! Checks run in the daemon that runs the org's apps ([`service`]); state,
//! thresholds and flap damping are in [`state`], the probes in [`probe`],
//! history in [`store`]. Channels hear `monitor.down`, `monitor.up` and
//! `monitor.cert_expiring` through the notifier like every other event.
//!
//! Definitions are kept in `<state>/orgs/<org>/monitors/monitors.json`, the
//! org's settings beside them in `settings.json`.

pub mod auto;
mod chain;
mod health_path;
pub mod heartbeat;
pub mod probe;
pub mod service;
pub mod state;
pub mod store;
mod target;
mod view;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::org::OrgId;
pub use service::Monitors;

/// Monitors per org at most.
pub const MAX_PER_ORG: usize = 200;
/// The shortest interval, seconds.
pub const MIN_INTERVAL: u64 = 30;
/// The prefix of an app's own monitor.
pub const AUTO_PREFIX: &str = "app-";
/// The prefix of a compose stack service's own monitor.
pub const STACK_PREFIX: &str = "stack-";
/// The org secrets an app monitor presents to Cloudflare Access, when both
/// exist and the monitor sets no Access headers of its own.
pub const ACCESS_ID_SECRET: &str = "CF_ACCESS_CLIENT_ID";
pub const ACCESS_SECRET_SECRET: &str = "CF_ACCESS_CLIENT_SECRET";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Http,
    Tcp,
    App,
    Service,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Http => "http",
            Kind::Tcp => "tcp",
            Kind::App => "app",
            Kind::Service => "service",
        }
    }
}

/// A request header: a plain `value`, or the value of org secret `secret`
/// (read at check time, never shown).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Header {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
}

/// One monitor. Fields that do not apply to its type are refused.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Monitor {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: Kind,
    /// http: the URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// tcp: host and port.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// app: the app, by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// service: the compose stack, by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
    /// service: the stack's service, by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    /// app, service: which of its domains (default: the first one served).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    /// app, service: the path to request (default: the path the service's healthcheck requests over HTTP, else the domain's path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// `GET` or `HEAD`.
    #[serde(default = "get")]
    pub method: String,
    /// Status codes that count as up: `200-399`, `200,204`, `200-299,301`.
    #[serde(default = "expected")]
    pub expected_status: String,
    /// The body must contain this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyword: Option<String>,
    /// The body must not contain this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyword_absent: Option<String>,
    #[serde(default)]
    pub follow_redirects: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<Header>,
    /// Seconds between checks (at least 30).
    #[serde(default = "interval")]
    pub interval: u64,
    /// Seconds one check may take.
    #[serde(default = "timeout")]
    pub timeout: u64,
    /// Failed checks in a row that make it down.
    #[serde(default = "two")]
    pub failure_threshold: u32,
    /// Successful checks in a row that make it up again.
    #[serde(default = "two")]
    pub recovery_threshold: u32,
    /// Warn this many days before an HTTPS certificate expires (0: never).
    #[serde(default = "cert_days")]
    pub cert_expiry_days: u32,
    #[serde(default)]
    pub paused: bool,
    /// Made for an app (`app-<name>`) or a compose stack service
    /// (`stack-<stack>-<service>`) with a served domain.
    #[serde(default)]
    pub auto: bool,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub updated_at: u64,
}

fn get() -> String {
    "GET".into()
}
fn expected() -> String {
    "200-399".into()
}
fn interval() -> u64 {
    60
}
fn timeout() -> u64 {
    10
}
fn two() -> u32 {
    2
}
fn cert_days() -> u32 {
    14
}

/// An org's monitoring settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// Give every app with a served domain a monitor of its own.
    #[serde(default = "yes")]
    pub auto_monitors: bool,
    /// Apps that get no monitor of their own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_apps: Vec<String>,
    /// Compose stack services that get no monitor of their own, as
    /// `<stack>/<service>`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_services: Vec<String>,
}

fn yes() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            auto_monitors: true,
            exclude_apps: Vec::new(),
            exclude_services: Vec::new(),
        }
    }
}

/// A monitor name: [a-z0-9-], a letter first, at most 63.
pub fn validate_name(n: &str) -> Result<()> {
    let ok = !n.is_empty()
        && n.len() <= 63
        && n.starts_with(|c: char| c.is_ascii_lowercase())
        && n.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "monitor name {n:?}: [a-z0-9-], starting with a letter, at most 63 characters"
        )))
    }
}

/// A compose service's name, as a service monitor refers to it.
pub fn validate_service_name(s: &str) -> Result<()> {
    let ok = !s.is_empty()
        && s.len() <= 63
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "service {s:?}: a compose service's name ([A-Za-z0-9._-], at most 63)"
        )))
    }
}

/// Parse `200-399`, `200,204,300-399` into inclusive ranges.
pub fn parse_status(s: &str) -> Result<Vec<(u16, u16)>> {
    let bad = || {
        Error::invalid(format!(
            "expected_status {s:?}: codes and ranges like 200-399 or 200,204"
        ))
    };
    let mut out = Vec::new();
    for part in s.split(',').map(str::trim) {
        let (a, b) = part.split_once('-').unwrap_or((part, part));
        let a: u16 = a.trim().parse().map_err(|_| bad())?;
        let b: u16 = b.trim().parse().map_err(|_| bad())?;
        if !(100..=599).contains(&a) || !(100..=599).contains(&b) || a > b {
            return Err(bad());
        }
        out.push((a, b));
    }
    if out.is_empty() || out.len() > 10 {
        return Err(bad());
    }
    Ok(out)
}

/// Headers isb sets itself, or that would change how the request is framed.
const RESERVED_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "upgrade",
];

fn validate_headers(hs: &[Header]) -> Result<()> {
    if hs.len() > 10 {
        return Err(Error::invalid("at most 10 headers"));
    }
    for h in hs {
        let n = &h.name;
        let token = !n.is_empty()
            && n.len() <= 64
            && n.chars()
                .all(|c| c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c));
        if !token || RESERVED_HEADERS.contains(&n.to_ascii_lowercase().as_str()) {
            return Err(Error::invalid(format!("header name {n:?}")));
        }
        match (&h.value, &h.secret) {
            (Some(v), None) if v.len() <= 1024 && !v.contains(['\r', '\n']) => {}
            (None, Some(s)) => crate::secrets::validate_name(s)?,
            _ => {
                return Err(Error::invalid(format!(
                    "header {n}: a value (without line breaks, at most 1024) or a secret, not both"
                )));
            }
        }
    }
    Ok(())
}

fn validate_tcp_host(h: &str) -> Result<()> {
    let ok = !h.is_empty()
        && h.len() <= 253
        && h.chars()
            .all(|c| c.is_ascii_alphanumeric() || ".-:[]".contains(c));
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "host {h:?} is not a host name or address"
        )))
    }
}

impl Monitor {
    /// A bare monitor of `kind` with every default.
    pub fn new(name: &str, kind: Kind) -> Monitor {
        serde_json::from_value(serde_json::json!({"name": name, "type": kind}))
            .expect("defaults deserialize")
    }

    /// Check everything but whether an app or secret exists.
    pub fn validate(&self) -> Result<()> {
        validate_name(&self.name)?;
        self.validate_target()?;
        if !(MIN_INTERVAL..=86_400).contains(&self.interval) {
            return Err(Error::invalid(format!(
                "interval: {MIN_INTERVAL} to 86400 seconds"
            )));
        }
        if !(1..=60).contains(&self.timeout) || self.timeout >= self.interval {
            return Err(Error::invalid(
                "timeout: 1 to 60 seconds, under the interval",
            ));
        }
        for (what, n) in [
            ("failure_threshold", self.failure_threshold),
            ("recovery_threshold", self.recovery_threshold),
        ] {
            if !(1..=10).contains(&n) {
                return Err(Error::invalid(format!("{what}: 1 to 10 checks")));
            }
        }
        if self.cert_expiry_days > 365 {
            return Err(Error::invalid("cert_expiry_days: 0 to 365"));
        }
        Ok(())
    }

    fn validate_target(&self) -> Result<()> {
        let http_only = self.keyword.is_some()
            || self.keyword_absent.is_some()
            || !self.headers.is_empty()
            || self.follow_redirects;
        match self.kind {
            Kind::Http => {
                let url = self
                    .url
                    .as_deref()
                    .ok_or_else(|| Error::invalid("an http monitor needs url"))?;
                crate::net::parse_url(url).map_err(|e| Error::invalid(format!("url: {e}")))?;
                self.refuse(&[
                    ("host", self.host.is_some()),
                    ("port", self.port.is_some()),
                    ("app", self.app.is_some()),
                    (
                        "stack or service",
                        self.stack.is_some() || self.service.is_some(),
                    ),
                    ("domain", self.domain.is_some()),
                    ("path", self.path.is_some()),
                ])?;
            }
            Kind::Tcp => {
                validate_tcp_host(self.host.as_deref().unwrap_or_default())?;
                if self.port.is_none_or(|p| p == 0) {
                    return Err(Error::invalid("a tcp monitor needs host and port"));
                }
                self.refuse(&[
                    ("url", self.url.is_some()),
                    ("app", self.app.is_some()),
                    (
                        "stack or service",
                        self.stack.is_some() || self.service.is_some(),
                    ),
                    ("domain", self.domain.is_some()),
                    ("path", self.path.is_some()),
                    ("keyword, headers or follow_redirects", http_only),
                ])?;
            }
            Kind::App => {
                let app = self.app.as_deref().unwrap_or_default();
                crate::app::validate_app_name(app)
                    .map_err(|_| Error::invalid(format!("app {app:?}: an app's name")))?;
                self.validate_path()?;
                self.refuse(&[
                    ("url", self.url.is_some()),
                    ("host", self.host.is_some()),
                    ("port", self.port.is_some()),
                    (
                        "stack or service",
                        self.stack.is_some() || self.service.is_some(),
                    ),
                ])?;
            }
            Kind::Service => {
                let stack = self.stack.as_deref().unwrap_or_default();
                crate::stack::validate_stack_name(stack)
                    .map_err(|_| Error::invalid(format!("stack {stack:?}: a stack's name")))?;
                validate_service_name(self.service.as_deref().unwrap_or_default())?;
                self.validate_path()?;
                self.refuse(&[
                    ("url", self.url.is_some()),
                    ("host", self.host.is_some()),
                    ("port", self.port.is_some()),
                    ("app", self.app.is_some()),
                ])?;
            }
        }
        if self.kind != Kind::Tcp {
            if !matches!(self.method.as_str(), "GET" | "HEAD") {
                return Err(Error::invalid("method: GET or HEAD"));
            }
            parse_status(&self.expected_status)?;
            for k in [&self.keyword, &self.keyword_absent].into_iter().flatten() {
                if k.is_empty() || k.len() > 200 {
                    return Err(Error::invalid("keywords: 1 to 200 characters"));
                }
            }
            validate_headers(&self.headers)?;
        }
        Ok(())
    }

    fn validate_path(&self) -> Result<()> {
        match &self.path {
            Some(p) if !p.starts_with('/') || p.len() > 1024 || p.contains(char::is_whitespace) => {
                Err(Error::invalid("path: starts with /, no spaces"))
            }
            _ => Ok(()),
        }
    }

    /// Does it follow something by reference (an app or a stack service)?
    pub fn follows(&self) -> bool {
        matches!(self.kind, Kind::App | Kind::Service)
    }

    fn refuse(&self, fields: &[(&str, bool)]) -> Result<()> {
        match fields.iter().find(|(_, set)| *set) {
            Some((f, _)) => Err(Error::invalid(format!(
                "{f} does not apply to a {} monitor",
                self.kind.as_str()
            ))),
            None => Ok(()),
        }
    }

    /// The secrets the monitor reads.
    pub fn secrets(&self) -> Vec<&str> {
        self.headers
            .iter()
            .filter_map(|h| h.secret.as_deref())
            .collect()
    }

    /// What it checks, in a few words.
    pub fn target(&self) -> String {
        match self.kind {
            Kind::Http => probe::display_url(self.url.as_deref().unwrap_or_default()),
            Kind::Tcp => format!(
                "{}:{}",
                self.host.as_deref().unwrap_or_default(),
                self.port.unwrap_or_default()
            ),
            Kind::App => format!("app {}", self.app.as_deref().unwrap_or_default()),
            Kind::Service => format!(
                "service {}/{}",
                self.stack.as_deref().unwrap_or_default(),
                self.service.as_deref().unwrap_or_default()
            ),
        }
    }
}

/// Where an org's monitors live.
pub fn dir(state: &Path, org: &OrgId) -> PathBuf {
    org.dir(state).join("monitors")
}

pub(crate) fn read_json<T: for<'de> Deserialize<'de> + Default>(p: &Path) -> Result<T> {
    match std::fs::read(p) {
        Ok(b) => {
            serde_json::from_slice(&b).map_err(|e| Error::invalid(format!("{}: {e}", p.display())))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e.into()),
    }
}

pub(crate) fn write_json<T: Serialize>(p: &Path, v: &T) -> Result<()> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(v)?)?;
    std::fs::rename(&tmp, p)?;
    Ok(())
}

/// A duration in words: `4m 12s`, `2h 5m`, `3d 1h`.
pub fn human(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m {}s", s / 60, s % 60),
        3600..86_400 => format!("{}h {}m", s / 3600, s % 3600 / 60),
        _ => format!("{}d {}h", s / 86_400, s % 86_400 / 3600),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn m(v: serde_json::Value) -> Result<Monitor> {
        let m: Monitor = serde_json::from_value(v).map_err(|e| Error::invalid(e.to_string()))?;
        m.validate()?;
        Ok(m)
    }

    #[test]
    fn defaults_and_validation() {
        let h =
            m(json!({"name": "shop", "type": "http", "url": "https://shop.example.com/"})).unwrap();
        assert_eq!(
            (
                h.interval,
                h.timeout,
                h.failure_threshold,
                h.recovery_threshold
            ),
            (60, 10, 2, 2)
        );
        assert_eq!(
            (
                h.method.as_str(),
                h.expected_status.as_str(),
                h.cert_expiry_days
            ),
            ("GET", "200-399", 14)
        );
        assert!(m(json!({"name": "x", "type": "http"})).is_err());
        assert!(m(json!({"name": "x", "type": "http", "url": "ftp://a"})).is_err());
        assert!(
            m(json!({"name": "x", "type": "http", "url": "https://a/", "interval": 10})).is_err()
        );
        assert!(m(json!({"name": "x", "type": "http", "url": "https://a/", "interval": 30, "timeout": 30})).is_err());
        assert!(m(json!({"name": "x", "type": "http", "url": "https://a/", "port": 80})).is_err());
        assert!(
            m(json!({"name": "x", "type": "http", "url": "https://a/", "method": "POST"})).is_err()
        );
        assert!(
            m(json!({"name": "x", "type": "http", "url": "https://a/", "failure_threshold": 0}))
                .is_err()
        );
        assert!(m(json!({"name": "X", "type": "http", "url": "https://a/"})).is_err());
        assert!(m(json!({"name": "x", "type": "http", "url": "https://a/", "bogus": 1})).is_err());
        m(json!({"name": "db", "type": "tcp", "host": "db.example.com", "port": 5432})).unwrap();
        assert!(m(json!({"name": "db", "type": "tcp", "host": "db.example.com"})).is_err());
        assert!(m(json!({"name": "db", "type": "tcp", "host": "a b", "port": 1})).is_err());
        assert!(
            m(json!({"name": "db", "type": "tcp", "host": "a", "port": 1, "keyword": "x"}))
                .is_err()
        );
        m(json!({"name": "web", "type": "app", "app": "web", "path": "/healthz"})).unwrap();
        assert!(m(json!({"name": "web", "type": "app", "app": "web", "path": "healthz"})).is_err());
        assert!(
            m(json!({"name": "web", "type": "app", "app": "web", "url": "https://a/"})).is_err()
        );
    }

    #[test]
    fn headers_and_status_ranges() {
        let ok = json!({"name": "a", "type": "http", "url": "https://a/", "headers": [
            {"name": "CF-Access-Client-Id", "secret": "CF_ID"}, {"name": "Accept", "value": "text/html"}]});
        assert_eq!(m(ok).unwrap().secrets(), vec!["CF_ID"]);
        for bad in [
            json!([{"name": "Host", "value": "x"}]),
            json!([{"name": "X", "value": "a\r\nb: c"}]),
            json!([{"name": "X", "value": "a", "secret": "S"}]),
            json!([{"name": "X"}]),
            json!([{"name": "Bad Name", "value": "a"}]),
        ] {
            assert!(
                m(json!({"name": "a", "type": "http", "url": "https://a/", "headers": bad}))
                    .is_err(),
                "{bad}"
            );
        }
        assert_eq!(parse_status("200-399").unwrap(), vec![(200, 399)]);
        assert_eq!(
            parse_status("200, 204,300-301").unwrap(),
            vec![(200, 200), (204, 204), (300, 301)]
        );
        for bad in ["", "abc", "399-200", "99", "200-700", "200,"] {
            assert!(parse_status(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn words() {
        assert_eq!(human(4_000), "4s");
        assert_eq!(human(252_000), "4m 12s");
        assert_eq!(human(7_500_000), "2h 5m");
        assert_eq!(human(266_400_000), "3d 2h");
        assert_eq!(Monitor::new("x", Kind::Tcp).interval, 60);
    }
}
