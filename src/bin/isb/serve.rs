//! `isb serve`: run, install or check the daemon, and its ingress.

use super::*;

#[derive(Args)]
pub(crate) struct ServeArgs {
    #[command(subcommand)]
    pub(crate) action: Option<ServeAction>,
    /// Addresses for remote MCP (`/mcp`), the web UI and `/healthz`
    /// (comma-separated): loopback, or a tailnet address with
    /// --superadmin-tailnet.
    #[arg(long, env = "ISB_SERVE_LISTEN", value_delimiter = ',')]
    pub(crate) listen: Vec<String>,
    /// Unix socket for the local CLI.
    #[arg(long = "serve-socket", env = "ISB_SERVE_SOCKET")]
    pub(crate) serve_socket: Option<PathBuf>,
    /// Where stack definitions are kept.
    #[arg(long, env = "ISB_SERVE_STATE_DIR")]
    pub(crate) state_dir: Option<PathBuf>,
    /// How often each service is reconciled and health-checked.
    #[arg(long, value_parser = dur, default_value = "5s")]
    pub(crate) interval: Duration,
    /// Cloudflare Access team domain (https://TEAM.cloudflareaccess.com).
    #[arg(long, env = "CF_ACCESS_TEAM_DOMAIN")]
    pub(crate) access_team_domain: Option<String>,
    /// Cloudflare Access application audience (AUD tag).
    #[arg(long, env = "CF_ACCESS_AUD")]
    pub(crate) access_aud: Option<String>,
    /// Serve remote MCP with no Access validation (local testing only).
    #[arg(long, env = "ISB_SERVE_ALLOW_UNAUTHENTICATED")]
    pub(crate) allow_unauthenticated: bool,
    /// Tools remote callers may use: names or globs, comma-separated.
    #[arg(long, env = "ISB_SERVE_ALLOW_TOOLS", default_value = "")]
    pub(crate) allow_tools: String,
    /// Tools hidden from remote callers (wins over --allow-tools).
    #[arg(long, env = "ISB_SERVE_DENY_TOOLS", default_value = "")]
    pub(crate) deny_tools: String,
    /// Host directories remote callers may bind-mount from (comma-separated).
    #[arg(long, env = "ISB_SERVE_BIND_ROOTS", value_delimiter = ',')]
    pub(crate) bind_root: Vec<PathBuf>,
    /// Host addresses remote callers may publish ports on, besides loopback.
    #[arg(long, env = "ISB_SERVE_PUBLISH_ADDRESSES", value_delimiter = ',')]
    pub(crate) publish_address: Vec<String>,
    /// Let remote callers create privileged containers.
    #[arg(long, env = "ISB_SERVE_ALLOW_PRIVILEGED")]
    pub(crate) allow_privileged: bool,
    /// Let remote callers use raw_config, raw_devices, incus_profiles,
    /// idmap maps and guest-bound ports.
    #[arg(long, env = "ISB_SERVE_ALLOW_RAW")]
    pub(crate) allow_raw: bool,
    /// Let remote callers reach every instance, not only managed ones.
    #[arg(long, env = "ISB_SERVE_ANY_INSTANCE")]
    pub(crate) any_instance: bool,
    /// Superadmins by tailnet identity: login names (someone@example.com)
    /// and node tags (tag:agents), comma-separated. They get the unix
    /// socket's reach from a tailnet --listen address.
    #[arg(long, env = "ISB_SUPERADMIN_TAILNET", value_name = "LIST")]
    pub(crate) superadmin_tailnet: Option<String>,
    /// Superadmins by Cloudflare Access identity: emails and service token
    /// client ids, comma-separated, exact. Needs Access and --public-url.
    #[arg(long, env = "ISB_SUPERADMIN_ACCESS", value_name = "LIST")]
    pub(crate) superadmin_access: Option<String>,
    /// Where users reach isb (https://isb.example.com), for invitation and
    /// password-reset links.
    #[arg(long, env = "ISB_PUBLIC_URL")]
    pub(crate) public_url: Option<String>,
    /// A browser session ends this long after sign-in.
    #[arg(long, env = "ISB_SESSION_MAX_AGE", value_parser = dur, default_value = "30d")]
    pub(crate) session_max_age: Duration,
    /// A browser session ends after this long unused.
    #[arg(long, env = "ISB_SESSION_IDLE", value_parser = dur, default_value = "7d")]
    pub(crate) session_idle: Duration,
    /// GitHub OAuth app client id (secret: ISB_GITHUB_CLIENT_SECRET in the
    /// environment, or a secret of that name in the default org).
    #[arg(long, env = "ISB_GITHUB_CLIENT_ID")]
    pub(crate) github_client_id: Option<String>,
    /// GitHub Enterprise Server: its web URL (default https://github.com).
    #[arg(long, env = "ISB_GITHUB_URL", hide = true)]
    pub(crate) github_url: Option<String>,
    /// GitHub Enterprise Server: its API URL (default https://api.github.com).
    #[arg(long, env = "ISB_GITHUB_API_URL", hide = true)]
    pub(crate) github_api_url: Option<String>,
    /// Google OAuth client id (secret: ISB_GOOGLE_CLIENT_SECRET, as for GitHub).
    #[arg(long, env = "ISB_GOOGLE_CLIENT_ID")]
    pub(crate) google_client_id: Option<String>,
    /// Generic OpenID Connect issuer (https://idp.example.com).
    #[arg(long, env = "ISB_OIDC_ISSUER")]
    pub(crate) oidc_issuer: Option<String>,
    /// Generic OIDC client id (secret: ISB_OIDC_CLIENT_SECRET, as for GitHub).
    #[arg(long, env = "ISB_OIDC_CLIENT_ID")]
    pub(crate) oidc_client_id: Option<String>,
    /// The generic OIDC button's label (default "SSO").
    #[arg(long, env = "ISB_OIDC_NAME")]
    pub(crate) oidc_name: Option<String>,
    /// Let a verified provider email make an account without an invitation.
    #[arg(long, env = "ISB_OPEN_SIGNUP")]
    pub(crate) open_signup: bool,
    /// How long the audit log keeps entries.
    #[arg(long, value_parser = dur, default_value = "90d", env = "ISB_AUDIT_RETENTION")]
    pub(crate) audit_retention: Duration,
    /// Record read-only tool calls too (secret reads always are).
    #[arg(long, env = "ISB_AUDIT_ALL")]
    pub(crate) audit_all: bool,
    /// How long the history keeps rows (controller and incus events).
    #[arg(long, value_parser = dur, default_value = "365d", env = "ISB_HISTORY_RETENTION")]
    pub(crate) history_retention: Duration,
    /// At most this many history rows; past it, the oldest go first.
    #[arg(long, default_value = "5000000", env = "ISB_HISTORY_MAX_ROWS")]
    pub(crate) history_max_rows: i64,
    /// Ingress: serve stack domains over plain HTTP here (and redirect
    /// HTTPS domains), e.g. 0.0.0.0:80. Turns the ingress on.
    #[arg(long, env = "ISB_INGRESS_HTTP", value_name = "ADDR")]
    pub(crate) ingress_http: Option<String>,
    /// Ingress: serve HTTPS domains here, e.g. 0.0.0.0:443. Turns the
    /// ingress on.
    #[arg(long, env = "ISB_INGRESS_HTTPS", value_name = "ADDR")]
    pub(crate) ingress_https: Option<String>,
    /// Ingress: serve Cloudflare-tunnel orgs even without public listeners.
    #[arg(long, env = "ISB_INGRESS_TUNNELS")]
    pub(crate) ingress_tunnels: bool,
    /// The port each tunnel org's listener takes on its bridge address.
    #[arg(long, env = "ISB_INGRESS_TUNNEL_PORT", default_value_t = isb::ingress::DEFAULT_TUNNEL_PORT)]
    pub(crate) ingress_tunnel_port: u16,
    /// The port each org's workspace reaches isb's MCP on, on its bridge
    /// address (docs/concepts/workspaces.md).
    #[arg(long, env = "ISB_WORKSPACE_MCP_PORT", default_value_t = isb::daemon::workspaces::DEFAULT_PORT)]
    pub(crate) workspace_mcp_port: u16,
    /// The storage pool new workspace homes go in (an org's own setting
    /// wins); default: the org's default pool. Prefer a copy-on-write pool
    /// (zfs, btrfs): on `dir` every home snapshot is a full copy.
    #[arg(long, env = "ISB_WORKSPACE_POOL")]
    pub(crate) workspace_pool: Option<String>,
    /// Make workspace homes host folders, `<DIR>/<org>/home`, bound into
    /// the workspace (the host backs them up, e.g. restic) instead of
    /// managed volumes. An org's home_kind setting can opt out.
    #[arg(long, env = "ISB_WORKSPACE_HOME_ROOT", value_name = "DIR")]
    pub(crate) workspace_home_root: Option<PathBuf>,
    /// The public IPv4 address `host: auto` names resolve to (sslip.io);
    /// default: the default route's source address, if it is public.
    #[arg(long, env = "ISB_INGRESS_PUBLIC_IP", value_name = "IP")]
    pub(crate) ingress_public_ip: Option<String>,
    /// Who certificates come from: letsencrypt (default),
    /// letsencrypt-staging, internal (Caddy's own CA), or an ACME directory URL.
    #[arg(long, env = "ISB_ACME_CA", default_value = "letsencrypt")]
    pub(crate) acme_ca: String,
    /// The ACME account's contact email.
    #[arg(long, env = "ISB_ACME_EMAIL")]
    pub(crate) acme_email: Option<String>,
    /// A Caddy binary to run instead of the pinned release isb downloads.
    #[arg(long, env = "ISB_CADDY_BIN")]
    pub(crate) caddy_bin: Option<PathBuf>,
    /// Run as a server's agent for a control plane (`isb server add` sets
    /// this up): no identity store or web UI, an mTLS listener instead.
    #[arg(long, env = "ISB_AGENT", requires_all = ["agent_listen", "agent_tls"])]
    pub(crate) agent: bool,
    /// The agent's mTLS listener, e.g. 0.0.0.0:7443.
    #[arg(long, env = "ISB_AGENT_LISTEN", requires = "agent")]
    pub(crate) agent_listen: Option<String>,
    /// The agent's TLS directory: ca.crt, tls.crt, tls.key.
    #[arg(long, env = "ISB_AGENT_TLS", requires = "agent")]
    pub(crate) agent_tls: Option<PathBuf>,
}

#[derive(Subcommand)]
pub(crate) enum ServeAction {
    /// Install (or update) `isb serve` as a systemd user service and start it.
    /// On macOS, where the daemon runs in the isb machine, install a
    /// LaunchAgent that starts the machine at login instead.
    Install {
        /// The loopback address to serve on (default: the env file's, else 127.0.0.1:8092).
        #[arg(long)]
        listen: Option<String>,
        /// macOS: the machine the LaunchAgent starts.
        #[arg(long, default_value = isb::machine::DEFAULT_NAME)]
        machine: String,
    },
}

#[expect(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
pub(crate) fn serve(ctx: &Ctx, a: ServeArgs) -> Result<u8> {
    use isb::daemon::{ServeConfig, policy::RemotePolicy};
    if cfg!(target_os = "macos") {
        let Some(ServeAction::Install { listen, machine }) = a.action else {
            return Err(Error::Invalid(
                "on macOS, isb serve runs inside the isb machine, next to incus: create it with \
                 `isb machine init` (`isb machine status` shows its socket), and run \
                 `isb serve install` to start the machine at login"
                    .into(),
            ));
        };
        if listen.is_some() {
            return Err(Error::Invalid(format!(
                "--listen: on macOS the daemon in the machine listens on {}",
                isb::machine::SERVE_LISTEN
            )));
        }
        let r = isb::machine::install_launch_agent(&machine)?;
        println!(
            "installed {} (runs {} machine start {})",
            r.plist.display(),
            r.exe.display(),
            r.machine
        );
        println!("isb serve socket: {}", r.serve_socket.display());
        println!("healthy at {}", r.health_url);
        return Ok(0);
    }
    if let Some(ServeAction::Install { listen, .. }) = a.action {
        let r =
            isb::server::service::install_user_service(&isb::server::service::ServiceOptions {
                listen,
                health_timeout: None,
            })?;
        println!(
            "installed {} (runs {})",
            r.unit_path.display(),
            r.exe.display()
        );
        println!("settings: {}", r.env_path.display());
        println!("healthy at {}", r.health_url);
        if let Some(c) = &r.key_credential {
            println!("secrets key: systemd credential {}", c.display());
        }
        for n in r.notes {
            println!("note: {n}");
        }
        return Ok(0);
    }
    let access = match (a.access_team_domain, a.access_aud) {
        (Some(t), Some(aud)) if !t.is_empty() && !aud.is_empty() => Some((t, aud)),
        (Some(t), None) | (None, Some(t)) if !t.is_empty() => {
            return Err(Error::Invalid(
                "Cloudflare Access needs both CF_ACCESS_TEAM_DOMAIN and CF_ACCESS_AUD".into(),
            ));
        }
        _ => None,
    };
    let cfg = ServeConfig {
        listen: a
            .listen
            .into_iter()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
        socket: a
            .serve_socket
            .unwrap_or_else(isb::server::default_socket_path),
        access,
        allow_unauthenticated: a.allow_unauthenticated,
        remote_tools: isb::server::ToolPolicy::from_lists(&a.allow_tools, &a.deny_tools),
        policy: RemotePolicy {
            allow_privileged: a.allow_privileged,
            allow_raw: a.allow_raw,
            bind_roots: a.bind_root,
            publish_addresses: a.publish_address,
            any_instance: a.any_instance,
        },
        state_dir: a.state_dir.unwrap_or_else(isb::daemon::default_state_dir),
        interval: a.interval,
        keys: isb::secrets::KeySources::from_env(),
        secrets_config: isb::secrets::SecretsConfig::default_path(),
        auth: isb::auth::AuthConfig {
            session_max_age: a.session_max_age,
            session_idle: a.session_idle,
            ..Default::default()
        },
        public_url: a.public_url.filter(|u| !u.is_empty()),
        // Client secrets never come from argv, where `ps` would show them.
        oauth: isb::auth::oauth::OAuthSettings {
            github_client_id: a.github_client_id,
            github_url: a.github_url,
            github_api_url: a.github_api_url,
            google_client_id: a.google_client_id,
            oidc_issuer: a.oidc_issuer,
            oidc_client_id: a.oidc_client_id,
            oidc_name: a.oidc_name,
            ..Default::default()
        }
        .secrets_from_env(),
        open_signup: a.open_signup,
        audit_retention: a.audit_retention,
        audit_all: a.audit_all,
        history_retention: a.history_retention,
        history_max_rows: a.history_max_rows,
        workspace_mcp_port: a.workspace_mcp_port,
        workspace_pool: a.workspace_pool.filter(|p| !p.trim().is_empty()),
        workspace_home_root: match a.workspace_home_root {
            Some(r) if r.as_os_str().is_empty() => None,
            Some(r) if !r.is_absolute() => {
                return Err(Error::Invalid(format!(
                    "--workspace-home-root {}: an absolute path",
                    r.display()
                )));
            }
            r => r,
        },
        ingress: ingress_config(
            a.ingress_http,
            a.ingress_https,
            a.ingress_tunnels,
            a.ingress_tunnel_port,
            a.ingress_public_ip,
            &a.acme_ca,
            a.acme_email,
            a.caddy_bin,
        )?,
        agent: match (a.agent, a.agent_listen, a.agent_tls) {
            (true, Some(listen), Some(tls_dir)) => {
                Some(isb::daemon::AgentConfig { listen, tls_dir })
            }
            _ => None,
        },
        // Given but empty is refused: it would read as "superadmins on"
        // while granting nobody.
        superadmin_tailnet: a
            .superadmin_tailnet
            .as_deref()
            .map(isb::server::tailnet::AllowList::parse)
            .transpose()?,
        superadmin_access: a
            .superadmin_access
            .as_deref()
            .map(isb::daemon::superadmin::AccessAllowList::parse)
            .transpose()?,
    };
    isb::daemon::serve(ctx.client(None), cfg)?;
    Ok(0)
}

/// The ingress settings from `isb serve`'s flags; `None` when it is off.
#[expect(clippy::too_many_arguments)]
pub(crate) fn ingress_config(
    http: Option<String>,
    https: Option<String>,
    tunnels: bool,
    tunnel_port: u16,
    public_ip: Option<String>,
    ca: &str,
    email: Option<String>,
    caddy_bin: Option<PathBuf>,
) -> Result<Option<isb::ingress::IngressConfig>> {
    let addr = |flag: &str, v: Option<String>| -> Result<Option<std::net::SocketAddr>> {
        match v.filter(|s| !s.is_empty()) {
            None => Ok(None),
            Some(s) => {
                // `:80` means every address, as Caddy spells it.
                let full = if s.starts_with(':') {
                    format!("0.0.0.0{s}")
                } else {
                    s
                };
                full.parse().map(Some).map_err(|_| {
                    Error::Invalid(format!("{flag} {full:?}: want IP:PORT, e.g. 0.0.0.0:443"))
                })
            }
        }
    };
    let http = addr("--ingress-http", http)?;
    let https = addr("--ingress-https", https)?;
    if http.is_none() && https.is_none() && !tunnels {
        return Ok(None);
    }
    let public_ip = match public_ip.filter(|s| !s.is_empty()) {
        Some(s) => Some(s.parse().map_err(|_| {
            Error::Invalid(format!("--ingress-public-ip {s:?}: not an IP address"))
        })?),
        None => isb::ingress::detect_public_ip(),
    };
    Ok(Some(isb::ingress::IngressConfig {
        http,
        https,
        ca: isb::ingress::caddy::Ca::parse(ca)?,
        email: email.filter(|e| !e.is_empty()),
        public_ip,
        tunnel_port,
        caddy_bin,
        ..Default::default()
    }))
}

pub(crate) fn ingress_status(json: bool) -> Result<u8> {
    let r = call("ingress_status", serde_json::json!({}), SHORT)?;
    if json {
        print_json(&r);
        return Ok(0);
    }
    if r["enabled"] != true {
        println!("{}", r["message"].as_str().unwrap_or("ingress off"));
        return Ok(0);
    }
    let caddy = &r["caddy"];
    println!(
        "edge: caddy {} {} (http {}, https {}, ca {}){}",
        caddy["version"].as_str().unwrap_or("?"),
        if caddy["running"] == true {
            "running"
        } else {
            "down"
        },
        r["http"].as_str().unwrap_or("-"),
        r["https"].as_str().unwrap_or("-"),
        r["ca"].as_str().unwrap_or("-"),
        r["error"]
            .as_str()
            .or(caddy["last_error"].as_str())
            .map(|e| format!(": {e}"))
            .unwrap_or_default()
    );
    let mut rows = vec![vec![
        "URL".to_string(),
        "STACK".into(),
        "SERVICE".into(),
        "STATE".into(),
        "CERT".into(),
        "UPSTREAMS".into(),
    ]];
    for x in r["routes"].as_array().into_iter().flatten() {
        let d = &x["domain"];
        rows.push(vec![
            d["url"].as_str().unwrap_or("").to_string(),
            format!(
                "{}/{}",
                x["org"].as_str().unwrap_or(""),
                x["stack"].as_str().unwrap_or("")
            ),
            x["service"].as_str().unwrap_or("").into(),
            d["state"].as_str().unwrap_or("").into(),
            d["cert"].as_str().unwrap_or("").into(),
            d["upstreams"]
                .as_array()
                .map(|a| a.len())
                .unwrap_or(0)
                .to_string(),
        ]);
    }
    table(rows);
    for c in r["conflicts"].as_array().into_iter().flatten() {
        eprintln!(
            "conflict: {}/{} {}: {}",
            c["stack"].as_str().unwrap_or(""),
            c["service"].as_str().unwrap_or(""),
            c["host"].as_str().unwrap_or(""),
            c["reason"].as_str().unwrap_or("")
        );
    }
    for c in r["refused"].as_array().into_iter().flatten() {
        eprintln!(
            "refused: {}/{}: {}",
            c["stack"].as_str().unwrap_or(""),
            c["service"].as_str().unwrap_or(""),
            c["reason"].as_str().unwrap_or("")
        );
    }
    for t in r["tunnels"].as_array().into_iter().flatten() {
        eprintln!(
            "tunnel {}: origin {}, cloudflared {}, {}{}",
            t["org"].as_str().unwrap_or(""),
            t["origin"].as_str().unwrap_or("-"),
            if t["stack"] == true {
                "deployed"
            } else {
                "not deployed"
            },
            if t["api_managed"] == true {
                "rules and DNS managed by isb"
            } else {
                "rules and DNS set in the Cloudflare dashboard"
            },
            t["error"]
                .as_str()
                .map(|e| format!(": {e}"))
                .unwrap_or_default()
        );
    }
    Ok(0)
}
