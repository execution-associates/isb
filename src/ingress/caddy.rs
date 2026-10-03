//! Caddy as the HTTP(S) edge: its JSON config, its binary, and the process.
//!
//! - **Config** is generated whole from the routes ([`render`]) and pushed
//!   with `POST /load` on every change. Caddy swaps it gracefully: requests
//!   in flight on the old config finish (up to [`GRACE`]) before `/load`
//!   answers.
//! - **Binary**: a pinned release, downloaded from GitHub and checked
//!   against the SHA-512 compiled in here (not the release's own checksum
//!   file, which would only prove the download matched itself), or a path
//!   the operator gives.
//! - **Process**: a child of `isb serve`, restarted with backoff, killed
//!   with the daemon (SIGTERM on shutdown, and the kernel's parent-death
//!   signal if the daemon dies without one). It is never adopted: a new
//!   daemon starts a new Caddy, which finds its certificates in storage.
//! - **Admin API** on a unix socket inside a 0700 directory, never TCP.
//! - **Logs**: Caddy writes JSON to stderr; certificate events are picked
//!   out of it ([`LogEvent`]), warnings and errors go to the journal.

use std::io::{BufRead, Read, Write};
use std::net::SocketAddr;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};

use super::domain::{Route, redirect_keeps_path};
use crate::error::{Error, Result};

/// The Caddy release isb runs.
pub const VERSION: &str = "2.11.6";

/// SHA-512 of `caddy_<VERSION>_linux_<arch>.tar.gz`, from the release's
/// checksums file, checked when this version was pinned.
const SHA512: &[(&str, &str)] = &[
    (
        "amd64",
        "422771007d505ea97efd1177a4905b2c1a471cd426668f2ace3bcda3d8e30b11f9b1610bfb02c6ad60f2a795f56124f2f5eec6409c17d5a0dd4c21a11375fb94",
    ),
    (
        "arm64",
        "bd228ea44b6b95720a0c2d7b62886e99cb4a0b05356fe6e058c4a155618c913377b18e686839ae7fcc6c2c05aea2d549fa91f25aa3ef8d43cdead117259577ed",
    ),
];

const RELEASES: &str = "https://github.com/caddyserver/caddy/releases/download";

/// How long a config change waits for requests on the old config.
pub const GRACE: &str = "10s";

/// How a public certificate is issued.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ca {
    /// ACME at this directory URL.
    Acme(String),
    /// Caddy's own local CA (tests, private networks).
    Internal,
}

pub const LETSENCRYPT: &str = "https://acme-v02.api.letsencrypt.org/directory";
pub const LETSENCRYPT_STAGING: &str = "https://acme-staging-v02.api.letsencrypt.org/directory";

impl Ca {
    /// `letsencrypt` (default), `letsencrypt-staging`, `internal`, or an ACME
    /// directory URL.
    pub fn parse(s: &str) -> Result<Ca> {
        Ok(match s.trim() {
            "" | "letsencrypt" => Ca::Acme(LETSENCRYPT.into()),
            "letsencrypt-staging" | "staging" => Ca::Acme(LETSENCRYPT_STAGING.into()),
            "internal" => Ca::Internal,
            u if u.starts_with("https://") => Ca::Acme(u.into()),
            other => {
                return Err(Error::invalid(format!(
                    "--acme-ca {other:?}: letsencrypt, letsencrypt-staging, internal, or an https:// ACME directory URL"
                )));
            }
        })
    }

    /// The directory Caddy keeps this issuer's certificates under in its
    /// storage (`certificates/<key>/<host>/`), and the `issuer` its logs name.
    pub fn issuer_key(&self) -> String {
        match self {
            Ca::Internal => "local".into(),
            Ca::Acme(url) => {
                let rest = url
                    .strip_prefix("https://")
                    .or_else(|| url.strip_prefix("http://"))
                    .unwrap_or(url);
                rest.trim_end_matches('/').replace('/', "-")
            }
        }
    }
}

/// Where a route is served from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Via {
    /// The public listeners (and Caddy's certificates).
    Public,
    /// The org's tunnel listener: plain HTTP from its cloudflared, which
    /// terminates TLS at Cloudflare.
    Tunnel(crate::org::OrgId),
}

/// A route and the replicas behind it.
#[derive(Debug, Clone)]
pub struct Served {
    pub route: Route,
    /// The in-rotation replicas' `ip:port`.
    pub upstreams: Vec<SocketAddr>,
    pub via: Via,
}

/// An org's tunnel listener: on its bridge's own address, reachable only
/// from inside the org.
#[derive(Debug, Clone)]
pub struct TunnelListener {
    pub org: crate::org::OrgId,
    pub listen: SocketAddr,
    /// The org's subnet, whose `X-Forwarded-*` headers (its cloudflared's)
    /// are trusted.
    pub subnet: String,
}

/// Everything [`render`] needs besides the routes.
#[derive(Debug, Clone)]
pub struct Params {
    pub admin_socket: PathBuf,
    pub storage: PathBuf,
    pub http: Option<SocketAddr>,
    pub https: Option<SocketAddr>,
    pub ca: Ca,
    pub email: Option<String>,
}

/// Hosts that cannot get a certificate from an ACME CA over HTTP or
/// TLS-ALPN challenges.
pub fn needs_dns_challenge(host: &str, ca: &Ca) -> bool {
    host.starts_with("*.") && matches!(ca, Ca::Acme(_))
}

/// Caddy's whole config.
#[allow(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
pub fn render(p: &Params, served: &[Served], tunnels: &[TunnelListener]) -> Value {
    let mut servers = serde_json::Map::new();
    let public: Vec<&Served> = served.iter().filter(|s| s.via == Via::Public).collect();
    let https_port = p.https.map(|a| a.port());

    let mut tls_hosts: Vec<String> = Vec::new();
    let mut skip: Vec<String> = Vec::new();
    if let Some(addr) = p.https {
        let list: Vec<&Served> = public.iter().copied().filter(|s| s.route.https).collect();
        let routes: Vec<Value> = sorted(&list).into_iter().map(proxy_route).collect();
        for s in &list {
            let h = &s.route.host;
            if needs_dns_challenge(h, &p.ca) {
                if !skip.contains(h) {
                    skip.push(h.clone());
                }
            } else if !tls_hosts.contains(h) {
                tls_hosts.push(h.clone());
            }
        }
        let mut auto = json!({"disable_redirects": true});
        if !skip.is_empty() {
            auto["skip"] = json!(skip);
        }
        servers.insert(
            "public-https".into(),
            json!({"listen": [addr.to_string()], "routes": routes, "automatic_https": auto}),
        );
    }
    if let Some(addr) = p.http {
        let mut routes = Vec::new();
        // HTTPS hosts: everything goes to https, once per host. ACME HTTP
        // challenges are answered by Caddy before any route.
        if https_port.is_some() {
            let mut hosts: Vec<&str> = public
                .iter()
                .filter(|s| s.route.https)
                .map(|s| s.route.host.as_str())
                .collect();
            hosts.sort_by(|a, b| (a.starts_with("*."), *a).cmp(&(b.starts_with("*."), *b)));
            hosts.dedup();
            let port = match https_port {
                Some(443) | None => String::new(),
                Some(p) => format!(":{p}"),
            };
            for h in hosts {
                routes.push(json!({
                    "match": [{"host": [h]}],
                    "handle": [{
                        "handler": "static_response",
                        "status_code": 308,
                        "headers": {"Location": [format!("https://{{http.request.host}}{port}{{http.request.uri}}")]},
                    }],
                    "terminal": true,
                }));
            }
        }
        let plain: Vec<&Served> = public.iter().copied().filter(|s| !s.route.https).collect();
        routes.extend(sorted(&plain).into_iter().map(proxy_route));
        servers.insert(
            "public-http".into(),
            json!({"listen": [addr.to_string()], "routes": routes}),
        );
    }
    for t in tunnels {
        let mine: Vec<&Served> = served
            .iter()
            .filter(|s| s.via == Via::Tunnel(t.org.clone()))
            .collect();
        let mut routes = Vec::new();
        for s in sorted(&mine) {
            if s.route.https && s.route.redirect.is_none() {
                // cloudflared says how the visitor came in.
                let mut m = matcher(&s.route);
                m["header"] = json!({"X-Forwarded-Proto": ["http"]});
                routes.push(json!({
                    "match": [m],
                    "handle": [{
                        "handler": "static_response",
                        "status_code": 308,
                        "headers": {"Location": ["https://{http.request.host}{http.request.uri}"]},
                    }],
                    "terminal": true,
                }));
            }
            routes.push(proxy_route(s));
        }
        servers.insert(
            format!("tunnel-{}", t.org),
            json!({
                "listen": [t.listen.to_string()],
                "routes": routes,
                "automatic_https": {"disable": true},
                "trusted_proxies": {"source": "static", "ranges": [t.subnet]},
            }),
        );
    }

    let mut http = json!({"servers": servers, "grace_period": GRACE});
    if let Some(a) = p.http {
        http["http_port"] = json!(a.port());
    }
    if let Some(a) = p.https {
        http["https_port"] = json!(a.port());
    }
    let issuer = match &p.ca {
        Ca::Internal => json!({"module": "internal"}),
        Ca::Acme(url) => {
            let mut i = json!({"module": "acme", "ca": url});
            if let Some(e) = &p.email {
                i["email"] = json!(e);
            }
            i
        }
    };
    let mut apps = json!({"http": http});
    if !tls_hosts.is_empty() {
        apps["tls"] =
            json!({"automation": {"policies": [{"subjects": tls_hosts, "issuers": [issuer]}]}});
    }
    // Never touch the host's trust stores (Caddy would, with sudo).
    apps["pki"] = json!({"certificate_authorities": {"local": {"install_trust": false}}});
    json!({
        "admin": {
            "listen": format!("unix/{}", p.admin_socket.display()),
            "config": {"persist": false},
        },
        "logging": {"logs": {"default": {
            "writer": {"output": "stderr"},
            "encoder": {"format": "json"},
            "level": "INFO",
        }}},
        "storage": {"module": "file_system", "root": p.storage.display().to_string()},
        "apps": apps,
    })
}

/// Concrete hosts before wildcards (Caddy tries routes in order), then by
/// host, longest path first.
fn sorted<'a>(list: &[&'a Served]) -> Vec<&'a Served> {
    let mut v = list.to_vec();
    v.sort_by(|a, b| {
        let ka = (
            a.route.is_wildcard(),
            &a.route.host,
            std::cmp::Reverse(a.route.path.len()),
            &a.route.path,
        );
        let kb = (
            b.route.is_wildcard(),
            &b.route.host,
            std::cmp::Reverse(b.route.path.len()),
            &b.route.path,
        );
        ka.cmp(&kb)
    });
    v
}

fn matcher(r: &Route) -> Value {
    let mut m = json!({"host": [r.host]});
    if r.path != "/" {
        m["path"] = json!([r.path, format!("{}/*", r.path)]);
    }
    m
}

fn proxy_route(s: &Served) -> Value {
    json!({"match": [matcher(&s.route)], "handle": handlers(s), "terminal": true})
}

fn handlers(s: &Served) -> Vec<Value> {
    let r = &s.route;
    if let Some(target) = &r.redirect {
        let loc = if redirect_keeps_path(target) {
            format!("{}{{http.request.uri}}", target.trim_end_matches('/'))
        } else {
            target.clone()
        };
        return vec![json!({
            "handler": "static_response",
            "status_code": 308,
            "headers": {"Location": [loc]},
        })];
    }
    if s.upstreams.is_empty() {
        return vec![json!({
            "handler": "static_response",
            "status_code": 503,
            "headers": {"Retry-After": ["5"], "Content-Type": ["text/plain; charset=utf-8"]},
            "body": format!("no replica of {} is serving\n", r.service),
        })];
    }
    let mut ups: Vec<String> = s.upstreams.iter().map(|a| a.to_string()).collect();
    ups.sort();
    ups.dedup();
    let mut out = Vec::new();
    if r.strip_prefix && r.path != "/" {
        out.push(json!({"handler": "rewrite", "strip_path_prefix": r.path}));
    }
    out.push(json!({
        "handler": "reverse_proxy",
        "upstreams": ups.iter().map(|u| json!({"dial": u})).collect::<Vec<_>>(),
        "load_balancing": {
            "selection_policy": {"policy": "least_conn"},
            // A replica that refuses (stopping, restarting) costs the client
            // nothing while another is up.
            "retries": 2,
            "try_duration": "5s",
            "try_interval": "250ms",
        },
        "health_checks": {"passive": {"fail_duration": "10s", "max_fails": 1}},
    }));
    out
}

/// The tarball's arch name, or `None` where Caddy publishes none we pin.
pub fn arch() -> Option<&'static str> {
    match std::env::consts::ARCH {
        "x86_64" => Some("amd64"),
        "aarch64" => Some("arm64"),
        _ => None,
    }
}

/// The Caddy binary to run: `configured`, or the pinned release under
/// `<dir>/caddy-<VERSION>`, downloaded and verified when missing.
pub fn ensure_binary(dir: &Path, configured: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = configured {
        if !p.is_file() {
            return Err(Error::invalid(format!(
                "--caddy-bin {}: no such file",
                p.display()
            )));
        }
        return Ok(p.to_path_buf());
    }
    let dst = dir.join(format!("caddy-{VERSION}"));
    if dst.is_file() {
        return Ok(dst);
    }
    if !cfg!(target_os = "linux") {
        return Err(Error::invalid(
            "the ingress runs on Linux (on macOS, inside the isb machine)",
        ));
    }
    let arch = arch().ok_or_else(|| {
        Error::invalid(format!(
            "no pinned Caddy for {}; pass --caddy-bin",
            std::env::consts::ARCH
        ))
    })?;
    let want = SHA512
        .iter()
        .find(|(a, _)| *a == arch)
        .map(|(_, h)| *h)
        .unwrap_or_default();
    std::fs::create_dir_all(dir)?;
    let asset = format!("caddy_{VERSION}_linux_{arch}.tar.gz");
    let url = format!("{RELEASES}/v{VERSION}/{asset}");
    eprintln!("isb serve: downloading Caddy {VERSION} ({url})");
    let tarball = crate::machine::fetch(&url, 128 << 20)?;
    let got = crate::machine::hex(ring::digest::digest(&ring::digest::SHA512, &tarball).as_ref());
    if !got.eq_ignore_ascii_case(want) {
        return Err(Error::invalid(format!(
            "{asset}: sha512 {got} is not the pinned {want}"
        )));
    }
    let tmp = tempdir_in(dir)?;
    let tgz = tmp.join(&asset);
    std::fs::write(&tgz, &tarball)?;
    let out = Command::new("tar")
        .arg("-xzf")
        .arg(&tgz)
        .arg("-C")
        .arg(&tmp)
        .arg("caddy")
        .stdin(Stdio::null())
        .output()?;
    if !out.status.success() {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(Error::OperationFailed {
            step: format!("unpack {asset}"),
            message: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(tmp.join("caddy"), std::fs::Permissions::from_mode(0o755))?;
    std::fs::rename(tmp.join("caddy"), &dst)?;
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(dst)
}

fn tempdir_in(dir: &Path) -> Result<PathBuf> {
    let p = dir.join(format!(".caddy-dl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p)?;
    Ok(p)
}

/// One request to Caddy's admin API. HTTP/1.0, so Go answers without
/// chunking and closes at the end.
pub fn admin(
    socket: &Path,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    timeout: Duration,
) -> Result<(u16, Vec<u8>)> {
    let what = format!("caddy admin {method} {path}");
    let io = |e: std::io::Error| Error::OperationFailed {
        step: what.clone(),
        message: e.to_string(),
    };
    let mut s = UnixStream::connect(socket).map_err(io)?;
    s.set_read_timeout(Some(timeout)).map_err(io)?;
    s.set_write_timeout(Some(timeout)).map_err(io)?;
    let body = body.unwrap_or_default();
    let head = format!(
        "{method} {path} HTTP/1.0\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    s.write_all(head.as_bytes()).map_err(io)?;
    s.write_all(body).map_err(io)?;
    let started = Instant::now();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 16384];
    loop {
        if started.elapsed() > timeout {
            return Err(io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("no answer within {timeout:?}"),
            )));
        }
        match s.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(io(e)),
        }
        if buf.len() > 16 << 20 {
            return Err(Error::Protocol(format!("{what}: response too large")));
        }
    }
    let mut hs = [httparse::EMPTY_HEADER; 64];
    let mut r = httparse::Response::new(&mut hs);
    let n = match r.parse(&buf) {
        Ok(httparse::Status::Complete(n)) => n,
        _ => return Err(Error::Protocol(format!("{what}: bad response"))),
    };
    let code = r.code.unwrap_or(0);
    Ok((code, buf.split_off(n)))
}

/// A certificate event from Caddy's log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogEvent {
    Obtained { host: String, issuer: String },
    Failed { host: String, error: String },
}

/// Pick a certificate event out of one log line.
pub fn parse_log(line: &str) -> Option<LogEvent> {
    let v: Value = serde_json::from_str(line).ok()?;
    let logger = v["logger"].as_str().unwrap_or_default();
    if !logger.starts_with("tls") {
        return None;
    }
    let host = v["identifier"].as_str()?.to_string();
    let msg = v["msg"].as_str().unwrap_or_default();
    match msg {
        "certificate obtained successfully" => Some(LogEvent::Obtained {
            host,
            issuer: v["issuer"].as_str().unwrap_or_default().to_string(),
        }),
        "could not get certificate from issuer" | "will retry" | "obtaining certificate failed" => {
            Some(LogEvent::Failed {
                host,
                error: v["error"].as_str().unwrap_or(msg).to_string(),
            })
        }
        _ => None,
    }
}

/// Whether a certificate for `host` from `issuer_key` is in Caddy's storage.
pub fn cert_in_storage(storage: &Path, issuer_key: &str, host: &str) -> bool {
    let name = host.replace('*', "wildcard_");
    storage
        .join("certificates")
        .join(issuer_key)
        .join(&name)
        .join(format!("{name}.crt"))
        .is_file()
}

/// What `ingress_status` says about the Caddy process.
#[derive(Debug, Clone, Default, Serialize)]
pub struct EdgeStatus {
    pub version: String,
    pub binary: String,
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub restarts: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Unix seconds of the last config Caddy accepted.
    pub loaded_at: u64,
}

/// Called with each certificate event.
pub type OnLog = Arc<dyn Fn(LogEvent) + Send + Sync>;

/// The supervised Caddy process.
pub struct Edge {
    bin: PathBuf,
    config_file: PathBuf,
    admin_socket: PathBuf,
    desired: Mutex<Value>,
    child: Mutex<Option<Child>>,
    stop: AtomicBool,
    status: Mutex<EdgeStatus>,
    on_log: OnLog,
    /// Serialises loads, so an older config never lands after a newer one.
    loading: Mutex<()>,
}

const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// How long a fresh Caddy may take to open its admin socket.
const START_TIMEOUT: Duration = Duration::from_secs(20);
/// A `/load` waits for the old config's requests (GRACE) and then some.
const LOAD_TIMEOUT: Duration = Duration::from_secs(20);

impl Edge {
    /// Start supervising Caddy with `initial` as its config. `dir` holds the
    /// config file; the admin socket must be in a directory only we can open.
    pub fn start(
        bin: PathBuf,
        dir: &Path,
        admin_socket: PathBuf,
        initial: Value,
        on_log: OnLog,
    ) -> Result<Arc<Edge>> {
        std::fs::create_dir_all(dir)?;
        if let Some(parent) = admin_socket.parent() {
            std::fs::create_dir_all(parent)?;
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
        let edge = Arc::new(Edge {
            bin: bin.clone(),
            config_file: dir.join("caddy.json"),
            admin_socket,
            desired: Mutex::new(initial),
            child: Mutex::new(None),
            stop: AtomicBool::new(false),
            status: Mutex::new(EdgeStatus {
                version: VERSION.into(),
                binary: bin.display().to_string(),
                ..Default::default()
            }),
            on_log,
            loading: Mutex::new(()),
        });
        let e = edge.clone();
        std::thread::Builder::new()
            .name("isb-caddy".into())
            .spawn(move || e.supervise())?;
        Ok(edge)
    }

    pub fn status(&self) -> EdgeStatus {
        self.status.lock().unwrap().clone()
    }

    fn write_config(&self, cfg: &Value) -> Result<()> {
        let tmp = self.config_file.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(cfg)?)?;
        std::fs::rename(&tmp, &self.config_file)?;
        Ok(())
    }

    /// Make `cfg` Caddy's config: now if it runs, at its next start if not.
    pub fn load(&self, cfg: Value) -> Result<()> {
        let _g = self.loading.lock().unwrap();
        *self.desired.lock().unwrap() = cfg.clone();
        self.write_config(&cfg)?;
        if !self.status.lock().unwrap().running {
            return Ok(());
        }
        self.post_load(&cfg)
    }

    fn post_load(&self, cfg: &Value) -> Result<()> {
        let body = serde_json::to_vec(cfg)?;
        let (code, resp) = admin(
            &self.admin_socket,
            "POST",
            "/load",
            Some(&body),
            LOAD_TIMEOUT,
        )?;
        if code != 200 {
            let msg = serde_json::from_slice::<Value>(&resp)
                .ok()
                .and_then(|v| v["error"].as_str().map(String::from))
                .unwrap_or_else(|| String::from_utf8_lossy(&resp).trim().to_string());
            let e = format!("caddy refused the config (HTTP {code}): {msg}");
            self.status.lock().unwrap().last_error = Some(e.clone());
            return Err(Error::OperationFailed {
                step: "load ingress config".into(),
                message: e,
            });
        }
        let mut st = self.status.lock().unwrap();
        st.loaded_at = crate::stack::now_secs();
        st.last_error = None;
        Ok(())
    }

    /// The proxies' upstreams with requests in flight: `(address, requests)`.
    pub fn upstreams(&self) -> Result<Vec<(String, u64)>> {
        let (code, body) = admin(
            &self.admin_socket,
            "GET",
            "/reverse_proxy/upstreams",
            None,
            Duration::from_secs(3),
        )?;
        if code != 200 {
            return Err(Error::Protocol(format!(
                "caddy /reverse_proxy/upstreams: HTTP {code}"
            )));
        }
        let v: Value = serde_json::from_slice(&body)?;
        Ok(v.as_array()
            .into_iter()
            .flatten()
            .map(|u| {
                (
                    u["address"].as_str().unwrap_or_default().to_string(),
                    u["num_requests"].as_u64().unwrap_or(0),
                )
            })
            .collect())
    }

    /// Stop Caddy and the supervisor.
    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(mut c) = self.child.lock().unwrap().take() {
            terminate(&mut c);
        }
    }

    fn supervise(self: Arc<Self>) {
        let mut backoff = BACKOFF_MIN;
        while !self.stop.load(Ordering::SeqCst) {
            let started = Instant::now();
            match self.run_once() {
                Ok(()) => {}
                Err(e) => {
                    eprintln!("isb serve: caddy: {e}");
                    self.status.lock().unwrap().last_error = Some(e.to_string());
                }
            }
            {
                let mut st = self.status.lock().unwrap();
                st.running = false;
                st.pid = None;
            }
            if self.stop.load(Ordering::SeqCst) {
                return;
            }
            if started.elapsed() > Duration::from_secs(60) {
                backoff = BACKOFF_MIN;
            }
            self.status.lock().unwrap().restarts += 1;
            eprintln!("isb serve: caddy: restarting in {backoff:?}");
            let until = Instant::now() + backoff;
            while Instant::now() < until && !self.stop.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(100));
            }
            backoff = (backoff * 2).min(BACKOFF_MAX);
        }
    }

    /// Run Caddy until it exits (or we are stopped).
    #[allow(
        clippy::too_many_lines,
        reason = "predates the lint ratchet; split it when next changed"
    )]
    fn run_once(&self) -> Result<()> {
        let cfg = self.desired.lock().unwrap().clone();
        self.write_config(&cfg)?;
        let _ = std::fs::remove_file(&self.admin_socket);
        let mut cmd = Command::new(&self.bin);
        cmd.arg("run")
            .arg("--config")
            .arg(&self.config_file)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            // Its home for anything it caches outside our storage.
            .env(
                "XDG_DATA_HOME",
                self.config_file.parent().unwrap_or(Path::new(".")),
            )
            .env(
                "XDG_CONFIG_HOME",
                self.config_file.parent().unwrap_or(Path::new(".")),
            );
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::process::CommandExt;
            // SAFETY: prctl is async-signal-safe; nothing else runs here.
            unsafe {
                cmd.pre_exec(|| {
                    rustix::process::set_parent_process_death_signal(Some(
                        rustix::process::Signal::TERM,
                    ))
                    .map_err(std::io::Error::from)
                });
            }
        }
        let mut child = cmd.spawn().map_err(|e| Error::OperationFailed {
            step: format!("start {}", self.bin.display()),
            message: e.to_string(),
        })?;
        let pid = child.id();
        if let Some(err) = child.stderr.take() {
            let on_log = self.on_log.clone();
            let _ = std::thread::Builder::new()
                .name("isb-caddy-log".into())
                .spawn(move || read_log(err, &*on_log));
        }
        *self.child.lock().unwrap() = Some(child);
        {
            let mut st = self.status.lock().unwrap();
            st.pid = Some(pid);
        }
        // Up once the admin socket answers.
        let deadline = Instant::now() + START_TIMEOUT;
        let mut up = false;
        while Instant::now() < deadline && !self.stop.load(Ordering::SeqCst) {
            if self.exited()? {
                break;
            }
            if admin(
                &self.admin_socket,
                "GET",
                "/config/admin",
                None,
                Duration::from_secs(2),
            )
            .is_ok_and(|(c, _)| c == 200)
            {
                up = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if up {
            {
                let mut st = self.status.lock().unwrap();
                st.running = true;
                st.loaded_at = crate::stack::now_secs();
            }
            eprintln!("isb serve: caddy {VERSION} running (pid {pid})");
            // A load that came while it was starting went only to the file.
            let _g = self.loading.lock().unwrap();
            let latest = self.desired.lock().unwrap().clone();
            if latest != cfg {
                let _ = self.post_load(&latest);
            }
        } else if !self.stop.load(Ordering::SeqCst) {
            let exited = self.exited().unwrap_or(true);
            if let Some(mut c) = self.child.lock().unwrap().take() {
                terminate(&mut c);
            }
            if exited {
                return Err(Error::OperationFailed {
                    step: "start caddy".into(),
                    message: "it exited (see the lines above)".into(),
                });
            }
            return Err(Error::OperationFailed {
                step: "start caddy".into(),
                message: format!(
                    "admin socket {} did not answer within {START_TIMEOUT:?} (see the lines above)",
                    self.admin_socket.display()
                ),
            });
        }
        // Wait for it to exit.
        loop {
            if self.stop.load(Ordering::SeqCst) {
                return Ok(());
            }
            if self.exited()? {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let code = self
            .child
            .lock()
            .unwrap()
            .take()
            .and_then(|mut c| c.wait().ok())
            .map(|s| s.to_string())
            .unwrap_or_default();
        Err(Error::OperationFailed {
            step: "caddy".into(),
            message: format!("exited ({code})"),
        })
    }

    fn exited(&self) -> Result<bool> {
        let mut g = self.child.lock().unwrap();
        match g.as_mut() {
            None => Ok(true),
            Some(c) => Ok(c.try_wait()?.is_some()),
        }
    }
}

/// SIGTERM, then SIGKILL after Caddy's grace period and a margin.
fn terminate(c: &mut Child) {
    if let Some(pid) = rustix::process::Pid::from_raw(c.id() as i32) {
        let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if let Ok(Some(_)) = c.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = c.kill();
    let _ = c.wait();
}

fn read_log(err: std::process::ChildStderr, on_log: &(dyn Fn(LogEvent) + Send + Sync)) {
    let r = std::io::BufReader::new(err);
    for line in r.lines() {
        let Ok(line) = line else { break };
        if let Some(ev) = parse_log(&line) {
            on_log(ev);
        }
        // Warnings and errors to the journal; info only for certificates.
        let v: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
        let level = v["level"].as_str().unwrap_or("info");
        let logger = v["logger"].as_str().unwrap_or_default();
        let msg = v["msg"].as_str().unwrap_or(&line);
        // Per-request upstream errors, and what every config load repeats.
        let noisy = logger == "http.log.error"
            || logger == "admin"
            || msg.contains("listening only on the HTTP port")
            || msg.contains("skipped because it requires TLS")
            || msg.starts_with("exiting");
        if (matches!(level, "warn" | "error" | "fatal" | "panic") && !noisy)
            || logger.starts_with("tls.obtain")
        {
            let mut extra = String::new();
            for k in ["identifier", "error", "address"] {
                if let Some(s) = v[k].as_str() {
                    extra.push_str(&format!(" {k}={s}"));
                }
            }
            eprintln!("isb serve: caddy: {level}: {logger}: {msg}{extra}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::org::OrgId;

    fn route(
        org: &str,
        svc: &str,
        host: &str,
        path: &str,
        port: Option<u16>,
        https: bool,
    ) -> Route {
        Route {
            org: OrgId::new(org).unwrap(),
            stack: "shop".into(),
            service: svc.into(),
            host: host.into(),
            path: path.into(),
            port,
            https,
            redirect: None,
            strip_prefix: false,
            generated: false,
            auto: false,
        }
    }

    fn params(ca: Ca) -> Params {
        Params {
            admin_socket: "/run/isb/ingress/admin.sock".into(),
            storage: "/var/lib/isb/ingress/caddy".into(),
            http: Some("0.0.0.0:80".parse().unwrap()),
            https: Some("0.0.0.0:443".parse().unwrap()),
            ca,
            email: Some("ops@example.com".into()),
        }
    }

    fn up(s: &[&str]) -> Vec<SocketAddr> {
        s.iter().map(|a| a.parse().unwrap()).collect()
    }

    /// The whole config for a representative set of routes, compared with
    /// testdata/caddy-golden.json (regenerate with ISB_BLESS=1).
    #[test]
    fn golden_config() {
        let mut api = route("acme", "api", "app.example.com", "/api", Some(3000), true);
        api.strip_prefix = true;
        let mut www = route("acme", "web", "www.app.example.com", "/", None, true);
        www.redirect = Some("https://app.example.com".into());
        www.generated = true;
        let served = vec![
            Served {
                route: route("acme", "web", "app.example.com", "/", Some(8080), true),
                upstreams: up(&["10.64.3.18:8080", "10.64.3.17:8080"]),
                via: Via::Public,
            },
            Served {
                route: api,
                upstreams: up(&["10.64.3.20:3000"]),
                via: Via::Public,
            },
            Served {
                route: www,
                upstreams: vec![],
                via: Via::Public,
            },
            Served {
                route: route("acme", "docs", "docs.example.com", "/", Some(80), false),
                upstreams: vec![],
                via: Via::Public,
            },
            Served {
                route: route("acme", "any", "*.apps.example.com", "/", Some(80), true),
                upstreams: up(&["10.64.3.30:80"]),
                via: Via::Public,
            },
            Served {
                route: route("beta", "web", "beta.example.org", "/", Some(80), true),
                upstreams: up(&["10.70.1.5:80"]),
                via: Via::Tunnel(OrgId::new("beta").unwrap()),
            },
        ];
        let tunnels = vec![TunnelListener {
            org: OrgId::new("beta").unwrap(),
            listen: "10.70.1.1:8480".parse().unwrap(),
            subnet: "10.70.1.0/24".into(),
        }];
        let got = render(
            &params(Ca::Acme(LETSENCRYPT_STAGING.into())),
            &served,
            &tunnels,
        );
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ingress/testdata/caddy-golden.json");
        let text = serde_json::to_string_pretty(&got).unwrap() + "\n";
        if std::env::var_os("ISB_BLESS").is_some() {
            std::fs::write(&path, &text).unwrap();
        }
        let want = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text, want,
            "regenerate with ISB_BLESS=1 and review the diff"
        );
    }

    #[test]
    fn plain_http_only_and_internal_ca() {
        let mut p = params(Ca::Internal);
        p.http = Some("127.0.0.1:18080".parse().unwrap());
        p.https = Some("127.0.0.1:18443".parse().unwrap());
        let served = vec![
            Served {
                route: route("acme", "web", "a.test.example", "/", Some(80), true),
                upstreams: up(&["10.0.0.2:80"]),
                via: Via::Public,
            },
            Served {
                route: route("acme", "web", "*.w.example", "/", Some(80), true),
                upstreams: up(&["10.0.0.2:80"]),
                via: Via::Public,
            },
        ];
        let v = render(&p, &served, &[]);
        assert_eq!(v["apps"]["http"]["https_port"], 18443);
        assert_eq!(
            v["apps"]["tls"]["automation"]["policies"][0]["issuers"][0]["module"],
            "internal"
        );
        // Internal CA can issue wildcards: nothing skipped.
        assert!(v["apps"]["http"]["servers"]["public-https"]["automatic_https"]["skip"].is_null());
        assert_eq!(
            v["apps"]["http"]["servers"]["public-http"]["routes"][0]["handle"][0]["headers"]["Location"]
                [0],
            "https://{http.request.host}:18443{http.request.uri}"
        );
        assert_eq!(
            v["apps"]["pki"]["certificate_authorities"]["local"]["install_trust"],
            false
        );
        // No https listener: no https server, no redirects.
        p.https = None;
        let v = render(&p, &served, &[]);
        assert!(v["apps"]["http"]["servers"]["public-https"].is_null());
        assert_eq!(
            v["apps"]["http"]["servers"]["public-http"]["routes"],
            json!([])
        );
    }

    #[test]
    fn ca_names() {
        assert_eq!(Ca::parse("").unwrap(), Ca::Acme(LETSENCRYPT.into()));
        assert_eq!(
            Ca::parse("letsencrypt-staging").unwrap().issuer_key(),
            "acme-staging-v02.api.letsencrypt.org-directory"
        );
        assert_eq!(
            Ca::parse("letsencrypt").unwrap().issuer_key(),
            "acme-v02.api.letsencrypt.org-directory"
        );
        assert_eq!(Ca::Internal.issuer_key(), "local");
        assert!(Ca::parse("http://x").is_err());
    }

    #[test]
    fn log_events() {
        assert_eq!(
            parse_log(
                r#"{"level":"info","logger":"tls.obtain","msg":"certificate obtained successfully","identifier":"a.test","issuer":"local"}"#
            ),
            Some(LogEvent::Obtained {
                host: "a.test".into(),
                issuer: "local".into()
            })
        );
        assert_eq!(
            parse_log(
                r#"{"level":"error","logger":"tls.obtain","msg":"could not get certificate from issuer","identifier":"a.test","error":"HTTP 429"}"#
            ),
            Some(LogEvent::Failed {
                host: "a.test".into(),
                error: "HTTP 429".into()
            })
        );
        assert_eq!(
            parse_log(r#"{"level":"info","logger":"http","msg":"server running"}"#),
            None
        );
        assert_eq!(parse_log("not json"), None);
    }

    #[test]
    fn storage_lookup() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("certificates/local/wildcard_.x.example");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("wildcard_.x.example.crt"), "x").unwrap();
        assert!(cert_in_storage(d.path(), "local", "*.x.example"));
        assert!(!cert_in_storage(d.path(), "local", "a.x.example"));
    }
}
