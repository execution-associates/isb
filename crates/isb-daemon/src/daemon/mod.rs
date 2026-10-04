//! `isb serve`: the stack controller behind an MCP server.
//!
//! Two doors, one set of tools:
//! - the unix socket, for the local CLI (`isb stack ...`), trusted as the
//!   daemon's own user;
//! - loopback HTTP at `/mcp`, for remote agents through a cloudflared tunnel
//!   and Cloudflare Access, held to [`policy::RemotePolicy`].
//!
//! The tools manage stacks (long-running, replicated, load-balanced
//! services), apps over them ([`crate::app`]), plain sandboxes (an isolated
//! machine for an agent), and each org's secrets ([`crate::secrets`]).

/// Register one tool: `tool!(registry, ctx, name, title, description,
/// input_schema, annotations, handler)`. The handler is called with its own
/// clone of `ctx`, the arguments and the caller. Defined before the
/// submodules so their tool tables use it too.
macro_rules! tool {
    ($r:expr, $ctx:expr, $name:expr, $title:expr, $desc:expr, $schema:expr, $ann:expr, $f:expr) => {{
        let ctx = $ctx.clone();
        let f = $f;
        $r.register(
            Tool::new($name, $desc, $schema, move |a, c| f(&ctx, a, c))
                .title($title)
                .annotations($ann.clone()),
        )?;
    }};
}

mod accounts;
pub mod apps;
pub mod audit;
mod authorize;
pub mod builds;
pub mod data;
mod default_org;
mod dns;
mod egress;
mod kube;
mod monitors;
mod notify;
mod orgs;
pub mod policy;
pub mod previews;
mod secret_hooks;
pub mod secrets;
mod servers;
mod ssh;
pub mod superadmin;
pub mod templates;
mod terminal;
mod tools;
pub mod volumes;
pub mod workspaces;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::auth::AuthConfig;
use crate::auth::AuthStore;
use crate::auth::http::{ApiConfig, AuthApi};
use crate::client::Client;
use crate::error::{Error, Result};
use crate::exec::{ExecOptions, Stdin};
use crate::sandbox::{EnsureOptions, Sandbox, SandboxInfo};
use crate::server::{AccessValidator, Caller, Listener, Registry, Tool, ToolPolicy};
use crate::spec::{ComposeFile, SandboxSpec};
use crate::stack::{Controller, StackDef, Store, now_secs};
use authorize::{CROSS_ORG_READS, PLATFORM_TOOLS, arg_org, authorize_class, tool_listed};
use policy::RemotePolicy;
use tools::Ann;

/// Marks an instance a remote caller created with `sandbox_create`.
pub const LABEL_OWNER: &str = "isb.owner";

/// How `isb serve` runs.
#[derive(Debug, Clone)]
pub struct ServeConfig {
    /// `host:port` addresses for remote MCP and the web UI: loopback, or a
    /// tailnet address with `superadmin_tailnet`. Empty serves the socket
    /// only.
    pub listen: Vec<String>,
    pub socket: PathBuf,
    /// Cloudflare Access team domain and application audience.
    pub access: Option<(String, String)>,
    /// Serve the TCP listener with no Access (local testing only).
    pub allow_unauthenticated: bool,
    /// Which tools remote callers see.
    pub remote_tools: ToolPolicy,
    pub policy: RemotePolicy,
    pub state_dir: PathBuf,
    pub interval: Duration,
    /// Where the secrets key is looked for (and generated).
    pub keys: crate::secrets::KeySources,
    /// `~/.config/isb/secrets.toml`: break-glass recipients.
    pub secrets_config: PathBuf,
    /// Session lifetimes and the rest of the identity store's settings.
    pub auth: AuthConfig,
    /// Where users reach isb, for invitation and reset links, provider
    /// callbacks and the passkey relying party.
    pub public_url: Option<String>,
    /// External sign-in providers (GitHub, Google, generic OIDC).
    pub oauth: crate::auth::oauth::OAuthSettings,
    /// Accounts without an invitation, for verified provider emails.
    pub open_signup: bool,
    /// The HTTP(S) edge for stack domains; `None` leaves domains unserved.
    pub ingress: Option<crate::ingress::IngressConfig>,
    /// The port each org's workspace reaches the org-bound MCP on, on the
    /// org bridge's address.
    pub workspace_mcp_port: u16,
    /// `--workspace-pool`: the storage pool new workspace homes go in,
    /// unless the org sets its own; none: the org's default pool.
    pub workspace_pool: Option<String>,
    /// `--workspace-home-root`: workspace homes are host folders
    /// `<root>/<org>/home` instead of managed volumes.
    pub workspace_home_root: Option<PathBuf>,
    /// `--preview-domain`: where workspace ports' previews get their origins.
    pub preview_domain: Option<workspaces::PreviewBase>,
    /// How long audit rows are kept.
    pub audit_retention: Duration,
    /// Record read-only tool calls too (secret reads always are).
    pub audit_all: bool,
    /// How long, and how many, history rows are kept.
    pub history_retention: Duration,
    pub history_max_rows: i64,
    /// Run as a server's agent for a control plane (docs/guides/servers.md): an
    /// mTLS listener instead of the identity store, web UI and `--listen`.
    pub agent: Option<AgentConfig>,
    /// `--superadmin-tailnet`: tailnet logins and tags with the unix
    /// socket's reach.
    pub superadmin_tailnet: Option<crate::server::tailnet::AllowList>,
    /// `--superadmin-access`: Access emails and service token client ids
    /// with the unix socket's reach.
    pub superadmin_access: Option<superadmin::AccessAllowList>,
    /// `--heartbeat-url`: a dead man's switch pinged every interval.
    pub heartbeat: Option<crate::monitor::heartbeat::Heartbeat>,
    /// `--egress-pin NAME=IP[:PORT]`: names the egress proxy connects to
    /// at a fixed address instead of resolving.
    pub egress_pins: Vec<String>,
    /// `--egress-ca FILE`: roots the egress proxy trusts besides the system's.
    pub egress_ca: Vec<PathBuf>,
}

/// `isb serve --agent`.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// `host:port` on any address; only the control plane's client certificate gets through.
    pub listen: String,
    /// `ca.crt`, `tls.crt`, `tls.key` from the control plane.
    pub tls_dir: PathBuf,
}

/// The identity endpoints over `<state>/isb.db`, and the web UI. Provider
/// client secrets not in the environment are read from the default org's
/// secrets.
fn auth_routes(
    cfg: &ServeConfig,
    store: Arc<AuthStore>,
    secrets: &Arc<crate::secrets::Secrets>,
    log: &Arc<crate::audit::AuditLog>,
    gate: Arc<superadmin::Gate>,
) -> Result<crate::server::Routes> {
    use crate::auth::oauth::SecretFn;
    let default_org = crate::org::OrgId::default_org();
    let lookup = |name: &str| -> Option<SecretFn> {
        secrets.inspect(&default_org, name).ok()?;
        let (s, org, name) = (secrets.clone(), default_org.clone(), name.to_string());
        Some(Arc::new(move || {
            let (v, _) = s.get(&org, &name).map_err(|e| e.to_string())?;
            String::from_utf8(v)
                .map(|v| v.trim().to_string())
                .map_err(|_| "the secret is not UTF-8".to_string())
        }))
    };
    let (providers, notes) = cfg.oauth.providers(&lookup);
    for n in notes {
        eprintln!("isb serve: {n}");
    }
    let path = crate::auth::db_path(&cfg.state_dir);
    let agent_ways = gate.agent_ways().with_public_url(cfg.public_url.clone());
    let api = AuthApi::new(
        store.clone(),
        ApiConfig {
            agent: Some(gate.agent_fn()),
            agent_ways,
            public_url: cfg.public_url.clone(),
            notifier: None,
            setup_token_file: Some(cfg.state_dir.join("setup-token")),
            edge: Some(gate.edge_fn()),
            providers,
            open_signup: cfg.open_signup,
            audit: Some(log.clone()),
            superadmin: Some(Arc::new(move |r: &crate::server::http::Request| match gate
                .resolve(r, None)
            {
                superadmin::Resolved::Superadmin(s) => Some(s),
                _ => None,
            })),
        },
    )?;
    eprintln!("isb serve: identity store {}", path.display());
    if !crate::web::BUILT {
        eprintln!(
            "isb serve: this binary was built without the web UI (a placeholder page is served)"
        );
    }
    // The identity endpoints first, the audit tail, then the web UI, which
    // answers every other non-API GET.
    let (auth, web) = (Arc::new(api).router(), crate::web::routes());
    let tail = audit::stream_route(log.clone(), store);
    Ok(Arc::new(move |r| {
        auth(r).or_else(|| tail(r)).or_else(|| web(r))
    }))
}

struct Daemon {
    client: Client,
    ctl: Controller,
    policy: RemotePolicy,
    state_dir: PathBuf,
    secrets: Arc<crate::secrets::Secrets>,
    apps: crate::app::Apps,
    ingress: Option<Arc<crate::ingress::Manager>>,
    /// The identity store: the org list memberships hang off.
    users: Arc<AuthStore>,
    notifier: crate::notify::Notifier,
    monitors: crate::monitor::Monitors,
    history: crate::metrics_history::History,
    /// Databases' backups and scheduled jobs.
    data: data::Ctx,
    /// Named volumes' snapshots and staged restores.
    volumes: crate::volume_backup::VolumeBackups,
    audit: Arc<crate::audit::AuditLog>,
    /// The servers orgs can be placed on (a control plane; `None` on an
    /// agent).
    servers: Option<Arc<crate::servers::Servers>>,
    /// Who is a superadmin, and what `host_policy` reports.
    gate: Arc<superadmin::Gate>,
    host: Value,
    /// The template catalogs, shared by the tools and the logo route.
    catalogs: Arc<crate::template::catalog::Catalogs>,
    /// Each org's workspace, its token and its bridge listener; the
    /// sandbox reaper.
    workspaces: Arc<workspaces::Workspaces>,
    /// Where users reach isb, for invitation links.
    public_url: Option<String>,
    /// One proxy per egress network (docs/guides/egress.md).
    egress: Arc<isb_egress::Manager>,
}

/// Run the daemon until SIGINT/SIGTERM. Apps keep running when it stops.
#[expect(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
pub fn serve(client: Client, cfg: ServeConfig) -> Result<()> {
    client
        .server_info()
        .map_err(|e| Error::invalid(format!("isb serve needs incusd: {e}")))?;
    default_org::warn_old_incus(&client);
    let store = Store::open(&cfg.state_dir)?;
    dns::open_dns_path(&cfg.state_dir);
    // The default org is the incus project `isb-default`, made here when
    // it is missing. A server's agent has no default org of its own.
    if cfg.agent.is_none() {
        default_org::ensure(&client, &store);
    }
    let secrets_config = crate::secrets::SecretsConfig::load(&cfg.secrets_config)?;
    let opened = crate::secrets::Secrets::open(&cfg.state_dir, &cfg.keys, &secrets_config)?;
    for n in &opened.notes {
        eprintln!("isb serve: {n}");
    }
    let secrets = Arc::new(with_external_drivers(opened.secrets, &cfg.state_dir)?);
    // Definitions from before secrets were references carry values: move
    // them into the store before anything reads the definitions.
    for r in crate::stack::migrate::run(&store, &secrets, Some(&client)) {
        match r {
            Ok(m) => eprintln!("isb serve: {m}"),
            Err(e) => eprintln!("isb serve: WARNING: {e}"),
        }
    }
    // Open the identity store before anything starts, so a bad one fails
    // startup cleanly. Its endpoints ride on the TCP listener.
    let db = crate::auth::db_path(&cfg.state_dir);
    let users = Arc::new(
        AuthStore::open_with(&db, cfg.auth.clone())
            .map_err(|e| Error::invalid(format!("open {}: {e}", db.display())))?,
    );
    let audit_db = crate::audit::db_path(&cfg.state_dir);
    let audit_log = Arc::new(
        crate::audit::AuditLog::open(&audit_db, cfg.audit_retention)?
            .with_history_limits(cfg.history_retention, cfg.history_max_rows),
    );
    // The history: markers for the time nobody was watching and for this
    // start, then incus' lifecycle events from now on.
    let recorder = crate::history::Recorder::start(audit_log.clone());
    let stop_history = Arc::new(std::sync::atomic::AtomicBool::new(false));
    history_start(&audit_log, &recorder, &client, &stop_history);
    eprintln!(
        "isb serve: audit log {} (kept {} days{})",
        audit_db.display(),
        cfg.audit_retention.as_secs() / 86400,
        if cfg.audit_all {
            ", reads included"
        } else {
            ""
        }
    );
    if cfg.agent.is_some() && !cfg.listen.is_empty() {
        return Err(Error::invalid(
            "--agent serves its control plane only: drop --listen (users reach the control plane)",
        ));
    }
    let access = match &cfg.access {
        Some((team, aud)) => Some(Arc::new(AccessValidator::new(team, aud)?)),
        None => None,
    };
    let gate = Arc::new(superadmin::gate(&cfg, users.clone(), access.clone())?);
    let auth = if cfg.listen.is_empty() {
        None
    } else {
        Some(auth_routes(
            &cfg,
            users.clone(),
            &secrets,
            &audit_log,
            gate.clone(),
        )?)
    };
    let servers = match &cfg.agent {
        None => Some(crate::servers::Servers::open(&cfg.state_dir)?),
        Some(_) => None,
    };
    // The local registry, when set up: this daemon pushes to it and keeps
    // its push index under the state directory.
    match crate::registry::Registry::open(&client, Some(&cfg.state_dir)) {
        Ok(Some(r)) => {
            eprintln!("isb serve: local registry {}", r.info().url());
            crate::registry::install(Arc::new(r));
        }
        Ok(None) => {}
        Err(e) => eprintln!("isb serve: WARNING: local registry: {e}"),
    }
    // The ingress follows rotation from the first replica the controller
    // resumes, so it exists before the controller does.
    let ingress = match &cfg.ingress {
        Some(ic) => Some(crate::ingress::Manager::new(
            ic.clone(),
            client.clone(),
            secrets.clone(),
            &cfg.state_dir,
        )?),
        None => None,
    };
    let observer = ingress
        .clone()
        .map(|m| m as Arc<dyn crate::stack::controller::Observer>);
    let ctl = Controller::start_with(
        client.clone(),
        store,
        cfg.interval,
        secrets.clone(),
        observer,
    )?;
    if let Some(m) = &ingress {
        m.start(ctl.clone())?;
    }
    let apps = crate::app::Apps::new(&cfg.state_dir, client.clone(), ctl.clone(), secrets.clone());
    // Notifications follow the event feed from the start of this run.
    let ra = apps.clone();
    let resolve: crate::notify::Resolve = Arc::new(move |org, stack, service| {
        let a = ra.get(org, service).ok()?;
        // The app's own stack, or one of its previews' (`<project>-...-pr-<n>`).
        let own = a.spec.stack().ok()? == stack;
        let preview = stack.starts_with(&format!("{}-", a.spec.project))
            && crate::app::preview::is_pr_suffix(stack);
        (own || preview).then_some(a.spec.project)
    });
    let notifier = crate::notify::Notifier::new(&cfg.state_dir, secrets.clone(), resolve)?;
    notifier.start(ctl.clone());
    let monitors = monitors::start(&cfg, &apps, &secrets, &notifier);
    // Every controller event goes to the history (the ones already emitted at startup first).
    ctl.set_event_sink(recorder.controller_sink());
    // Every metrics sample also goes to the history.
    let history = crate::metrics_history::History::new(&cfg.state_dir);
    ctl.set_metrics_sink(history.start());
    // Previews past their TTL, and removals that did not finish.
    apps.start_preview_upkeep();
    // Jobs and backups share one scheduler thread.
    let jobs = crate::jobs::Jobs::new(&cfg.state_dir, apps.clone());
    let backups = crate::backup::Backups::new(&cfg.state_dir, apps.clone());
    let volumes =
        crate::volume_backup::VolumeBackups::new(&cfg.state_dir, apps.clone(), backups.clone());
    let scheduler = crate::jobs::Scheduler::start(vec![
        Arc::new(jobs.clone()) as Arc<dyn crate::jobs::Scheduled>,
        Arc::new(backups.clone()),
        Arc::new(volumes.clone()),
    ]);
    jobs.set_scheduler(scheduler.clone());
    backups.set_scheduler(scheduler.clone());
    volumes.set_scheduler(scheduler.clone());
    if let Some(ic) = &cfg.ingress {
        if ic.tunnel_port == cfg.workspace_mcp_port {
            return Err(Error::invalid(format!(
                "--workspace-mcp-port {} is the ingress's tunnel port; pick another",
                cfg.workspace_mcp_port
            )));
        }
    }
    let workspaces = workspaces::Workspaces::new(
        &cfg.state_dir,
        client.clone(),
        secrets.clone(),
        recorder.clone(),
        cfg.workspace_mcp_port,
        cfg.workspace_pool.clone(),
        cfg.workspace_home_root.clone(),
    );
    workspaces.previews.set_base(cfg.preview_domain.clone());
    let (egress, stop_egress) = egress::start(&cfg, &client, &secrets)?;
    let d = Arc::new(Daemon {
        client,
        ctl: ctl.clone(),
        policy: cfg.policy.clone(),
        state_dir: cfg.state_dir.clone(),
        secrets,
        apps: apps.clone(),
        ingress: ingress.clone(),
        users: users.clone(),
        notifier: notifier.clone(),
        monitors: monitors.clone(),
        history,
        data: data::Ctx {
            apps: apps.clone(),
            jobs,
            backups,
        },
        volumes,
        audit: audit_log.clone(),
        servers: servers.clone(),
        gate: gate.clone(),
        host: superadmin::host_summary(&cfg, &gate),
        catalogs: Arc::new(crate::template::catalog::Catalogs::new(&cfg.state_dir)),
        workspaces: workspaces.clone(),
        public_url: cfg.public_url.clone(),
        egress,
    });
    if let Some(s) = &servers {
        s.start(ctl.clone());
    }
    let registry = registry(d.clone())?;
    let mut hooks = hooks(d.clone(), users.clone(), cfg.allow_unauthenticated);
    superadmin::announce(&cfg, &gate, &users);
    hooks.audit = Some(audit::hook(audit_log.clone(), cfg.audit_all));
    if servers.is_some() {
        hooks.route = Some(servers::route(d.clone()));
    }
    // Webhooks carry their own credential (a signature), and come from
    // senders that hold no session; a control plane hands those for orgs on servers to the server.
    let webhooks = {
        let w = apps::webhook_routes(apps.clone());
        let w = match &servers {
            Some(s) => servers::forward_webhooks(w, s.clone()),
            None => w,
        };
        audit::audited_webhooks(w, audit_log.clone())
    };
    // Template logos from isb's own cache, ahead of the web UI.
    let auth = auth.map(|a| -> crate::server::Routes {
        let logo = templates::logo::route(
            d.catalogs.clone(),
            Arc::new(templates::logo::Logos::new(&cfg.state_dir)),
            templates::logo::admit(
                hooks.authn.clone().expect("the daemon authenticates"),
                access.clone(),
                cfg.allow_unauthenticated,
            ),
        );
        Arc::new(move |r| logo(r).or_else(|| a(r)))
    });
    let mut listeners = vec![Listener::unix(&cfg.socket).hooks(hooks.clone())];
    if let Some(ac) = &cfg.agent {
        listeners.push(servers::agent_listener(
            d.clone(),
            &hooks,
            ac,
            webhooks.clone(),
        )?);
    }
    for addr in &cfg.listen {
        let tailnet = superadmin::is_tailnet_listen(addr);
        let mut l = Listener::tcp(addr.clone())
            .policy(cfg.remote_tools.clone())
            .hooks(hooks.clone())
            .public_routes(webhooks.clone())
            .preview(workspaces::preview_route(d.clone()))
            .tailnet(tailnet);
        if let Some(r) = &auth {
            l = l.routes(r.clone());
        }
        listeners.push(match &access {
            // Access guards the loopback listeners (the tunnel's end).
            Some(v) if !tailnet => l.access_shared(v.clone()),
            // Callers sign in with an API token or a session (or are tailnet
            // superadmins); the authorizer refuses anonymous ones unless
            // --allow-unauthenticated.
            _ => l.allow_unauthenticated(true),
        });
    }
    let hd = d.clone();
    let healthz: crate::server::Healthz = Arc::new(move || {
        let stacks: Vec<Value> = hd
            .ctl
            .list()
            .into_iter()
            .map(|s| json!({"name": s.name, "converged": s.converged}))
            .collect();
        (
            true,
            json!({"ok": true, "isb": env!("CARGO_PKG_VERSION"), "stacks": stacks}),
        )
    });
    // Each org's workspace reaches the org-bound surface on its bridge:
    // the hooks and tool policy of the TCP listeners, no Access (only the
    // org's own subnet, with bearer tokens, gets in).
    let registry = Arc::new(registry);
    workspaces.set_serving(
        Listener::tcp("org-bridge")
            .policy(cfg.remote_tools.clone())
            .hooks(hooks.clone())
            .allow_unauthenticated(true),
        registry.clone(),
        healthz.clone(),
    );
    let local: workspaces::LocalOrg = {
        let d = d.clone();
        Arc::new(move |o: &crate::org::OrgId| d.remote(o).is_none())
    };
    workspaces.start(ctl.clone(), local);
    workspaces::start_ports(d.clone());
    let r = crate::server::serve_shared(listeners, registry, healthz);
    workspaces.shutdown();
    stop_egress.store(true, std::sync::atomic::Ordering::Relaxed);
    stop_history.store(true, std::sync::atomic::Ordering::Relaxed);
    recorder.record(crate::history::marker(
        "serve.stopped",
        "isb serve stopped: incus events from now on are not observed".into(),
        json!({"version": env!("CARGO_PKG_VERSION")}),
    ));
    recorder.shutdown();
    if let Some(s) = &servers {
        s.shutdown();
    }
    notifier.shutdown();
    monitors.shutdown();
    scheduler.shutdown();
    ctl.shutdown();
    if let Some(m) = &ingress {
        m.shutdown();
    }
    r
}

/// Record the gap since the history last heard anything and this start,
/// then follow incus' lifecycle events until `stop`.
fn history_start(
    log: &Arc<crate::audit::AuditLog>,
    rec: &Arc<crate::history::Recorder>,
    client: &Client,
    stop: &Arc<std::sync::atomic::AtomicBool>,
) {
    use crate::history::{HistoryQuery, marker};
    let now = crate::audit::now_ms();
    let last = log
        .history_list(
            &HistoryQuery::default(),
            &crate::audit::Visibility::All,
            None,
            None,
            1,
        )
        .ok()
        .and_then(|v| v.into_iter().next());
    if let Some(l) = last {
        let clean = l.kind == "serve.stopped";
        let reason = if clean {
            "isb serve was not running"
        } else {
            "isb serve was not running (it did not stop cleanly)"
        };
        rec.record(marker(
            "incus.gap",
            format!(
                "incus events between {} and {} were not observed: {reason}",
                crate::history::fmt_ms(l.time),
                crate::history::fmt_ms(now),
            ),
            json!({"from": l.time, "to": now, "reason": reason}),
        ));
    }
    rec.record(marker(
        "serve.started",
        format!("isb serve {} started", env!("CARGO_PKG_VERSION")),
        json!({"version": env!("CARGO_PKG_VERSION"), "pid": std::process::id()}),
    ));
    let (c, r, s) = (client.clone(), rec.clone(), stop.clone());
    let _ = std::thread::Builder::new()
        .name("isb-incus-events".into())
        .spawn(move || crate::history::watch_incus(c, r, s));
}

/// The external secret drivers, each reading its credentials from the org's own `local` secrets.
fn with_external_drivers(
    secrets: crate::secrets::Secrets,
    state_dir: &std::path::Path,
) -> Result<crate::secrets::Secrets> {
    use crate::secrets::{Driver, local::LocalDriver, onepassword};
    let local = Arc::new(LocalDriver::new(state_dir, secrets.keyring().clone()));
    let token: onepassword::TokenSource =
        Arc::new(move |org| match local.get(org, onepassword::TOKEN_SECRET) {
            Ok((v, _)) => Ok(Some(
                String::from_utf8(v)
                    .map_err(|_| Error::invalid("the 1Password token is not text"))?
                    .trim()
                    .to_string(),
            )),
            Err(e) if e.is_not_found() => Ok(None),
            Err(e) => Err(e),
        });
    secrets.with_driver(Arc::new(onepassword::OnePasswordDriver::new(token)))
}

/// The orgs a caller may see, or `None` for all of them.
fn visible_orgs(c: &Caller) -> Option<Vec<crate::org::OrgId>> {
    match c.principal() {
        Some(p) if !p.platform_admin => Some(p.orgs.iter().map(|(o, _)| o.clone()).collect()),
        _ => None,
    }
}

/// Authentication and authorization for every listener.
fn hooks(d: Arc<Daemon>, users: Arc<AuthStore>, allow_anonymous: bool) -> crate::server::Hooks {
    use crate::server::Authenticated;
    let term = terminal::terminal(d.clone());
    let ssh = ssh::ssh(d.clone(), users.clone());
    let u = users.clone();
    let gate = d.gate.clone();
    let wsa = d.workspaces.clone();
    let authn: crate::server::mcp::Authn = Arc::new(move |req, id| {
        match gate.resolve(req, id) {
            superadmin::Resolved::Superadmin(s) => return Authenticated::Superadmin(s),
            superadmin::Resolved::Refused => return Authenticated::Refused,
            superadmin::Resolved::None => {}
        }
        // An org's workspace token: judged by the workspaces, which keep it.
        if let Some(t) = bearer(req) {
            if t.starts_with(crate::auth::secret::TokenKind::Workspace.prefix()) {
                return match wsa.authenticate(t) {
                    Some(p) => Authenticated::User(Arc::new(p)),
                    None => Authenticated::Refused,
                };
            }
        }
        if req.header("authorization").is_some()
            || req
                .header("cookie")
                .is_some_and(|c| c.contains("isb_session="))
        {
            return match u.principal_from_request(req) {
                Some(p) => Authenticated::User(Arc::new(p)),
                None => Authenticated::Refused,
            };
        }
        // Access vouches for the email; the isb account decides the orgs.
        if let Some(email) = id.and_then(|i| i.email.as_deref()) {
            if let Ok(Some(p)) = u.principal_for_email(email) {
                return Authenticated::User(Arc::new(p));
            }
        }
        // A tailnet or Access caller an org mapped to a role.
        if let Some(p) = gate.agent(req, id) {
            return Authenticated::User(Arc::new(p));
        }
        Authenticated::None
    });
    let authorize: crate::server::mcp::Authorize = Arc::new(move |c, tool, args, scope| {
        // `app` for `name`, `command` for `argv`, before anything reads them.
        let args = crate::server::aliases::alias_args(tool, args);
        authorize_class(
            c,
            &tool.name,
            audit::class_for(tool, &args),
            args,
            scope,
            allow_anonymous,
        )
    });
    let events: crate::server::mcp::Events = Arc::new(move |c, since| {
        if let (Caller::Unauthenticated { .. }, false) = (c, allow_anonymous) {
            return Err(Error::Forbidden("sign in to follow events".into()));
        }
        if let Caller::Access(id) = c {
            return Err(Error::Forbidden(format!(
                "{} has no isb account",
                id.name()
            )));
        }
        let orgs = visible_orgs(c);
        let ctl = d.ctl.clone();
        Ok(Box::new(move |w: &mut dyn std::io::Write| {
            let mut since = since;
            loop {
                let (seq, evs) = ctl.wait_events(since, 200, Duration::from_secs(15));
                let mut wrote = false;
                for e in evs {
                    if !event_visible(&orgs, &e.stack) {
                        continue;
                    }
                    let data = serde_json::to_string(&e).unwrap_or_default();
                    write!(w, "id: {}\nevent: {}\ndata: {data}\n\n", e.seq, e.level)?;
                    wrote = true;
                }
                if !wrote {
                    // Keeps proxies from closing an idle stream.
                    w.write_all(b": keepalive\n\n")?;
                }
                w.flush()?;
                since = seq.max(since);
            }
        }))
    });
    crate::server::Hooks {
        authn: Some(authn),
        authorize: Some(authorize),
        events: Some(events),
        terminal: Some(term),
        ssh: Some(ssh),
        audit: None,
        route: None,
        listed: Some(Arc::new(tool_listed)),
        refuse_anonymous: !allow_anonymous,
    }
}

/// The bearer token a request carries, if any.
fn bearer(req: &crate::server::http::Request) -> Option<&str> {
    let (scheme, token) = req.header("authorization")?.trim().split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
}

/// Events name their stack `org/stack` (or just `stack` in the default org).
fn event_visible(orgs: &Option<Vec<crate::org::OrgId>>, stack: &str) -> bool {
    let Some(orgs) = orgs else { return true };
    let org = stack
        .split_once('/')
        .map(|(o, _)| o)
        .unwrap_or(crate::org::DEFAULT_ORG);
    orgs.iter().any(|o| o.as_str() == org)
}

fn args<T: DeserializeOwned>(v: Value) -> Result<T> {
    serde_json::from_value(v).map_err(|e| Error::invalid(format!("bad arguments: {e}")))
}

fn obj(mut props: Value, required: &[&str]) -> Value {
    // Every tool works within one org.
    props["org"] =
        json!({"type": "string", "description": "The org to act in (default: default)."});
    json!({"type": "object", "properties": props, "required": required, "additionalProperties": false})
}

/// A stack's qualified name from a tool's `org` and `name`.
fn qname(org: &Option<String>, name: &str) -> Result<String> {
    let org = match org {
        Some(o) => crate::org::OrgId::new(o.clone())?,
        None => crate::org::OrgId::default_org(),
    };
    Ok(crate::stack::qualified(&org, name))
}

fn caller_name(c: &Caller) -> String {
    c.to_string()
}

/// Build the tool registry.
fn registry(d: Arc<Daemon>) -> Result<Registry> {
    let mut r = Registry::new().instructions(INSTRUCTIONS);
    superadmin::register(&mut r, d.clone())?;
    let ann = Ann {
        ro: json!({"readOnlyHint": true, "openWorldHint": false}),
        destructive: json!({"destructiveHint": true, "openWorldHint": false}),
        write: json!({"destructiveHint": false, "openWorldHint": false}),
    };

    tools::stack_deploy_tool(&mut r, &d, &ann)?;
    tools::overview_tool(&mut r, &d, &ann)?;
    tools::events_tool(&mut r, &d, &ann)?;
    tools::ingress_status_tool(&mut r, &d, &ann)?;
    tools::stack_list_tool(&mut r, &d, &ann)?;
    tools::stack_status_tool(&mut r, &d, &ann)?;
    tools::stack_config_tool(&mut r, &d, &ann)?;
    tools::stack_logs_tool(&mut r, &d, &ann)?;
    tools::stack_scale_tool(&mut r, &d, &ann)?;
    tools::stack_edit_tools(&mut r, &d, &ann)?;
    tools::sandbox_create_tool(&mut r, &d, &ann)?;
    secret_hooks::register(&mut r, &d)?;
    builds::register(
        &mut r,
        builds::Ctx {
            client: d.client.clone(),
            policy: d.policy.clone(),
            ctl: d.ctl.clone(),
        },
    )?;
    tools::sandbox_tools(&mut r, &d, &ann)?;
    apps::register(&mut r, d.apps.clone(), d.ingress.is_some())?;
    previews::register(&mut r, d.apps.clone())?;
    let mut t = templates::Templates::new(
        &d.state_dir,
        d.apps.clone(),
        d.secrets.clone(),
        d.ingress.as_ref().and_then(|m| m.public_ip()),
    );
    t.catalogs = d.catalogs.clone();
    templates::register(&mut r, t)?;
    data::register(&mut r, d.data.clone())?;
    volumes::register(&mut r, d.volumes.clone())?;
    tools::server_status_tool(&mut r, &d, &ann)?;
    orgs::register(&mut r, d.clone())?;
    notify::register(
        &mut r,
        d.notifier.clone(),
        d.history.clone(),
        d.apps.clone(),
    )?;
    monitors::register(&mut r, d.monitors.clone())?;
    audit::register(&mut r, d.audit.clone())?;
    audit::register_history(&mut r, d.audit.clone())?;
    servers::register(&mut r, d.clone())?;
    ssh::register(&mut r, d.clone())?;
    workspaces::register(&mut r, d.clone())?;
    accounts::register(&mut r, d.clone())?;
    Ok(r)
}

const INSTRUCTIONS: &str = "isb runs incus containers and VMs on this host. Two uses: \
stacks (long-running services from a docker-compose-style file, with replicas, health checks, \
rolling updates and a load balancer: stack_deploy, then stack_status) and sandboxes \
(an isolated machine to run code in: sandbox_create, sandbox_exec, sandbox_remove). \
Images: local incus aliases (dev-base), images:debian/12, OCI images (docker:nginx:1.27, ghcr:org/app:tag), \
or the org's own builds in the local registry (registry:APP:TAG; build_run makes them, registry_list lists them). \
Deploys return immediately; poll stack_status, or pass wait=true. \
Each org also has a secret store (secret_create, secret_set, secret_list; values are base64). \
Apps (Dokploy-style): project_create, then app_create (an image, or a repository with a builder), \
app_env_set, app_deploy (or app_apply: a YAML definition that creates or updates, dry_run to diff first); each project environment runs as one stack <project>-<env>. \
One-click apps: template_list, template_get, then template_deploy (dry_run first shows the plan). \
Databases are apps too (database_create; connection details via database_get), backed up to S3-compatible \
destinations on a cron schedule (backup_destination_create, backup_create, backup_run, backup_restore). \
Scheduled jobs run commands against an app on a cron schedule (job_create, job_runs, job_run_log).";

impl Daemon {
    /// May this caller touch this instance? Local callers: always. Remote:
    /// only what isb serve manages, unless the operator allowed any.
    fn reachable(&self, c: &Caller, i: &SandboxInfo) -> bool {
        // A signed-in user reaches everything in an org they belong to (the
        // authorizer already checked the org): the org is the boundary.
        c.is_trusted()
            || c.principal().is_some()
            || self.policy.any_instance
            || i.config.contains_key("user.isb.stack")
            || i.config.contains_key(&format!("user.{LABEL_OWNER}"))
    }

    /// A client on the org a tool call names (default: the default org),
    /// refused up front when that org does not exist.
    fn oc(&self, org: &Option<String>) -> Result<Client> {
        let org = crate::org::OrgId::new(org.as_deref().unwrap_or(crate::org::DEFAULT_ORG))?;
        crate::org::check_exists(&self.client, &org)?;
        Ok(crate::org::client(&self.client, &org))
    }

    fn reach(&self, c: &Caller, oc: &Client, name: &str) -> Result<SandboxInfo> {
        let info = Sandbox::get(oc, name)?.info()?;
        if !self.reachable(c, &info) {
            // Indistinguishable from absent, so a remote caller cannot map
            // the host's other instances.
            return Err(Error::NotFound(format!("sandbox {name}")));
        }
        Ok(info)
    }

    /// Where a remote stack's relative paths resolve by default.
    /// An org's workspace definition, by name.
    fn workspaces_def(
        &self,
        org: &crate::org::OrgId,
        name: &str,
    ) -> Result<crate::workspace::Workspace> {
        crate::workspace::Store::new(&self.state_dir)
            .get(org, name)?
            .ok_or_else(|| Error::NotFound(format!("org {org} has no workspace {name}")))
    }

    fn files_dir(&self, stack: &str) -> Result<PathBuf> {
        let p = self.state_dir.join("files").join(stack);
        std::fs::create_dir_all(&p)?;
        Ok(p)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeployArgs {
    name: String,
    #[serde(default)]
    org: Option<String>,
    /// YAML text (remote callers, and anything not pre-resolved).
    #[serde(default)]
    compose: Option<String>,
    /// A compose file already resolved by the local CLI (no interpolation).
    #[serde(default)]
    file: Option<ComposeFile>,
    #[serde(default)]
    vars: BTreeMap<String, String>,
    #[serde(default)]
    secrets: BTreeMap<String, String>,
    #[serde(default)]
    base_dir: Option<PathBuf>,
    #[serde(default)]
    wait: bool,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    timeout: Option<String>,
}

#[expect(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
fn stack_deploy(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    let a: DeployArgs = args(a)?;
    crate::stack::validate_stack_name(&a.name)?;
    if a.name == crate::ingress::cloudflare::TUNNEL_STACK {
        return Err(Error::invalid(format!(
            "stack name {} is isb's (an org's cloudflared)",
            a.name
        )));
    }
    let base = match &a.base_dir {
        Some(b) => {
            if !b.is_absolute() {
                return Err(Error::invalid("base_dir must be absolute"));
            }
            if !c.is_trusted() {
                d.policy.check_base_dir(b)?;
            }
            b.clone()
        }
        None => d.files_dir(&a.name)?,
    };
    let file = match (a.file, a.compose) {
        (Some(_), _) if !c.is_trusted() => {
            return Err(Error::invalid("remote callers send `compose` as YAML text"));
        }
        (Some(f), None) => f,
        (None, Some(text)) => {
            // The daemon's own environment is never consulted.
            let vars = a.vars.clone();
            let lookup = move |k: &str| vars.get(k).cloned();
            crate::compose::load_docs(
                &[(PathBuf::from("compose.yaml"), text)],
                &base,
                Some(&a.name),
                &lookup,
            )?
            .file
        }
        _ => return Err(Error::invalid("pass exactly one of compose or file")),
    };
    if !c.is_trusted() {
        d.policy.check_file(&file, &base)?;
    }
    let org = match &a.org {
        Some(o) => crate::org::OrgId::new(o.clone())?,
        None => crate::org::OrgId::default_org(),
    };
    // Values for `file:`/`environment:` secrets: given directly, or from
    // `vars` for `environment:` ones. The rest come from the org's store.
    let mut given: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for key in crate::stack::secrets::used_keys(&file) {
        let Some(def) = file.secrets.get(&key) else {
            continue;
        };
        if !def.is_client_side() {
            continue;
        }
        let v = a.secrets.get(&key).cloned().or_else(|| {
            def.environment
                .as_ref()
                .and_then(|e| a.vars.get(e).cloned())
        });
        if let Some(v) = v {
            given.insert(key, v.into_bytes());
        }
    }
    let mut def = StackDef {
        name: a.name.clone(),
        org: org.clone(),
        file,
        base_dir: base,
        secrets: BTreeMap::new(),
        force: BTreeMap::new(),
        images: BTreeMap::new(),
        deployed_at: now_secs(),
        deployed_by: caller_name(c),
        previous: None,
    };
    // Checked before any value is stored, so a deploy that cannot happen bumps no secret's version.
    d.ctl.validate(&def)?;
    if let Some(m) = &d.ingress {
        m.check(&def)?;
    }
    def.secrets =
        crate::stack::secrets::bind(&d.secrets, &org, &a.name, &def.file, &given, a.dry_run)?;
    if a.dry_run {
        return Ok(json!({"changes": d.ctl.plan(&def)?, "dry_run": true}));
    }
    let who = def.deployed_by.clone();
    let changes = d.ctl.deploy(def)?;
    let summary: Vec<String> = changes
        .iter()
        .filter(|c| c.change != "unchanged")
        .map(|c| format!("{} {}", c.service, c.change))
        .collect();
    d.ctl.note(
        "info",
        &crate::stack::qualified(&org, &a.name),
        format!(
            "deployed by {who}: {}",
            if summary.is_empty() {
                "no changes".to_string()
            } else {
                summary.join(", ")
            }
        ),
    );
    if !a.wait {
        return Ok(json!({"changes": changes}));
    }
    let timeout = match &a.timeout {
        Some(t) => crate::flex::parse_duration(t).map_err(Error::invalid)?,
        None => Duration::from_secs(600),
    };
    let st = wait_settled(&d.ctl, &crate::stack::qualified(&org, &a.name), timeout)?;
    Ok(json!({"changes": changes, "status": st}))
}

/// Poll until every service is converged, or one is paused or failing (its
/// message says why), or `timeout`.
pub fn wait_settled(
    ctl: &Controller,
    name: &str,
    timeout: Duration,
) -> Result<crate::stack::controller::StackStatus> {
    let started = Instant::now();
    let def = ctl.definition(name)?;
    loop {
        let st = ctl.status(name)?;
        // A status from before the worker saw this deployment has an older
        // revision or replica count; only one that matches counts.
        let settled = st.services.iter().all(|s| {
            let current = def.revision(&s.service).is_ok_and(|r| r == s.rev)
                && def
                    .service(&s.service)
                    .is_ok_and(|d| d.replicas() == s.replicas);
            current && matches!(s.state.as_str(), "converged" | "paused" | "failing")
        });
        if settled || started.elapsed() >= timeout {
            return Ok(st);
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SpecArg {
    Text(String),
    Object(Box<SandboxSpec>),
}

#[expect(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
fn sandbox_create(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    #[derive(Deserialize)]
    struct A {
        spec: Value,
        #[serde(default)]
        wait_ready: Option<bool>,
        #[serde(default)]
        expires: Option<String>,
        #[serde(default)]
        idle_timeout: Option<String>,
        #[serde(default)]
        org: Option<String>,
    }
    let org = arg_org(&a)?;
    let a: A = args(a)?;
    let mut spec = match serde_json::from_value::<SpecArg>(a.spec)
        .map_err(|e| Error::invalid(format!("spec: {e}")))?
    {
        SpecArg::Text(t) => serde_yaml_ng::from_str::<SandboxSpec>(&t)
            .map_err(|e| Error::invalid(format!("spec: {e}")))?,
        SpecArg::Object(s) => *s,
    };
    let name = spec
        .name
        .clone()
        .ok_or_else(|| Error::invalid("spec needs container_name"))?;
    egress::check_secrets(&d.secrets, &org, &spec)?;
    let base = if c.is_local() {
        std::env::current_dir()?
    } else {
        d.files_dir("_sandboxes")?
    };
    if let Caller::Superadmin(s) = c {
        // The socket's reach, under the superadmin's own name.
        spec.labels.insert(LABEL_OWNER.into(), s.label());
    }
    if !c.is_trusted() {
        d.policy.check_spec(&spec, &base)?;
        if let Ok(sb) = Sandbox::get(&d.oc(&a.org)?, &name) {
            // Reconciling someone else's instance would be taking it over.
            if !d.reachable(c, &sb.info()?) {
                return Err(Error::AlreadyExists(name));
            }
        }
        spec.labels.insert(LABEL_OWNER.into(), owner_label(c));
    }
    if let Ok(sb) = Sandbox::get(&d.oc(&a.org)?, &name) {
        let info = sb.info()?;
        // A sandbox spec over the workspace would replace the org's machine.
        if info.config.contains_key(crate::workspace::KEY_WORKSPACE) {
            return Err(Error::invalid(format!(
                "{name} is the org's workspace; pick another name"
            )));
        }
    }
    // incus counts every disk against an org's disk quota, and refuses a
    // root disk without a size there.
    if !spec
        .raw_devices
        .get("root")
        .is_some_and(|r| r.contains_key("size"))
        && workspaces::project_has_disk_limit(&d.client, &org)
    {
        spec.raw_devices
            .entry("root".into())
            .or_default()
            .insert("size".into(), workspaces::SANDBOX_ROOT_SIZE.into());
    }
    // Short-lived by rule: the org's defaults unless the call says otherwise.
    let settings = d.workspaces.settings(&org)?;
    let (expires_at, idle) = crate::workspace::sandbox_deadlines(
        &settings,
        a.expires.as_deref(),
        a.idle_timeout.as_deref(),
        now_secs(),
    )?;
    spec.labels
        .insert("isb.expires_at".into(), expires_at.to_string());
    match idle {
        Some(s) => {
            spec.labels.insert("isb.idle_timeout".into(), s.to_string());
        }
        None => {
            spec.labels.insert("isb.idle_timeout".into(), "0".into());
        }
    }
    let opts = EnsureOptions {
        wait_ready: a.wait_ready.unwrap_or(true),
        ..Default::default()
    };
    let mut log: Vec<String> = Vec::new();
    let (sb, report) = Sandbox::connect_or_create_with_base(
        &d.oc(&a.org)?,
        &spec,
        &Default::default(),
        &base,
        opts,
        &mut |m| log.push(m.to_string()),
    )?;
    d.workspaces.mark_active(&org.incus_project(), &name);
    d.egress.kick();
    Ok(json!({
        "info": sb.info()?,
        "report": report,
        "log": log,
        "expires_at": expires_at,
        "idle_timeout": idle,
        "message": format!(
            "{name} expires {} from now{}; sandbox_extend pushes it out.",
            crate::workspace::human(expires_at.saturating_sub(now_secs())),
            match idle {
                Some(s) => format!(" and is deleted after {} idle", crate::workspace::human(s)),
                None => String::new(),
            }
        ),
    }))
}

/// What `isb.owner` says about a sandbox this caller creates.
fn owner_label(c: &Caller) -> String {
    match c {
        Caller::Superadmin(s) => s.label(),
        Caller::User { principal } if principal.is_workspace() => {
            crate::auth::WORKSPACE_ACTOR.to_string()
        }
        _ => format!("mcp:{}", caller_name(c)),
    }
}

fn sandbox_extend(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        name: String,
        #[serde(default)]
        by: Option<String>,
        #[serde(default)]
        idle_timeout: Option<String>,
        #[serde(default)]
        org: Option<String>,
    }
    let org = arg_org(&a)?;
    let a: A = args(a)?;
    let oc = d.oc(&a.org)?;
    let info = d.reach(c, &oc, &a.name)?;
    let labels: BTreeMap<String, String> = info
        .config
        .iter()
        .filter_map(|(k, v)| k.strip_prefix("user.").map(|k| (k.to_string(), v.clone())))
        .collect();
    if crate::workspace::kind_of(&labels) != "sandbox" {
        return Err(Error::invalid(format!(
            "{} is a {}, not a sandbox: it does not expire",
            a.name,
            crate::workspace::kind_of(&labels)
        )));
    }
    // Its creator, or the org's admins.
    let mine = labels
        .get("isb.owner")
        .is_some_and(|o| *o == owner_label(c));
    let admin = match c {
        Caller::Local { .. } | Caller::Superadmin(_) => true,
        Caller::User { principal } => {
            principal.platform_admin
                || principal
                    .role_in(&org)
                    .is_some_and(|r| r >= crate::auth::Role::Admin)
        }
        _ => false,
    };
    if !mine && !admin {
        return Err(Error::Forbidden(format!(
            "{} was created by {}; its creator or the org's admins extend it",
            a.name,
            labels
                .get("isb.owner")
                .map(String::as_str)
                .unwrap_or("someone else")
        )));
    }
    let now = now_secs();
    let mut patch = serde_json::Map::new();
    let current = labels
        .get("isb.expires_at")
        .and_then(|v| v.parse::<u64>().ok());
    let by = match &a.by {
        Some(b) => crate::flex::parse_duration(b).map_err(Error::invalid)?,
        None if a.idle_timeout.is_some() => Duration::ZERO,
        None => Duration::from_secs(86400),
    };
    let mut expires_at = current;
    if !by.is_zero() {
        let e = crate::workspace::extended(current, by, now)?;
        patch.insert(
            crate::workspace::KEY_EXPIRES_AT.into(),
            json!(e.to_string()),
        );
        expires_at = Some(e);
    }
    let mut idle = labels
        .get("isb.idle_timeout")
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|s| *s > 0);
    if let Some(t) = &a.idle_timeout {
        idle = crate::workspace::idle(t)?.map(|d| d.as_secs());
        patch.insert(
            crate::workspace::KEY_IDLE_TIMEOUT.into(),
            json!(idle.unwrap_or(0).to_string()),
        );
    }
    oc.mutate(
        "PATCH",
        &format!("/1.0/instances/{}", crate::client::encode_segment(&a.name)),
        Some(&json!({"config": patch})),
        &format!("extend sandbox {}", a.name),
        oc.timeouts.other,
    )?;
    d.workspaces.mark_active(&org.incus_project(), &a.name);
    Ok(json!({
        "name": a.name,
        "expires_at": expires_at,
        "idle_timeout": idle,
        "message": format!(
            "{} now expires {} from now.",
            a.name,
            crate::workspace::human(expires_at.unwrap_or(now).saturating_sub(now))
        ),
    }))
}

const OUTPUT_CAP: usize = 256 * 1024;

fn cap(b: &[u8]) -> (String, bool) {
    if b.len() <= OUTPUT_CAP {
        return (String::from_utf8_lossy(b).into_owned(), false);
    }
    (
        String::from_utf8_lossy(&b[b.len() - OUTPUT_CAP..]).into_owned(),
        true,
    )
}

fn sandbox_exec(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    #[derive(Deserialize)]
    struct A {
        name: String,
        #[serde(default)]
        org: Option<String>,
        argv: Vec<String>,
        cwd: Option<String>,
        user: Option<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
        stdin: Option<String>,
        timeout: Option<String>,
    }
    let org = arg_org(&a)?;
    let a: A = args(a)?;
    let oc = d.oc(&a.org)?;
    d.reach(c, &oc, &a.name)?;
    d.workspaces.mark_active(&org.incus_project(), &a.name);
    let timeout = match &a.timeout {
        Some(t) => crate::flex::parse_duration(t).map_err(Error::invalid)?,
        None => Duration::from_secs(600),
    };
    let mut opts = ExecOptions::default().timeout(timeout);
    opts.cwd = a.cwd;
    opts.user = a.user;
    opts.env = a.env;
    if let Some(s) = a.stdin {
        opts.stdin = Stdin::Bytes(s.into_bytes());
    }
    let sb = Sandbox::get(&oc, &a.name)?;
    let out = match sb.exec_with(a.argv, opts) {
        Err(Error::ExecTimeout { .. }) => {
            return Err(Error::invalid(format!(
                "timed out after {timeout:?} and was killed"
            )));
        }
        r => r?,
    };
    let (stdout, t1) = cap(&out.stdout);
    let (stderr, t2) = cap(&out.stderr);
    Ok(
        json!({"exit_code": out.exit_code, "stdout": stdout, "stderr": stderr, "truncated": t1 || t2}),
    )
}

pub use crate::stack::local_deploy_args;

/// Default state directory, exported for the CLI.
pub fn default_state_dir() -> PathBuf {
    Store::default_dir()
}

#[cfg(test)]
#[path = "agent_tests.rs"]
mod agent_tests;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod downscope_tests;
