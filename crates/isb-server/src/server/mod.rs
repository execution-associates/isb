//! `isb serve`: the MCP server layer, with the tools supplied by the embedder.
//!
//! Security model:
//! - TCP listeners bind loopback only. Remote clients arrive through a
//!   cloudflared tunnel, behind a Cloudflare Access application with Managed
//!   OAuth; Cloudflare runs the OAuth flow and isb stays the resource origin.
//! - With Access configured, every `/mcp` request must carry a valid
//!   `Cf-Access-Jwt-Assertion` for the application's audience, so a request
//!   reaching the port by another route is still refused.
//! - A TCP listener without Access is refused unless the embedder opts in, and
//!   then only accepts browser requests whose `Origin` is localhost, which
//!   blocks DNS rebinding.
//! - The unix socket (0600, in a 0700 directory) is the trusted local path:
//!   filesystem permissions are the gate, and its callers are
//!   [`Caller::Local`], which [`Caller::is_trusted`] reports.
//! - `/healthz` never requires auth and reveals only what the embedder puts in it.
//! - A listener can carry extra [`Routes`] (`isb serve` mounts the identity
//!   endpoints, `/api/v1/auth/*`, this way). They authenticate their own
//!   callers; with Access configured they sit behind it, as `/mcp` does.
//! - [`Listener::public_routes`] are served ahead of Access, for requests
//!   that carry their own credential (app webhooks, signed by the sender).

pub mod access;
pub use isb_core::serve_client as client;
#[cfg(test)]
mod client_tests;
pub mod http;
pub mod mcp;
pub mod service;
pub mod ssh;
pub mod ssh_config;
pub mod tailnet;
pub mod terminal;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

pub use access::{AccessValidator, Identity};
pub use http::Shutdown;
pub use mcp::{Authenticated, Caller, Hooks, Registry, Tool, ToolHandler, ToolPolicy};

use crate::error::{Error, Result};
use http::{Handler, HttpListener, HttpServer, Limits};
use mcp::Endpoint;

/// Health for `GET /healthz`: `(ok, body)`. 200 when ok, else 503. Called on
/// every probe, so it must be cheap.
pub type Healthz = Arc<dyn Fn() -> (bool, Value) + Send + Sync>;

/// Extra routes on a listener: `Some` answers the request, `None` leaves it
/// to the server (a 404).
pub type Routes = Arc<dyn Fn(&http::Request) -> Option<http::Response> + Send + Sync>;

#[derive(Clone)]
pub enum ListenerKind {
    /// `host:port`, which must resolve to loopback only.
    Tcp(String),
    Unix(PathBuf),
    /// `host:port` on any address, TLS with required client certificates:
    /// an agent's listener for its control plane (docs/guides/servers.md).
    Mtls(String, http::TlsConfig),
}

impl std::fmt::Debug for ListenerKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ListenerKind::Tcp(a) => write!(f, "Tcp({a:?})"),
            ListenerKind::Unix(p) => write!(f, "Unix({p:?})"),
            ListenerKind::Mtls(a, _) => write!(f, "Mtls({a:?})"),
        }
    }
}

/// One address the server answers on, with its own gate and tool policy.
#[derive(Clone)]
pub struct Listener {
    pub kind: ListenerKind,
    /// Required on TCP unless `allow_unauthenticated`; refused on unix.
    pub access: Option<Arc<AccessValidator>>,
    pub policy: ToolPolicy,
    /// Serve TCP with no Access validation, trusting whatever reaches the port.
    pub allow_unauthenticated: bool,
    /// A TCP listener on a tailnet address rather than loopback.
    pub tailnet: bool,
    /// Paths other than `/healthz` and `/mcp`.
    pub routes: Option<Routes>,
    /// Routes that authenticate every request themselves and are served
    /// even with Access configured (webhooks, signed by their sender).
    pub public_routes: Option<Routes>,
    /// Authentication and authorization the embedder supplies.
    pub hooks: mcp::Hooks,
}

impl std::fmt::Debug for Listener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Listener")
            .field("kind", &self.kind)
            .field("access", &self.access)
            .field("policy", &self.policy)
            .field("allow_unauthenticated", &self.allow_unauthenticated)
            .field("routes", &self.routes.is_some())
            .finish()
    }
}

impl Listener {
    pub fn tcp(addr: impl Into<String>) -> Self {
        Self::new(ListenerKind::Tcp(addr.into()))
    }

    pub fn unix(path: impl Into<PathBuf>) -> Self {
        Self::new(ListenerKind::Unix(path.into()))
    }

    /// TLS on any address, admitting only clients whose certificate `tls`
    /// verifies.
    pub fn mtls(addr: impl Into<String>, tls: http::TlsConfig) -> Self {
        Self::new(ListenerKind::Mtls(addr.into(), tls))
    }

    fn new(kind: ListenerKind) -> Self {
        Listener {
            kind,
            access: None,
            policy: ToolPolicy::default(),
            allow_unauthenticated: false,
            tailnet: false,
            routes: None,
            public_routes: None,
            hooks: mcp::Hooks::default(),
        }
    }

    /// Serve `routes` on this listener too.
    pub fn hooks(mut self, h: mcp::Hooks) -> Self {
        self.hooks = h;
        self
    }

    pub fn routes(mut self, r: Routes) -> Self {
        self.routes = Some(r);
        self
    }

    /// Serve `r` ahead of Access: only for routes whose every request
    /// carries its own credential.
    pub fn public_routes(mut self, r: Routes) -> Self {
        self.public_routes = Some(r);
        self
    }

    pub fn access(mut self, v: AccessValidator) -> Self {
        self.access = Some(Arc::new(v));
        self
    }

    /// [`Self::access`], sharing a validator (and its key cache).
    pub fn access_shared(mut self, v: Arc<AccessValidator>) -> Self {
        self.access = Some(v);
        self
    }

    /// Bind a tailnet address instead of loopback (no Access: only tailnet
    /// peers reach it).
    pub fn tailnet(mut self, yes: bool) -> Self {
        self.tailnet = yes;
        self
    }

    pub fn policy(mut self, p: ToolPolicy) -> Self {
        self.policy = p;
        self
    }

    pub fn allow_unauthenticated(mut self, yes: bool) -> Self {
        self.allow_unauthenticated = yes;
        self
    }

    /// The unix socket is trusted as itself; a TCP caller is trusted only
    /// as a superadmin, per request.
    pub fn is_trusted(&self) -> bool {
        matches!(self.kind, ListenerKind::Unix(_))
    }

    fn check(&self) -> Result<()> {
        match (&self.kind, &self.access) {
            (ListenerKind::Unix(p), Some(_)) => Err(Error::invalid(format!(
                "unix socket {}: Cloudflare Access applies to TCP listeners only",
                p.display()
            ))),
            (ListenerKind::Mtls(a, _), Some(_)) => Err(Error::invalid(format!(
                "{a}: Cloudflare Access does not apply to an mTLS listener"
            ))),
            (ListenerKind::Tcp(a), Some(_)) if self.tailnet => Err(Error::invalid(format!(
                "{a}: Cloudflare Access applies to loopback listeners (behind the tunnel), not a tailnet one"
            ))),
            (ListenerKind::Tcp(a), None) if !self.allow_unauthenticated => {
                Err(Error::invalid(format!(
                    "TCP listener {a} needs Cloudflare Access (team domain and audience), \
                     or an explicit opt-in to serve it unauthenticated"
                )))
            }
            _ => Ok(()),
        }
    }

    fn describe(&self, tools: usize) -> String {
        match (&self.kind, &self.access) {
            (ListenerKind::Unix(p), _) => {
                format!("unix:{} (trusted local, {tools} tools)", p.display())
            }
            (ListenerKind::Mtls(a, _), _) => {
                format!("https://{a} (control plane mTLS only, {tools} tools)")
            }
            (ListenerKind::Tcp(a), Some(v)) => format!(
                "http://{a}/mcp (Cloudflare Access: {}, {tools} tools)",
                v.issuer()
            ),
            (ListenerKind::Tcp(a), None) if self.tailnet => format!(
                "http://{a}/mcp on the tailnet ({tools} tools): callers sign in with isb API tokens \
                 or sessions, or are superadmins by tailnet identity"
            ),
            (ListenerKind::Tcp(a), None) if self.hooks.authorize.is_some() => format!(
                "http://{a}/mcp ({tools} tools) without Cloudflare Access: callers sign in \
                 with isb API tokens or sessions"
            ),
            (ListenerKind::Tcp(a), None) => format!(
                "http://{a}/mcp ({tools} tools) WITHOUT Cloudflare Access: anything that \
                 reaches this port can call these tools"
            ),
        }
    }
}

/// Where the CLI and the server meet: `$ISB_SERVE_SOCKET`, else
/// `$XDG_RUNTIME_DIR/isb/serve.sock`, else a per-uid directory under /tmp.
/// On macOS, where the daemon runs inside the `isb machine`, it is that
/// machine's forwarded socket, `~/.isb/machine/isb/serve.sock`.
pub fn default_socket_path() -> PathBuf {
    if let Some(s) = std::env::var_os("ISB_SERVE_SOCKET").filter(|s| !s.is_empty()) {
        return PathBuf::from(s);
    }
    #[cfg(target_os = "macos")]
    if let Ok(s) = crate::machine::serve_socket(crate::machine::DEFAULT_NAME) {
        return s;
    }
    if let Some(d) = std::env::var_os("XDG_RUNTIME_DIR").filter(|s| !s.is_empty()) {
        return PathBuf::from(d).join("isb/serve.sock");
    }
    std::env::temp_dir()
        .join(format!("isb-{}", rustix::process::getuid().as_raw()))
        .join("serve.sock")
}

/// Serve until SIGINT or SIGTERM.
pub fn serve(listeners: Vec<Listener>, registry: Registry, healthz: Healthz) -> Result<()> {
    serve_until(listeners, registry, healthz, Shutdown::on_signals()?)
}

/// [`serve`], with a registry the embedder also keeps (to serve it on
/// listeners it adds later, [`spawn_private`]).
pub fn serve_shared(
    listeners: Vec<Listener>,
    registry: Arc<Registry>,
    healthz: Healthz,
) -> Result<()> {
    serve_until_shared(listeners, registry, healthz, Shutdown::on_signals()?)
}

/// What a listener's requests go to: `/healthz`, `/mcp`, the REST surface
/// and its routes, through its hooks and policy.
pub fn handler(l: &Listener, registry: Arc<Registry>, healthz: Healthz) -> Handler {
    let ep = Endpoint {
        registry,
        policy: l.policy.clone(),
        access: l.access.clone(),
        healthz,
        routes: l.routes.clone(),
        public_routes: l.public_routes.clone(),
        hooks: l.hooks.clone(),
    };
    Arc::new(move |r: &http::Request| ep.handle(r))
}

/// Serve `handler` on a private (RFC 1918) address that is not loopback,
/// such as an org bridge's, until `stop` (or a bind failure, returned at
/// once). The caller's `handler` decides who gets in: nothing about such
/// an address keeps anyone out.
pub fn spawn_private(
    addr: std::net::SocketAddr,
    handler: Handler,
    stop: Shutdown,
) -> Result<std::thread::JoinHandle<()>> {
    let sock = HttpListener::bind_tcp_private(addr)?;
    let server = HttpServer::new(Limits::default(), stop);
    std::thread::Builder::new()
        .name(format!("isb-listen-{addr}"))
        .spawn(move || {
            if let Err(e) = server.run(sock, handler) {
                eprintln!("isb serve: listener {addr}: {e}");
            }
            server.drain(Duration::from_secs(5));
        })
        .map_err(|e| Error::Protocol(format!("cannot start a listener thread: {e}")))
}

/// Serve until `shutdown` is triggered, then give in-flight requests up to 10s.
/// Every listener is bound before any is served, so a bad one fails startup.
pub fn serve_until(
    listeners: Vec<Listener>,
    registry: Registry,
    healthz: Healthz,
    shutdown: Shutdown,
) -> Result<()> {
    serve_until_shared(listeners, Arc::new(registry), healthz, shutdown)
}

/// [`serve_until`] with a shared registry.
pub fn serve_until_shared(
    listeners: Vec<Listener>,
    registry: Arc<Registry>,
    healthz: Healthz,
    shutdown: Shutdown,
) -> Result<()> {
    if listeners.is_empty() {
        return Err(Error::invalid("isb serve needs at least one listener"));
    }
    let mut bound: Vec<(HttpListener, Handler)> = Vec::new();
    for l in &listeners {
        l.check()?;
        let sock = match &l.kind {
            ListenerKind::Tcp(a) if l.tailnet => HttpListener::bind_tcp_tailnet(a)?,
            ListenerKind::Tcp(a) => HttpListener::bind_tcp(a)?,
            ListenerKind::Unix(p) => HttpListener::bind_unix(p)?,
            ListenerKind::Mtls(a, t) => HttpListener::bind_tls(a, t.clone())?,
        };
        let tools = registry
            .tools()
            .iter()
            .filter(|t| l.policy.allows(&t.name))
            .count();
        let line = l.describe(tools);
        let mtls = matches!(l.kind, ListenerKind::Mtls(..));
        if l.access.is_none() && !l.is_trusted() && !mtls && l.hooks.authorize.is_none() {
            eprintln!("isb serve: WARNING: {line}");
        } else {
            eprintln!("isb serve: listening on {line}");
        }
        bound.push((sock, handler(l, registry.clone(), healthz.clone())));
    }
    let server = HttpServer::new(Limits::default(), shutdown.clone());
    let threads: Vec<_> = bound
        .into_iter()
        .map(|(sock, h)| {
            let (srv, stop) = (server.clone(), shutdown.clone());
            std::thread::spawn(move || {
                let r = srv.run(sock, h);
                // One listener failing takes the others down with it.
                stop.trigger();
                r
            })
        })
        .collect();
    let mut first = Ok(());
    for t in threads {
        let r = t
            .join()
            .unwrap_or_else(|_| Err(Error::Protocol("listener thread panicked".into())));
        if first.is_ok() {
            first = r;
        }
    }
    server.drain(Duration::from_secs(10));
    if server.active() > 0 {
        eprintln!(
            "isb serve: exiting with {} request(s) still running",
            server.active()
        );
    }
    first
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn listener_config_is_checked() {
        assert!(Listener::tcp("127.0.0.1:0").check().is_err());
        assert!(
            Listener::tcp("127.0.0.1:0")
                .allow_unauthenticated(true)
                .check()
                .is_ok()
        );
        let v = || AccessValidator::new("team.cloudflareaccess.com", "aud").unwrap();
        assert!(Listener::unix("/x").access(v()).check().is_err());
        assert!(Listener::tcp("127.0.0.1:0").access(v()).check().is_ok());
        assert!(Listener::unix("/x").is_trusted());
        let r = serve_until(
            vec![Listener::tcp("0.0.0.0:0").allow_unauthenticated(true)],
            Registry::new(),
            Arc::new(|| (true, json!({}))),
            Shutdown::new(),
        );
        assert!(r.unwrap_err().to_string().contains("not loopback"));
    }
}
