//! The `isb` command line.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use serde::Serialize;

use isb::compose::{self, LoadOptions, Project};
use isb::plan::DiffOptions;
use isb::sandbox::{self, EnsureOptions, LabelFilter, Sandbox, SandboxInfo};
use isb::shorthand;
use isb::spec::{IdmapMode, IdmapRaw, IdmapSpec, InstanceType, SandboxSpec};
use isb::{Client, Error, ExecOptions, Result, Stdin, Timeouts};

#[path = "isb/apps.rs"]
mod apps;
#[path = "isb/notify.rs"]
mod notify;

/// Declarative incus sandboxes.
///
/// Talks to incusd over its unix socket ($INCUS_SOCKET, else
/// $INCUS_DIR/unix.socket, else /var/lib/incus/unix.socket; on macOS, the
/// isb machine's ~/.isb/machine/isb/incus.sock).
#[derive(Parser)]
#[command(name = "isb", version, about, long_about = None, propagate_version = true)]
struct Cli {
    #[command(flatten)]
    global: Global,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args, Clone)]
struct Global {
    /// incusd unix socket.
    #[arg(long, global = true, env = "INCUS_SOCKET", hide_env_values = true)]
    socket: Option<PathBuf>,

    /// incus project (default: `default`, or the compose file's `project`).
    #[arg(long, global = true, env = "INCUS_PROJECT")]
    project: Option<String>,

    /// The org to work in: its incus project (`isb org ls`).
    #[arg(long, global = true, env = "ISB_ORG")]
    org: Option<String>,

    /// Compose file(s), merged in order (default: ./isb.yaml or ./isb.yml).
    /// Also accepted after the compose-aware subcommands.
    #[arg(short = 'f', long = "file")]
    files: Vec<PathBuf>,

    /// dotenv file(s) for ${VAR} interpolation (the environment wins).
    #[arg(long = "env-file", global = true)]
    env_files: Vec<PathBuf>,

    /// Compose project name (default: the file's `name`, else its directory).
    #[arg(short = 'P', long = "project-name", global = true)]
    project_name: Option<String>,

    /// Deadline for creating an instance (e.g. 10m).
    #[arg(long, global = true, value_parser = dur)]
    create_timeout: Option<Duration>,

    /// Suppress progress lines on stderr.
    #[arg(short, long, global = true)]
    quiet: bool,
}

/// `-f FILE` after a compose-aware subcommand (`isb up -f x.yaml`).
#[derive(Args, Clone, Default)]
struct Files {
    /// Compose file(s), merged in order.
    #[arg(short = 'f', long = "file")]
    files: Vec<PathBuf>,
}

fn dur(s: &str) -> std::result::Result<Duration, String> {
    isb::parse_duration(s)
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)] // parsed once per run
enum Cmd {
    /// Create and start a sandbox from flags (fails if it exists, unless --ensure).
    Create(CreateArgs),
    /// Start sandboxes (and wait until running).
    Start { names: Vec<String> },
    /// Stop sandboxes.
    Stop {
        names: Vec<String>,
        /// Kill instead of a clean shutdown.
        #[arg(short, long)]
        force: bool,
        /// Clean shutdown deadline.
        #[arg(short, long, value_parser = dur, default_value = "30s")]
        timeout: Duration,
    },
    /// Restart sandboxes.
    Restart { names: Vec<String> },
    /// Delete sandboxes.
    #[command(alias = "remove", alias = "delete")]
    Rm {
        names: Vec<String>,
        /// Stop running ones first.
        #[arg(short, long)]
        force: bool,
    },
    /// List sandboxes, optionally filtered by label (`key` or `key=value`).
    #[command(alias = "list")]
    Ls {
        #[arg(short, long = "label")]
        labels: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Status of the compose file's sandboxes (or of running sandboxes without one).
    Ps {
        #[command(flatten)]
        f: Files,
        services: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show one sandbox.
    Inspect {
        #[command(flatten)]
        f: Files,
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Run a command in a sandbox (a compose service name or an instance name).
    Exec(ExecArgs),
    /// Manage named volumes.
    #[command(subcommand)]
    Volume(VolumeCmd),
    /// Manage proxy devices on a running sandbox.
    #[command(subcommand)]
    Port(PortCmd),
    /// Manage devices.
    #[command(subcommand)]
    Device(DeviceCmd),
    /// Delete sandboxes whose label value is a host path that no longer exists.
    Prune {
        /// The label holding the host path.
        #[arg(short, long)]
        label: String,
        /// Required: prune by missing path (the only criterion so far).
        #[arg(long)]
        missing_path: bool,
        /// Actually delete (default is a dry run).
        #[arg(short = 'y', long)]
        yes: bool,
        #[arg(long)]
        json: bool,
    },
    /// Print the JSON Schema of the compose file format.
    Schema,
    /// Serve the SDK protocol (line-delimited JSON) on stdin/stdout.
    Rpc,
    /// Create or reconcile the compose file's sandboxes, then hold them in the
    /// foreground: run each `command`, stream its output, and stop the
    /// sandboxes when the commands exit, on Ctrl-C, or when the process that
    /// started isb goes away. `-d` returns once they are up instead.
    Up {
        #[command(flatten)]
        f: Files,
        services: Vec<String>,
        /// Detached: return once the sandboxes are up and leave them running.
        #[arg(short, long)]
        detach: bool,
        /// Don't prefix command output with the service name.
        #[arg(long)]
        no_log_prefix: bool,
        /// Clean-shutdown timeout when stopping, before the sandbox is killed.
        #[arg(short, long, value_parser = dur, default_value = "10s")]
        timeout: Duration,
        /// Remove instance-local devices not in the spec.
        #[arg(long)]
        prune_devices: bool,
        /// Skip readiness checks.
        #[arg(long)]
        no_ready: bool,
        #[arg(long)]
        json: bool,
    },
    /// Delete the compose file's sandboxes.
    Down {
        #[command(flatten)]
        f: Files,
        services: Vec<String>,
        /// Also delete the file's (non-external) named volumes, if unused.
        #[arg(long)]
        volumes: bool,
    },
    /// Show what `up` would change.
    Plan {
        #[command(flatten)]
        f: Files,
        services: Vec<String>,
        #[arg(long)]
        prune_devices: bool,
        #[arg(long)]
        json: bool,
        /// Exit 2 if there are changes.
        #[arg(long)]
        exit_code: bool,
    },
    /// Print the resolved compose file (interpolated, merged, defaults filled).
    Config {
        #[command(flatten)]
        f: Files,
        /// Print only the service names.
        #[arg(long)]
        services: bool,
    },
    /// Recent output of a long-running service (`restart:`): its supervised
    /// command's journal, or an OCI image's console log.
    Logs {
        #[command(flatten)]
        f: Files,
        service: String,
        /// How many lines.
        #[arg(short = 'n', long, default_value = "100")]
        lines: usize,
    },
    /// Run the stack daemon: keeps deployed stacks running, and serves MCP
    /// on a unix socket (for `isb stack`) and on loopback HTTP (for remote
    /// agents behind Cloudflare Access).
    Serve(ServeArgs),
    /// Deploy and manage stacks on the `isb serve` daemon.
    #[command(subcommand)]
    Stack(StackCmd),
    /// Orgs: isolated tenants, each an incus project with its own network.
    #[command(subcommand)]
    Org(OrgCmd),
    /// The ingress of `isb serve`: routed domains, certificates, conflicts,
    /// tunnels (docs/ingress.md).
    Ingress {
        #[arg(long)]
        json: bool,
    },
    /// One-time host preparation (needs root).
    #[command(subcommand)]
    Host(HostCmd),
    /// macOS: the Lima VM that runs incus and isb serve for isb.
    #[command(subcommand)]
    Machine(MachineCmd),
    /// Manage an org's secrets on the `isb serve` daemon (docs/secrets.md).
    #[command(subcommand)]
    Secret(SecretCmd),
    /// Projects and their environments, for apps (docs/apps.md).
    #[command(subcommand)]
    Project(apps::ProjectCmd),
    /// Apps on the `isb serve` daemon: an image or a repository, deployed
    /// into its project environment's stack (docs/apps.md).
    #[command(subcommand)]
    App(apps::AppCmd),
    /// Build a source directory into an image in the org's local registry,
    /// in a fresh sandbox, through `isb serve` (docs/builds.md).
    Build(BuildArgs),
    /// The local OCI registry builds push to and stacks pull from
    /// (docs/builds.md).
    #[command(subcommand)]
    Registry(RegistryCmd),
    /// Notification channels of an org on the `isb serve` daemon: webhook,
    /// Slack, Discord, Telegram, email (docs/notifications.md).
    #[command(subcommand)]
    Notify(notify::NotifyCmd),
    /// A live dashboard of stacks and sandboxes (`isb serve`'s view; with no
    /// daemon, sandboxes only).
    Tui,
    /// Users of `isb serve` (its identity store, `<state>/isb.db`).
    #[command(subcommand)]
    User(UserCmd),
    /// Invite someone to an org: prints the invitation token, shown once
    /// (and its link when ISB_PUBLIC_URL is set).
    Invite {
        org: String,
        email: String,
        /// owner, admin or member.
        #[arg(long, default_value = "member")]
        role: String,
        #[command(flatten)]
        db: AuthDb,
    },
    /// API tokens for `isb serve`.
    #[command(subcommand)]
    Token(TokenCmd),
}

/// The identity store the user/invite/token commands open directly.
#[derive(Args, Clone)]
struct AuthDb {
    /// `isb serve`'s state directory (holds isb.db).
    #[arg(long, env = "ISB_SERVE_STATE_DIR")]
    state_dir: Option<PathBuf>,
}

#[derive(Subcommand)]
enum UserCmd {
    /// Create a user. Prompts for the password on a terminal; otherwise reads
    /// it from the first line of stdin. The first user is always a platform
    /// admin and owner of the default org.
    Create {
        email: String,
        /// Make the user a platform admin (spans every org).
        #[arg(long)]
        admin: bool,
        /// Display name.
        #[arg(long, default_value = "")]
        name: String,
        #[command(flatten)]
        db: AuthDb,
    },
    /// List users and their org memberships.
    Ls {
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        db: AuthDb,
    },
    /// Set a user's password (prompted, or stdin) and end their sessions.
    Passwd {
        email: String,
        #[command(flatten)]
        db: AuthDb,
    },
}

#[derive(Subcommand)]
enum TokenCmd {
    /// Create an API token and print it, once.
    Create {
        name: String,
        /// Confine the token to this org (required unless the user is a
        /// platform admin).
        #[arg(long)]
        org: Option<String>,
        /// Lifetime, e.g. 90d (default: never expires).
        #[arg(long, value_parser = dur)]
        expires: Option<Duration>,
        /// Whose token (default: the only platform admin).
        #[arg(long)]
        user: Option<String>,
        #[command(flatten)]
        db: AuthDb,
    },
    /// List API tokens (metadata only; tokens are never shown again).
    Ls {
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        db: AuthDb,
    },
    /// Revoke API tokens by id.
    Revoke {
        #[arg(required = true)]
        ids: Vec<i64>,
        #[command(flatten)]
        db: AuthDb,
    },
}

#[derive(Args)]
struct ServeArgs {
    #[command(subcommand)]
    action: Option<ServeAction>,
    /// Loopback address for remote MCP (`/mcp`) and `/healthz`.
    #[arg(long, env = "ISB_SERVE_LISTEN")]
    listen: Option<String>,
    /// Unix socket for the local CLI.
    #[arg(long = "serve-socket", env = "ISB_SERVE_SOCKET")]
    serve_socket: Option<PathBuf>,
    /// Where stack definitions are kept.
    #[arg(long, env = "ISB_SERVE_STATE_DIR")]
    state_dir: Option<PathBuf>,
    /// How often each service is reconciled and health-checked.
    #[arg(long, value_parser = dur, default_value = "5s")]
    interval: Duration,
    /// Cloudflare Access team domain (https://TEAM.cloudflareaccess.com).
    #[arg(long, env = "CF_ACCESS_TEAM_DOMAIN")]
    access_team_domain: Option<String>,
    /// Cloudflare Access application audience (AUD tag).
    #[arg(long, env = "CF_ACCESS_AUD")]
    access_aud: Option<String>,
    /// Serve remote MCP with no Access validation (local testing only).
    #[arg(long, env = "ISB_SERVE_ALLOW_UNAUTHENTICATED")]
    allow_unauthenticated: bool,
    /// Tools remote callers may use: names or globs, comma-separated.
    #[arg(long, env = "ISB_SERVE_ALLOW_TOOLS", default_value = "")]
    allow_tools: String,
    /// Tools hidden from remote callers (wins over --allow-tools).
    #[arg(long, env = "ISB_SERVE_DENY_TOOLS", default_value = "")]
    deny_tools: String,
    /// Host directories remote callers may bind-mount from (comma-separated).
    #[arg(long, env = "ISB_SERVE_BIND_ROOTS", value_delimiter = ',')]
    bind_root: Vec<PathBuf>,
    /// Host addresses remote callers may publish ports on, besides loopback.
    #[arg(long, env = "ISB_SERVE_PUBLISH_ADDRESSES", value_delimiter = ',')]
    publish_address: Vec<String>,
    /// Let remote callers create privileged containers.
    #[arg(long, env = "ISB_SERVE_ALLOW_PRIVILEGED")]
    allow_privileged: bool,
    /// Let remote callers use raw_config, raw_devices, incus_profiles,
    /// idmap maps and guest-bound ports.
    #[arg(long, env = "ISB_SERVE_ALLOW_RAW")]
    allow_raw: bool,
    /// Let remote callers reach every instance, not only managed ones.
    #[arg(long, env = "ISB_SERVE_ANY_INSTANCE")]
    any_instance: bool,
    /// Where users reach isb (https://isb.example.com), for invitation and
    /// password-reset links.
    #[arg(long, env = "ISB_PUBLIC_URL")]
    public_url: Option<String>,
    /// A browser session ends this long after sign-in.
    #[arg(long, env = "ISB_SESSION_MAX_AGE", value_parser = dur, default_value = "30d")]
    session_max_age: Duration,
    /// A browser session ends after this long unused.
    #[arg(long, env = "ISB_SESSION_IDLE", value_parser = dur, default_value = "7d")]
    session_idle: Duration,
    /// GitHub OAuth app client id (secret: ISB_GITHUB_CLIENT_SECRET in the
    /// environment, or a secret of that name in the default org).
    #[arg(long, env = "ISB_GITHUB_CLIENT_ID")]
    github_client_id: Option<String>,
    /// GitHub Enterprise Server: its web URL (default https://github.com).
    #[arg(long, env = "ISB_GITHUB_URL", hide = true)]
    github_url: Option<String>,
    /// GitHub Enterprise Server: its API URL (default https://api.github.com).
    #[arg(long, env = "ISB_GITHUB_API_URL", hide = true)]
    github_api_url: Option<String>,
    /// Google OAuth client id (secret: ISB_GOOGLE_CLIENT_SECRET, as for GitHub).
    #[arg(long, env = "ISB_GOOGLE_CLIENT_ID")]
    google_client_id: Option<String>,
    /// Generic OpenID Connect issuer (https://idp.example.com).
    #[arg(long, env = "ISB_OIDC_ISSUER")]
    oidc_issuer: Option<String>,
    /// Generic OIDC client id (secret: ISB_OIDC_CLIENT_SECRET, as for GitHub).
    #[arg(long, env = "ISB_OIDC_CLIENT_ID")]
    oidc_client_id: Option<String>,
    /// The generic OIDC button's label (default "SSO").
    #[arg(long, env = "ISB_OIDC_NAME")]
    oidc_name: Option<String>,
    /// Let a verified provider email make an account without an invitation.
    #[arg(long, env = "ISB_OPEN_SIGNUP")]
    open_signup: bool,
    /// Ingress: serve stack domains over plain HTTP here (and redirect
    /// HTTPS domains), e.g. 0.0.0.0:80. Turns the ingress on.
    #[arg(long, env = "ISB_INGRESS_HTTP", value_name = "ADDR")]
    ingress_http: Option<String>,
    /// Ingress: serve HTTPS domains here, e.g. 0.0.0.0:443. Turns the
    /// ingress on.
    #[arg(long, env = "ISB_INGRESS_HTTPS", value_name = "ADDR")]
    ingress_https: Option<String>,
    /// Ingress: serve Cloudflare-tunnel orgs even without public listeners.
    #[arg(long, env = "ISB_INGRESS_TUNNELS")]
    ingress_tunnels: bool,
    /// The port each tunnel org's listener takes on its bridge address.
    #[arg(long, env = "ISB_INGRESS_TUNNEL_PORT", default_value_t = isb::ingress::DEFAULT_TUNNEL_PORT)]
    ingress_tunnel_port: u16,
    /// The public IPv4 address `host: auto` names resolve to (sslip.io);
    /// default: the default route's source address, if it is public.
    #[arg(long, env = "ISB_INGRESS_PUBLIC_IP", value_name = "IP")]
    ingress_public_ip: Option<String>,
    /// Who certificates come from: letsencrypt (default),
    /// letsencrypt-staging, internal (Caddy's own CA), or an ACME directory URL.
    #[arg(long, env = "ISB_ACME_CA", default_value = "letsencrypt")]
    acme_ca: String,
    /// The ACME account's contact email.
    #[arg(long, env = "ISB_ACME_EMAIL")]
    acme_email: Option<String>,
    /// A Caddy binary to run instead of the pinned release isb downloads.
    #[arg(long, env = "ISB_CADDY_BIN")]
    caddy_bin: Option<PathBuf>,
}

#[derive(Subcommand)]
enum ServeAction {
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

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)] // parsed once per run
enum OrgCmd {
    /// Create an org, or update an existing one's limits.
    Create {
        name: String,
        /// Total CPUs across the org.
        #[arg(long)]
        cpus: Option<u32>,
        /// Total memory, e.g. 16GiB.
        #[arg(long)]
        memory: Option<String>,
        /// Total disk, e.g. 100GiB.
        #[arg(long)]
        disk: Option<String>,
        /// Most instances the org may have.
        #[arg(long)]
        instances: Option<u32>,
        /// CPUs an instance gets when its spec sets none (default 1).
        #[arg(long)]
        default_cpus: Option<u32>,
        /// Memory an instance gets when its spec sets none (default 512MiB).
        #[arg(long)]
        default_memory: Option<String>,
        /// Host directory the org may bind-mount from (repeatable).
        #[arg(long)]
        bind_root: Vec<PathBuf>,
        /// A private destination the org may reach despite the default deny:
        /// CIDR[:PORTS[/tcp|udp]], e.g. 100.79.171.47/32:1080/tcp
        /// (repeatable). Replaces the org's exceptions; `none` clears them.
        #[arg(long, value_name = "DEST")]
        allow_egress: Vec<String>,
        /// A domain suffix the org's services may serve (repeatable):
        /// `example.com` allows it and every name under it,
        /// `*.example.com` wildcard hosts too. Replaces the list; `none`
        /// clears it (any concrete name, no wildcards).
        #[arg(long, value_name = "SUFFIX")]
        allow_domain: Vec<String>,
        /// How the org's domains are reached: `caddy` (the server's public
        /// listeners) or `cloudflare-tunnel` (the org's own tunnel, token
        /// in its secret cloudflare-tunnel-token).
        #[arg(long, value_name = "PROVIDER")]
        ingress: Option<String>,
        /// Cloudflare account id for the tunnel's API calls (default: the
        /// tunnel token's).
        #[arg(long, value_name = "ID")]
        cloudflare_account: Option<String>,
        /// Cloudflare zone id the org's hostnames are in (default: looked up
        /// per hostname).
        #[arg(long, value_name = "ID")]
        cloudflare_zone: Option<String>,
    },
    /// List orgs.
    #[command(alias = "list")]
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// One org's settings and usage.
    Show {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Delete an org (with --force, everything in it).
    #[command(alias = "remove")]
    Rm {
        name: String,
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum MachineCmd {
    /// Create and start the machine: Ubuntu 24.04 with incus, $HOME shared at
    /// the same path, its sockets forwarded under ~/.isb/machine/NAME, and
    /// isb serve running inside.
    Init {
        /// Machine name (also the Lima instance's).
        #[arg(default_value = isb::machine::DEFAULT_NAME)]
        name: String,
        #[arg(long, default_value = "4")]
        cpus: u32,
        #[arg(long, default_value = "4GiB")]
        memory: String,
        /// The VM disk's maximum size (it grows as used).
        #[arg(long, default_value = "10GiB")]
        disk: String,
        /// A Linux (musl) isb binary for the guest, instead of downloading
        /// this version's release.
        #[arg(long)]
        isb_binary: Option<PathBuf>,
        /// Deadline for the first boot (image download, incus install).
        #[arg(long, value_parser = dur, default_value = "20m")]
        timeout: Duration,
    },
    /// Start a stopped machine and wait until incus and isb serve answer.
    Start {
        #[arg(default_value = isb::machine::DEFAULT_NAME)]
        name: String,
    },
    /// Stop the machine (sandboxes and stacks in it stop too).
    Stop {
        #[arg(default_value = isb::machine::DEFAULT_NAME)]
        name: String,
    },
    /// Delete the machine, everything in it, and a LaunchAgent that starts it.
    Rm {
        #[arg(default_value = isb::machine::DEFAULT_NAME)]
        name: String,
    },
    /// The machine's state, resources and sockets.
    Status {
        #[arg(default_value = isb::machine::DEFAULT_NAME)]
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// A shell in the machine, or a command: `isb machine ssh [NAME] -- CMD...`.
    Ssh {
        #[arg(default_value = isb::machine::DEFAULT_NAME)]
        name: String,
        #[arg(last = true)]
        command: Vec<String>,
    },
}

#[derive(Args)]
struct BuildArgs {
    /// The source directory.
    dir: PathBuf,
    /// The app: names the image (`<org>/<app>`) and its build cache.
    #[arg(long)]
    app: String,
    /// railpack (default), nixpacks or dockerfile (default when --dockerfile is given).
    #[arg(long)]
    builder: Option<String>,
    /// Dockerfile path, relative to the directory.
    #[arg(long)]
    dockerfile: Option<String>,
    /// Dockerfile stage to build.
    #[arg(long)]
    target: Option<String>,
    /// Build argument KEY=VALUE (repeatable).
    #[arg(long = "arg")]
    args: Vec<String>,
    /// Tag to push (default latest).
    #[arg(long)]
    tag: Option<String>,
    /// Build from this subdirectory.
    #[arg(long)]
    subdir: Option<String>,
    /// Build in a VM (its own kernel), for code you do not trust.
    #[arg(long)]
    untrusted: bool,
    /// Longest the build may take (default 30m).
    #[arg(long)]
    timeout: Option<String>,
    /// Start the build and print its id without following it.
    #[arg(short, long)]
    detach: bool,
}

#[derive(Subcommand)]
enum RegistryCmd {
    /// Create the local registry (or bring it in line): an OCI container in
    /// the isb-system project, reachable only on 127.0.0.1, with TLS from an
    /// isb CA kept in the state directory. Then run `sudo isb host setup`.
    Setup {
        /// Port on 127.0.0.1.
        #[arg(long, default_value_t = isb::registry::DEFAULT_PORT)]
        port: u16,
        /// Issue a new certificate.
        #[arg(long)]
        renew: bool,
        /// The daemon's state directory (holds the CA).
        #[arg(long, env = "ISB_SERVE_STATE_DIR")]
        state_dir: Option<PathBuf>,
    },
    /// The org's images: apps, tags, digests.
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Delete old images: keep the newest N tags per app and anything a
    /// deployed stack (or its rollback) uses. Platform admins.
    Gc {
        #[arg(long, default_value_t = isb::registry::DEFAULT_KEEP)]
        keep: usize,
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum HostCmd {
    /// Let org bridges through a default-deny host firewall (ufw): DHCP and
    /// DNS to the host, and egress through the uplink. Also makes the
    /// directory service names are published in. Run once, as root.
    Setup {
        /// The uplink interface (default: the default route's).
        #[arg(long)]
        uplink: Option<String>,
        /// The user `isb serve` and `isb org` run as, who writes service
        /// names (default: the user who ran sudo).
        #[arg(long)]
        user: Option<String>,
        /// Print the firewall commands instead of running them.
        #[arg(long)]
        dry_run: bool,
        /// Also prepare a public ingress: open 80 and 443 in ufw, and let
        /// unprivileged users bind them (net.ipv4.ip_unprivileged_port_start=80),
        /// so `isb serve --ingress-http :80 --ingress-https :443` runs as you.
        #[arg(long)]
        public_ingress: bool,
    },
}

#[derive(Subcommand)]
enum StackCmd {
    /// Deploy (or update) a stack from the compose file. Waits for the
    /// rollout unless -d.
    Deploy {
        #[command(flatten)]
        f: Files,
        /// Stack name (default: the compose project name).
        name: Option<String>,
        /// Return once the deployment is accepted.
        #[arg(short, long)]
        detach: bool,
        /// How long to wait for the rollout.
        #[arg(long, default_value = "10m")]
        timeout: String,
    },
    /// List stacks.
    #[command(alias = "list")]
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// A stack's services and replicas.
    Ps {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Remove a stack (its instances and ports; volumes with --volumes).
    #[command(alias = "remove")]
    Rm {
        name: String,
        #[arg(long)]
        volumes: bool,
    },
    /// Go back to the previous deployment.
    Rollback { name: String },
    /// Set replica counts: SERVICE=N ...
    Scale {
        name: String,
        #[arg(required = true)]
        services: Vec<String>,
    },
    /// Replace a service's replicas even though nothing changed (a moved tag).
    Redeploy { name: String, service: String },
    /// Recent output of a service's replicas.
    Logs {
        name: String,
        service: String,
        #[arg(long)]
        slot: Option<u32>,
        #[arg(short = 'n', long, default_value = "100")]
        lines: usize,
    },
    /// The compose file a stack runs, as deployed.
    Config { name: String },
}

#[derive(Subcommand)]
enum SecretCmd {
    /// Create a secret from FILE, or stdin if FILE is - or omitted (fails if
    /// it exists).
    Create {
        name: String,
        file: Option<PathBuf>,
        /// Where it is stored.
        #[arg(long, default_value = "local")]
        driver: String,
        /// A label, k=v (repeatable).
        #[arg(short, long = "label")]
        labels: Vec<String>,
    },
    /// Give a secret a new value (a new version) from FILE or stdin; creates
    /// it if missing.
    Set { name: String, file: Option<PathBuf> },
    /// Write a secret's value to stdout, as is.
    Get { name: String },
    /// List secrets (metadata only).
    #[command(alias = "list")]
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// A secret's metadata (never its value).
    Inspect {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Delete secrets (refused while a deployed stack uses one).
    #[command(alias = "remove")]
    Rm {
        #[arg(required = true)]
        names: Vec<String>,
    },
    /// Encrypt FILE (or stdin) for a compose file's `age:` field, to the
    /// daemon's recipients, or to --recipient keys without a daemon.
    Encrypt {
        file: Option<PathBuf>,
        /// An age (age1...) or SSH public key (repeatable).
        #[arg(short, long = "recipient")]
        recipients: Vec<String>,
    },
    /// Re-encrypt stored values to the current recipients (after changing
    /// ~/.config/isb/secrets.toml and restarting the daemon).
    Reencrypt {
        /// Every org.
        #[arg(long, conflicts_with = "org")]
        all: bool,
    },
    /// Re-read an externally stored secret from its source now (a no-op for
    /// local secrets).
    Refresh { name: String },
}

#[derive(Args)]
struct CreateArgs {
    name: String,
    /// Image: local alias/fingerprint, or `images:debian/12` style.
    #[arg(short, long)]
    image: String,
    /// Number of CPUs (limits.cpu).
    #[arg(long)]
    cpus: Option<String>,
    /// CPUs to pin to, e.g. 0-3 (limits.cpu).
    #[arg(long)]
    cpuset_cpus: Option<String>,
    /// Memory limit: 512m, 8g, 8GiB.
    #[arg(short, long)]
    memory: Option<String>,
    /// Storage pool (`auto`: incus-zfs, else default, else first).
    #[arg(short, long)]
    storage: Option<String>,
    /// `auto`, `none`, `always`, or a raw.idmap value.
    #[arg(long)]
    idmap: Option<String>,
    #[arg(long)]
    privileged: Option<bool>,
    /// Label `key=value` (stored as user.key).
    #[arg(short, long = "label")]
    labels: Vec<String>,
    /// Instance environment `KEY=VALUE`.
    #[arg(short, long = "env")]
    env: Vec<String>,
    /// `SRC:GUEST[:ro,owner=U,device=N]` (SRC is a host path or a volume name).
    #[arg(short, long = "volume")]
    volumes: Vec<String>,
    /// `[IP:]PUBLISHED:TARGET[/udp]` (PUBLISHED may be a range), or
    /// `listen=..,connect=..[,bind=guest][,name=][,search=]`.
    #[arg(short, long = "port")]
    ports: Vec<String>,
    /// Readiness check (repeatable): running, default_route, user_exists=U,
    /// path_writable=P, command=ARG,ARG.
    #[arg(long)]
    ready: Vec<String>,
    #[arg(long)]
    ready_timeout: Option<String>,
    /// Raw config `key=value`.
    #[arg(short, long = "config")]
    config: Vec<String>,
    #[arg(long = "profile")]
    profiles: Vec<String>,
    /// Create a virtual machine.
    #[arg(long)]
    vm: bool,
    /// Reconcile if it exists instead of failing.
    #[arg(long)]
    ensure: bool,
    /// Skip readiness checks.
    #[arg(long)]
    no_ready: bool,
}

#[derive(Args)]
struct ExecArgs {
    #[command(flatten)]
    f: Files,
    /// Compose service or instance name.
    target: String,
    /// Guest user (name, uid or uid:gid).
    #[arg(short, long)]
    user: Option<String>,
    /// Working directory.
    #[arg(short = 'w', long)]
    cwd: Option<String>,
    /// `KEY=VALUE` (repeatable).
    #[arg(short, long = "env")]
    env: Vec<String>,
    /// Run via the user's login shell.
    #[arg(short, long)]
    login: bool,
    /// Force a pseudo-terminal.
    #[arg(short = 't', long, conflicts_with = "no_tty")]
    tty: bool,
    /// Never allocate a pseudo-terminal.
    #[arg(short = 'T', long)]
    no_tty: bool,
    /// Do not forward stdin (the command sees EOF).
    #[arg(short = 'n', long)]
    no_stdin: bool,
    /// Kill the command after this long (default: no limit).
    #[arg(long, value_parser = dur)]
    timeout: Option<Duration>,
    /// The command and its arguments, passed as-is (no shell).
    #[arg(last = true, required = true)]
    argv: Vec<String>,
}

#[derive(Subcommand)]
enum VolumeCmd {
    /// Create a named volume (no-op if it exists).
    Create {
        name: String,
        #[arg(long)]
        pool: Option<String>,
        #[arg(short, long = "config")]
        config: Vec<String>,
    },
    /// List named volumes.
    #[command(alias = "list")]
    Ls {
        #[arg(long)]
        pool: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show a named volume.
    Inspect {
        name: String,
        #[arg(long)]
        pool: Option<String>,
    },
    /// Delete a named volume (refused while in use).
    #[command(alias = "remove")]
    Rm {
        name: String,
        #[arg(long)]
        pool: Option<String>,
    },
}

#[derive(Subcommand)]
enum PortCmd {
    /// Add a proxy device (a correct one is left as is). Prints the listen address.
    Add {
        name: String,
        /// `[IP:]HOST:GUEST[/udp]` or `listen=..,connect=..[,bind=guest][,search=N]`.
        spec: String,
        /// Device name.
        #[arg(long = "name")]
        device: Option<String>,
        /// Step past up to N taken host ports.
        #[arg(long)]
        search: Option<u16>,
    },
    /// Remove proxy devices by name.
    Rm { name: String, devices: Vec<String> },
    /// Print one property of a proxy device (default: its listen address).
    Get {
        name: String,
        device: String,
        /// Property to print (listen, connect, bind, ...).
        #[arg(default_value = "listen")]
        key: String,
    },
    /// List proxy devices.
    Ls {
        name: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum DeviceCmd {
    /// List instance-local devices.
    Ls {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Remove instance-local devices by name.
    Rm { name: String, devices: Vec<String> },
}

impl Cmd {
    fn files(&self) -> Option<&Files> {
        match self {
            Cmd::Ps { f, .. }
            | Cmd::Inspect { f, .. }
            | Cmd::Up { f, .. }
            | Cmd::Down { f, .. }
            | Cmd::Plan { f, .. }
            | Cmd::Config { f, .. }
            | Cmd::Logs { f, .. }
            | Cmd::Stack(StackCmd::Deploy { f, .. }) => Some(f),
            Cmd::Exec(a) => Some(&a.f),
            _ => None,
        }
    }
}

struct Ctx {
    global: Global,
}

impl Ctx {
    fn client(&self, project: Option<&str>) -> Client {
        let mut c = match &self.global.socket {
            Some(s) => Client::with_socket(s),
            None => Client::new(),
        };
        let org_project = self
            .global
            .org
            .as_deref()
            .and_then(|o| isb::org::OrgId::new(o).ok())
            .map(|o| o.incus_project());
        if let Some(p) = self
            .global
            .project
            .as_deref()
            .or(org_project.as_deref())
            .or(project)
        {
            c = c.project(p);
        }
        let mut t = Timeouts::default();
        if let Some(d) = self.global.create_timeout {
            t.create = d;
        }
        c.timeouts(t)
    }

    fn report(&self) -> impl FnMut(&str) + '_ {
        move |line: &str| {
            if !self.global.quiet {
                eprintln!("{line}");
            }
        }
    }

    fn load(&self) -> Result<Project> {
        compose::load(&LoadOptions {
            files: self.global.files.clone(),
            env_files: self.global.env_files.clone(),
            project_name: self.global.project_name.clone(),
            ..Default::default()
        })
    }

    /// The compose project, only if one was named or exists in the cwd.
    fn maybe_load(&self) -> Result<Option<Project>> {
        if self.global.files.is_empty() {
            let cwd = std::env::current_dir()?;
            if compose::find_default(&cwd).is_none() {
                return Ok(None);
            }
        }
        self.load().map(Some)
    }
}

fn print_json<T: Serialize>(v: &T) {
    println!("{}", serde_json::to_string_pretty(v).expect("serializable"));
}

fn table(rows: Vec<Vec<String>>) {
    let n = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut w = vec![0; n];
    for r in &rows {
        for (i, c) in r.iter().enumerate() {
            w[i] = w[i].max(c.chars().count());
        }
    }
    for r in rows {
        let line: Vec<String> = r
            .iter()
            .enumerate()
            .map(|(i, c)| {
                if i + 1 == r.len() {
                    c.clone()
                } else {
                    format!("{c:<width$}", width = w[i])
                }
            })
            .collect();
        println!("{}", line.join("  ").trim_end());
    }
}

fn labels_brief(i: &SandboxInfo) -> String {
    i.labels
        .iter()
        .filter(|(k, _)| !k.starts_with("isb."))
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let mut ctx = Ctx { global: cli.global };
    if let Some(f) = cli.cmd.files() {
        ctx.global.files.extend(f.files.iter().cloned());
    }
    match run(&ctx, cli.cmd) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("isb: {e}");
            if cfg!(target_os = "macos") && matches!(e, Error::Connect { .. }) {
                eprintln!(
                    "isb: on macOS incus runs in the isb machine: `isb machine init` creates \
                     it, `isb machine start` starts it"
                );
            }
            ExitCode::from(1)
        }
    }
}

fn run(ctx: &Ctx, cmd: Cmd) -> Result<u8> {
    match cmd {
        Cmd::Logs { service, lines, .. } => logs(ctx, &service, lines),
        Cmd::Serve(a) => serve(ctx, a),
        Cmd::Stack(s) => stack(ctx, s),
        Cmd::Org(o) => org(ctx, o),
        Cmd::Ingress { json } => ingress_status(json),
        Cmd::Host(HostCmd::Setup {
            uplink,
            user,
            dry_run,
            public_ingress,
        }) => host_setup(uplink, user, dry_run, public_ingress),
        Cmd::Machine(m) => machine(ctx, m),
        Cmd::Secret(s) => secret(ctx, s),
        Cmd::Project(p) => apps::project(&ctx.global.org, p),
        Cmd::App(a) => apps::app(&ctx.global.org, a),
        Cmd::Build(a) => build_cmd(ctx, a),
        Cmd::Registry(r) => registry_cmd(ctx, r),
        Cmd::Notify(n) => notify::notify(&ctx.global.org, n),
        Cmd::Tui => {
            isb::tui::run(ctx.client(None), isb::server::default_socket_path())?;
            Ok(0)
        }
        Cmd::User(c) => user_cmd(c),
        Cmd::Invite {
            org,
            email,
            role,
            db,
        } => invite_cmd(&org, &email, &role, &db),
        Cmd::Token(c) => token_cmd(c),
        Cmd::Create(a) => create(ctx, a),
        Cmd::Start { names } => {
            let c = ctx.client(None);
            for n in names {
                Sandbox::get(&c, &n)?.start()?;
            }
            Ok(0)
        }
        Cmd::Stop {
            names,
            force,
            timeout,
        } => {
            let c = ctx.client(None);
            for n in names {
                Sandbox::get(&c, &n)?.stop(force, timeout)?;
            }
            Ok(0)
        }
        Cmd::Restart { names } => {
            let c = ctx.client(None);
            for n in names {
                Sandbox::get(&c, &n)?.restart()?;
            }
            Ok(0)
        }
        Cmd::Rm { names, force } => {
            let c = ctx.client(None);
            for n in names {
                Sandbox::remove(&c, &n, force)?;
            }
            Ok(0)
        }
        Cmd::Ls { labels, json } => {
            let filters: Vec<LabelFilter> = labels.iter().map(|l| LabelFilter::parse(l)).collect();
            let list = Sandbox::list_with(&ctx.client(None), &filters)?;
            if json {
                print_json(&list);
            } else {
                let mut rows = vec![vec![
                    "NAME".into(),
                    "STATUS".into(),
                    "TYPE".into(),
                    "LABELS".into(),
                ]];
                for i in &list {
                    rows.push(vec![
                        i.name.clone(),
                        i.status.to_uppercase(),
                        i.instance_type.clone(),
                        labels_brief(i),
                    ]);
                }
                table(rows);
            }
            Ok(0)
        }
        Cmd::Ps { services, json, .. } => ps(ctx, services, json),
        Cmd::Inspect { name, json, .. } => {
            let c = ctx.client(None);
            let name = match ctx.maybe_load()? {
                Some(p) if p.file.services.contains_key(&name) => {
                    p.service(&name)?.name.clone().unwrap_or(name)
                }
                _ => name,
            };
            let info = Sandbox::get(&c, &name)?.info()?;
            if json {
                print_json(&info);
            } else {
                print!(
                    "{}",
                    serde_yaml_ng::to_string(&info).map_err(|e| Error::Protocol(e.to_string()))?
                );
            }
            Ok(0)
        }
        Cmd::Exec(a) => exec(ctx, a),
        Cmd::Volume(v) => volume(ctx, v),
        Cmd::Port(p) => port(ctx, p),
        Cmd::Device(d) => device(ctx, d),
        Cmd::Prune {
            label,
            missing_path,
            yes,
            json,
        } => {
            if !missing_path {
                return Err(Error::Invalid(
                    "prune needs a criterion: --missing-path".into(),
                ));
            }
            let c = ctx.client(None);
            let mut rep = ctx.report();
            let mut say = |l: &str| {
                if json { rep(l) } else { println!("{l}") }
            };
            let items = sandbox::prune_missing_path(&c, &label, !yes, &mut say)?;
            if json {
                print_json(&items);
            } else if items.is_empty() {
                println!("nothing to prune");
            } else if !yes {
                println!(
                    "dry run; re-run with -y to delete ({} sandbox(es))",
                    items.len()
                );
            }
            Ok(0)
        }
        Cmd::Rpc => {
            isb::rpc::serve(std::io::stdin().lock(), std::io::stdout(), ctx.client(None))?;
            Ok(0)
        }
        Cmd::Schema => {
            print_json(&isb::spec::compose_schema());
            Ok(0)
        }
        Cmd::Up {
            services,
            detach,
            no_log_prefix,
            timeout,
            prune_devices,
            no_ready,
            json,
            ..
        } => up(
            ctx,
            services,
            UpFlags {
                detach,
                no_log_prefix,
                timeout,
                prune_devices,
                no_ready,
                json,
            },
        ),
        Cmd::Down {
            services, volumes, ..
        } => down(ctx, services, volumes),
        Cmd::Plan {
            services,
            prune_devices,
            json,
            exit_code,
            ..
        } => plan(ctx, services, prune_devices, json, exit_code),
        Cmd::Config { services, .. } => {
            let p = ctx.load()?;
            if services {
                for s in p.file.services.keys() {
                    println!("{s}");
                }
            } else {
                print!("{}", p.to_yaml()?);
            }
            Ok(0)
        }
    }
}

fn create(ctx: &Ctx, a: CreateArgs) -> Result<u8> {
    let mut spec = SandboxSpec::new(&a.name, &a.image);
    spec.cpus = a.cpus;
    spec.cpuset = a.cpuset_cpus;
    spec.memory = a.memory;
    spec.storage = a.storage;
    spec.privileged = a.privileged;
    spec.idmap = a.idmap.map(|s| match s.as_str() {
        "auto" => IdmapSpec::Mode(IdmapMode::Auto),
        "none" => IdmapSpec::Mode(IdmapMode::None),
        "always" => IdmapSpec::Mode(IdmapMode::Always),
        raw => IdmapSpec::Raw(IdmapRaw { raw: raw.into() }),
    });
    if a.vm {
        spec.instance_type = InstanceType::VirtualMachine;
    }
    for l in &a.labels {
        let (k, v) = shorthand::key_value(l)?;
        spec.labels.insert(k, v);
    }
    for e in &a.env {
        let (k, v) = shorthand::key_value(e)?;
        spec.env.insert(k, v);
    }
    for c in &a.config {
        let (k, v) = shorthand::key_value(c)?;
        spec.raw_config.insert(k, v);
    }
    for v in &a.volumes {
        spec.volumes.push(shorthand::volume(v)?);
    }
    for p in &a.ports {
        spec.ports.push(shorthand::port(p)?);
    }
    if !a.ready.is_empty() {
        spec.ready = Some(
            a.ready
                .iter()
                .map(|r| shorthand::ready(r))
                .collect::<Result<_>>()?,
        );
    }
    spec.ready_timeout = a.ready_timeout;
    if !a.profiles.is_empty() {
        spec.profiles = Some(a.profiles);
    }
    let c = ctx.client(None);
    let mut rep = ctx.report();
    let opts = EnsureOptions {
        wait_ready: !a.no_ready,
        ..Default::default()
    };
    if a.ensure {
        Sandbox::connect_or_create_with(&c, &spec, &Default::default(), opts, &mut rep)?;
    } else {
        Sandbox::create_with(&c, &spec, &Default::default(), opts, &mut rep)?;
    }
    Ok(0)
}

fn ps(ctx: &Ctx, services: Vec<String>, json: bool) -> Result<u8> {
    #[derive(Serialize)]
    struct Row {
        service: Option<String>,
        name: String,
        status: String,
    }
    let mut rows = Vec::new();
    match ctx.maybe_load()? {
        Some(p) => {
            let c = ctx.client(p.file.incus_project.as_deref());
            let all: BTreeMap<String, SandboxInfo> = Sandbox::list(&c)?
                .into_iter()
                .map(|i| (i.name.clone(), i))
                .collect();
            for s in p.select_exact(&services)? {
                let name = p.service(&s)?.name.clone().unwrap_or_default();
                let status = all
                    .get(&name)
                    .map(|i| i.status.clone())
                    .unwrap_or_else(|| "missing".into());
                rows.push(Row {
                    service: Some(s),
                    name,
                    status,
                });
            }
        }
        None => {
            if !services.is_empty() {
                return Err(Error::Invalid(
                    "no compose file; `isb ps` without one lists running sandboxes".into(),
                ));
            }
            for i in Sandbox::list(&ctx.client(None))? {
                if i.status.eq_ignore_ascii_case("running") {
                    rows.push(Row {
                        service: None,
                        name: i.name,
                        status: i.status,
                    });
                }
            }
        }
    }
    if json {
        print_json(&rows);
    } else {
        let mut t = vec![vec!["SERVICE".into(), "NAME".into(), "STATUS".into()]];
        for r in rows {
            t.push(vec![
                r.service.unwrap_or_else(|| "-".into()),
                r.name,
                r.status.to_uppercase(),
            ]);
        }
        table(t);
    }
    Ok(0)
}

fn exec(ctx: &Ctx, a: ExecArgs) -> Result<u8> {
    // A compose service if a file was named (or exists here) and defines it;
    // otherwise an instance name.
    let (client, sb) = match ctx.maybe_load()? {
        Some(p) if p.file.services.contains_key(&a.target) => {
            let spec = p.service(&a.target)?;
            let c = ctx.client(p.file.incus_project.as_deref());
            let name = spec.name.clone().unwrap_or_default();
            let sb = Sandbox::get(&c, &name)?.with_exec_defaults(spec.exec_defaults());
            (c, sb)
        }
        Some(p) if !ctx.global.files.is_empty() => {
            return Err(Error::Invalid(format!(
                "no sandbox {:?} in {}",
                a.target,
                p.files_display()
            )));
        }
        _ => {
            let c = ctx.client(None);
            let sb = Sandbox::get(&c, &a.target)?;
            (c, sb)
        }
    };
    let _ = client;
    let tty = if a.tty {
        true
    } else if a.no_tty {
        false
    } else {
        isb::exec::stdio_is_tty()
    };
    let mut env = BTreeMap::new();
    for e in &a.env {
        let (k, v) = shorthand::key_value(e)?;
        env.insert(k, v);
    }
    let (width, height) = isb::exec::terminal_size().unzip();
    let opts = ExecOptions {
        cwd: a.cwd,
        user: a.user,
        env,
        login: a.login.then_some(true),
        tty,
        width,
        height,
        timeout: a.timeout,
        stdin: if a.no_stdin {
            Stdin::Null
        } else {
            Stdin::Inherit
        },
    };
    match sb.attach(a.argv, opts) {
        Ok(code) => Ok(code.clamp(0, 255) as u8),
        Err(e) => {
            eprintln!("isb: {e}");
            // Distinguish isb's own failure from the command's exit status.
            Ok(125)
        }
    }
}

fn volume(ctx: &Ctx, v: VolumeCmd) -> Result<u8> {
    let c = ctx.client(None);
    let pool =
        |p: Option<String>| -> Result<String> { sandbox::host_facts(&c)?.pick_pool(p.as_deref()) };
    match v {
        VolumeCmd::Create {
            name,
            pool: p,
            config,
        } => {
            let pool = pool(p)?;
            let mut cfg = BTreeMap::new();
            for kv in &config {
                let (k, v) = shorthand::key_value(kv)?;
                cfg.insert(k, v);
            }
            let created = isb::volume::ensure(&c, &pool, &name, &cfg)?;
            if !ctx.global.quiet {
                eprintln!(
                    "{name} (pool {pool}): {}",
                    if created { "created" } else { "already exists" }
                );
            }
        }
        VolumeCmd::Ls { pool: p, json } => {
            let pools = match p {
                Some(p) => vec![p],
                None => sandbox::host_facts(&c)?.pools,
            };
            let mut all = Vec::new();
            for p in pools {
                all.extend(isb::volume::list(&c, &p)?);
            }
            if json {
                print_json(&all);
            } else {
                let mut t = vec![vec!["NAME".into(), "POOL".into(), "USED BY".into()]];
                for v in all {
                    t.push(vec![v.name, v.pool, v.used_by.len().to_string()]);
                }
                table(t);
            }
        }
        VolumeCmd::Inspect { name, pool: p } => {
            let pool = pool(p)?;
            let v = isb::volume::get(&c, &pool, &name)?
                .ok_or_else(|| Error::NotFound(format!("volume {name} in pool {pool}")))?;
            print_json(&v);
        }
        VolumeCmd::Rm { name, pool: p } => {
            let pool = pool(p)?;
            isb::volume::remove(&c, &pool, &name)?;
        }
    }
    Ok(0)
}

fn port(ctx: &Ctx, p: PortCmd) -> Result<u8> {
    let c = ctx.client(None);
    match p {
        PortCmd::Add {
            name,
            spec,
            device,
            search,
        } => {
            let mut ps = shorthand::port(&spec)?;
            if device.is_some() {
                ps.name = device;
            }
            if search.is_some() {
                ps.search = search;
            }
            let listen = Sandbox::get(&c, &name)?.add_port(&ps)?;
            println!("{listen}");
        }
        PortCmd::Get { name, device, key } => {
            let info = Sandbox::get(&c, &name)?.info()?;
            let dev = info
                .devices
                .get(&device)
                .filter(|p| p.get("type").map(String::as_str) == Some("proxy"))
                .ok_or_else(|| Error::NotFound(format!("proxy device {device} on {name}")))?;
            let v = dev
                .get(&key)
                .ok_or_else(|| Error::NotFound(format!("property {key} of {name}/{device}")))?;
            println!("{v}");
        }
        PortCmd::Rm { name, devices } => {
            let sb = Sandbox::get(&c, &name)?;
            for d in devices {
                if !sb.remove_device(&d)? && !ctx.global.quiet {
                    eprintln!("{name}: no device {d}");
                }
            }
        }
        PortCmd::Ls { name, json } => {
            let info = Sandbox::get(&c, &name)?.info()?;
            let ports: BTreeMap<_, _> = info
                .devices
                .into_iter()
                .filter(|(_, p)| p.get("type").map(String::as_str) == Some("proxy"))
                .collect();
            if json {
                print_json(&ports);
            } else {
                let mut t = vec![vec![
                    "DEVICE".into(),
                    "BIND".into(),
                    "LISTEN".into(),
                    "CONNECT".into(),
                ]];
                for (n, p) in ports {
                    let g = |k: &str| p.get(k).cloned().unwrap_or_default();
                    t.push(vec![n.clone(), g("bind"), g("listen"), g("connect")]);
                }
                table(t);
            }
        }
    }
    Ok(0)
}

fn device(ctx: &Ctx, d: DeviceCmd) -> Result<u8> {
    let c = ctx.client(None);
    match d {
        DeviceCmd::Ls { name, json } => {
            let info = Sandbox::get(&c, &name)?.info()?;
            if json {
                print_json(&info.devices);
            } else {
                let mut t = vec![vec!["DEVICE".into(), "TYPE".into(), "PROPERTIES".into()]];
                for (n, p) in info.devices {
                    let props = p
                        .iter()
                        .filter(|(k, _)| k.as_str() != "type")
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect::<Vec<_>>()
                        .join(" ");
                    t.push(vec![n, p.get("type").cloned().unwrap_or_default(), props]);
                }
                table(t);
            }
        }
        DeviceCmd::Rm { name, devices } => {
            let sb = Sandbox::get(&c, &name)?;
            for d in devices {
                if !sb.remove_device(&d)? && !ctx.global.quiet {
                    eprintln!("{name}: no device {d}");
                }
            }
        }
    }
    Ok(0)
}

struct UpFlags {
    detach: bool,
    no_log_prefix: bool,
    timeout: Duration,
    prune_devices: bool,
    no_ready: bool,
    json: bool,
}

/// Read the values of the project's store-backed secrets (`external`, `age`,
/// `driver`): through `isb serve` when it answers on its socket (the only
/// way when its key is a systemd credential), else from the store on disk
/// with the daemon's key, looked up as the daemon does. A key is never
/// generated here.
fn read_store_secrets(ctx: &Ctx, p: &mut Project) -> Result<()> {
    use serde_json::json;
    let defs = p.store_backed_secrets();
    if defs.is_empty() {
        return Ok(());
    }
    let org = isb::org::OrgId::new(
        ctx.global
            .org
            .clone()
            .unwrap_or_else(|| isb::org::DEFAULT_ORG.to_string()),
    )?;
    let socket = isb::server::default_socket_path();
    let values = if std::os::unix::net::UnixStream::connect(&socket).is_ok() {
        let r = isb::server::client::call_tool(
            &socket,
            "secret_resolve",
            json!({"org": org, "secrets": defs}),
            SHORT,
        )?;
        r["values"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(k, v)| {
                let b = isb::rpc::b64_decode(v.as_str().unwrap_or_default()).map_err(|_| {
                    Error::Invalid(format!("secret {k:?}: bad value from isb serve"))
                })?;
                Ok((k.clone(), b))
            })
            .collect::<Result<BTreeMap<_, _>>>()?
    } else {
        let config =
            isb::secrets::SecretsConfig::load(&isb::secrets::SecretsConfig::default_path())?;
        // The daemon's state dir, as `isb serve` picks it.
        let state = std::env::var_os("ISB_SERVE_STATE_DIR")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(isb::daemon::default_state_dir);
        let secrets = isb::secrets::Secrets::open_existing(
            &state,
            &isb::secrets::KeySources::from_env(),
            &config,
        )
        .map_err(|e| {
            Error::Invalid(format!(
                "secrets {}: no isb serve on {} and the store cannot be opened here: {e}",
                defs.keys().cloned().collect::<Vec<_>>().join(", "),
                socket.display()
            ))
        })?;
        isb::stack::secrets::resolve(&secrets, &org, &defs)?
    };
    p.store_secrets = compose::SecretValues(values);
    Ok(())
}

fn up(ctx: &Ctx, services: Vec<String>, flags: UpFlags) -> Result<u8> {
    let mut p = ctx.load()?;
    read_store_secrets(ctx, &mut p)?;
    let opts = EnsureOptions {
        diff: DiffOptions {
            prune_devices: flags.prune_devices,
        },
        wait_ready: !flags.no_ready,
        ..Default::default()
    };
    let mut rep = ctx.report();
    let ups = compose::up_handles(&ctx.client(None), &p, &services, opts, &mut rep)?;
    if flags.json {
        let r: Vec<_> = ups.iter().map(|(_, r, _)| r).collect();
        print_json(&r);
    } else {
        for (s, r, _) in &ups {
            for (dev, listen) in &r.ports {
                println!("{s} {dev} {listen}");
            }
        }
    }
    if flags.detach {
        return Ok(0);
    }
    let held = ups
        .into_iter()
        .map(|(s, _, sandbox)| {
            use isb::foreground::Run;
            let spec = p.service(&s)?;
            let oci = isb::plan::ImageSource::parse(&spec.image)?.is_oci();
            let run = match &spec.command {
                _ if oci => Run::Console,
                Some(_) if spec.long_running() => Run::Follow(isb::supervise::follow_argv(&s)),
                Some(argv) => Run::Command(argv.clone()),
                None => Run::Hold,
            };
            Ok(isb::foreground::Service {
                run,
                name: s,
                sandbox,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let fg = isb::foreground::Options {
        log_prefix: !flags.no_log_prefix,
        stop_timeout: flags.timeout,
        ..Default::default()
    };
    isb::foreground::run(&held, fg, &mut rep)
}

fn down(ctx: &Ctx, services: Vec<String>, volumes: bool) -> Result<u8> {
    let p = ctx.load()?;
    let mut rep = ctx.report();
    compose::down(&ctx.client(None), &p, &services, volumes, &mut rep)?;
    Ok(0)
}

fn plan(
    ctx: &Ctx,
    services: Vec<String>,
    prune_devices: bool,
    json: bool,
    exit_code: bool,
) -> Result<u8> {
    let p = ctx.load()?;
    let plans = compose::plan(
        &ctx.client(None),
        &p,
        &services,
        DiffOptions { prune_devices },
    )?;
    let changes = plans.iter().any(|p| !p.is_noop());
    if json {
        print_json(&plans);
    } else {
        for pl in &plans {
            let state = pl.status.clone().unwrap_or_else(|| "missing".into());
            println!("{} ({}):", pl.name, state.to_lowercase());
            if pl.actions.is_empty() {
                println!("  up to date");
            }
            for a in &pl.actions {
                println!("  {a}");
            }
        }
    }
    Ok(if exit_code && changes { 2 } else { 0 })
}

fn logs(ctx: &Ctx, service: &str, lines: usize) -> Result<u8> {
    let p = ctx.load()?;
    let spec = p.service(service)?;
    let c = ctx.client(p.file.incus_project.as_deref());
    let name = spec.name.clone().unwrap_or_default();
    let oci = isb::plan::ImageSource::parse(&spec.image)?.is_oci();
    if !oci && !spec.long_running() {
        return Err(Error::Invalid(format!(
            "{service} is not long-running (no restart), so its output went to `isb up`"
        )));
    }
    let sb = Sandbox::get(&c, &name)?;
    let out = isb::supervise::logs(&sb, service, oci, lines)?;
    println!("{}", out.trim_end());
    Ok(0)
}

fn machine(ctx: &Ctx, cmd: MachineCmd) -> Result<u8> {
    use isb::machine as m;
    let log = |s: &str| {
        if !ctx.global.quiet {
            eprintln!("isb machine: {s}");
        }
    };
    match cmd {
        MachineCmd::Init {
            name,
            cpus,
            memory,
            disk,
            isb_binary,
            timeout,
        } => {
            let st = m::init(
                &m::InitOptions {
                    name,
                    cpus,
                    memory,
                    disk,
                    isb_binary,
                    timeout,
                },
                &log,
            )?;
            print_machine(&st);
        }
        MachineCmd::Start { name } => {
            m::start(&name)?;
            log(&format!("{name} is running"));
        }
        MachineCmd::Stop { name } => m::stop(&name)?,
        MachineCmd::Rm { name } => {
            m::remove(&name)?;
            log(&format!("removed {name}"));
        }
        MachineCmd::Status { name, json } => {
            let st = m::status(&name)?;
            if json {
                print_json(&st);
            } else {
                print_machine(&st);
            }
        }
        MachineCmd::Ssh { name, command } => {
            use std::os::unix::process::CommandExt;
            let err = m::shell_command(&name, &command)?.exec();
            return Err(Error::Invalid(format!("limactl shell {name}: {err}")));
        }
    }
    Ok(0)
}

fn print_machine(st: &isb::machine::Status) {
    let gib = |b: Option<u64>| {
        b.map(|b| format!("{:.1}GiB", b as f64 / (1u64 << 30) as f64))
            .unwrap_or_else(|| "-".into())
    };
    let default = if st.default { " (default)" } else { "" };
    println!("machine    {}{default}", st.name);
    println!("state      {}", st.state);
    println!(
        "resources  {} cpus, {} memory, {} disk{}",
        st.cpus.map(|c| c.to_string()).unwrap_or_else(|| "-".into()),
        gib(st.memory_bytes),
        gib(st.disk_bytes),
        st.arch
            .as_deref()
            .map(|a| format!(", {a}"))
            .unwrap_or_default()
    );
    let incus = match (&st.incus_version, &st.incus_error) {
        (Some(v), _) => format!("incus {v}"),
        (None, Some(e)) => format!("not answering ({e})"),
        (None, None) => "not running".into(),
    };
    println!("incus      {} at {}", incus, st.incus_socket.display());
    println!(
        "isb serve  {} at {}, http://{}",
        if st.serve_ok { "ok" } else { "not answering" },
        st.serve_socket.display(),
        st.serve_listen
    );
    if !st.default {
        println!(
            "use it with: export INCUS_SOCKET={} ISB_SERVE_SOCKET={}",
            st.incus_socket.display(),
            st.serve_socket.display()
        );
    }
}

fn serve(ctx: &Ctx, a: ServeArgs) -> Result<u8> {
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
        listen: a.listen.filter(|l| !l.is_empty()),
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
    };
    isb::daemon::serve(ctx.client(None), cfg)?;
    Ok(0)
}

/// The ingress settings from `isb serve`'s flags; `None` when it is off.
#[allow(clippy::too_many_arguments)]
fn ingress_config(
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

/// Call a tool on the local daemon.
fn call(tool: &str, args: serde_json::Value, timeout: Duration) -> Result<serde_json::Value> {
    let socket = isb::server::default_socket_path();
    isb::server::client::call_tool(&socket, tool, args, timeout).map_err(|e| match e {
        Error::Io(_) | Error::Connect { .. } if cfg!(target_os = "macos") => {
            Error::Invalid(format!(
                "no isb serve on {} ({e}); it runs in the isb machine: `isb machine start`, \
                 or `isb machine init` to create it",
                socket.display()
            ))
        }
        Error::Io(_) | Error::Connect { .. } => Error::Invalid(format!(
            "no isb serve on {} ({e}); start it with `isb serve`, or install it with `isb serve install`",
            socket.display()
        )),
        e => e,
    })
}

const SHORT: Duration = Duration::from_secs(60);

fn stack(ctx: &Ctx, cmd: StackCmd) -> Result<u8> {
    use serde_json::json;
    // Every stack tool takes the org; the global --org picks it.
    let org = ctx.global.org.clone();
    let call = |tool: &str, mut args: serde_json::Value, timeout: Duration| {
        if let Some(o) = &org {
            args["org"] = json!(o);
        }
        call(tool, args, timeout)
    };
    match cmd {
        StackCmd::Deploy {
            name,
            detach,
            timeout,
            ..
        } => {
            let p0 = ctx.load()?;
            let name = name.unwrap_or_else(|| p0.name.clone());
            // Load again under the stack's name, so named volumes are
            // `<stack>_<volume>`, as `isb up -P <stack>` would name them.
            let p = compose::load(&LoadOptions {
                files: p0.files.clone(),
                env_files: ctx.global.env_files.clone(),
                project_name: Some(name.clone()),
                ..Default::default()
            })?;
            let wait_for = isb::parse_duration(&timeout).map_err(Error::Invalid)?;
            let args = isb::daemon::local_deploy_args(&p, &name, !detach, Some(&timeout))?;
            let r = call("stack_deploy", args, wait_for + SHORT)?;
            for c in r["changes"].as_array().into_iter().flatten() {
                eprintln!(
                    "{}: {} (rev {}, {} replicas)",
                    c["service"].as_str().unwrap_or(""),
                    c["change"].as_str().unwrap_or(""),
                    c["rev"].as_str().unwrap_or(""),
                    c["replicas"]
                );
            }
            if detach {
                return Ok(0);
            }
            let st = &r["status"];
            print_stack(st);
            let ok = st["services"]
                .as_array()
                .into_iter()
                .flatten()
                .all(|s| s["state"] == "converged");
            Ok(if ok { 0 } else { 1 })
        }
        StackCmd::Ls { json } => {
            let r = call("stack_list", json!({}), SHORT)?;
            if json {
                print_json(&r["stacks"]);
                return Ok(0);
            }
            let mut rows = vec![vec![
                "NAME".into(),
                "SERVICES".into(),
                "CONVERGED".into(),
                "DEPLOYED BY".into(),
            ]];
            for s in r["stacks"].as_array().into_iter().flatten() {
                rows.push(vec![
                    s["name"].as_str().unwrap_or("").into(),
                    s["services"]
                        .as_array()
                        .map(|a| a.len())
                        .unwrap_or(0)
                        .to_string(),
                    s["converged"].to_string(),
                    s["deployed_by"].as_str().unwrap_or("").into(),
                ]);
            }
            table(rows);
            Ok(0)
        }
        StackCmd::Ps { name, json } => {
            let r = call("stack_status", json!({"name": name}), SHORT)?;
            if json {
                print_json(&r);
            } else {
                print_stack(&r);
            }
            Ok(0)
        }
        StackCmd::Rm { name, volumes } => {
            call(
                "stack_remove",
                json!({"name": name, "volumes": volumes}),
                Duration::from_secs(400),
            )?;
            Ok(0)
        }
        StackCmd::Rollback { name } => {
            let r = call("stack_rollback", json!({"name": name}), SHORT)?;
            for c in r["changes"].as_array().into_iter().flatten() {
                eprintln!(
                    "{}: {}",
                    c["service"].as_str().unwrap_or(""),
                    c["change"].as_str().unwrap_or("")
                );
            }
            Ok(0)
        }
        StackCmd::Scale { name, services } => {
            for s in services {
                let (svc, n) = s
                    .split_once('=')
                    .ok_or_else(|| Error::Invalid(format!("{s:?}: expected SERVICE=REPLICAS")))?;
                let n: u32 = n
                    .parse()
                    .map_err(|_| Error::Invalid(format!("{s:?}: replicas must be a number")))?;
                call(
                    "stack_scale",
                    json!({"name": name, "service": svc, "replicas": n}),
                    SHORT,
                )?;
            }
            Ok(0)
        }
        StackCmd::Redeploy { name, service } => {
            call(
                "stack_redeploy",
                json!({"name": name, "service": service}),
                SHORT,
            )?;
            Ok(0)
        }
        StackCmd::Logs {
            name,
            service,
            slot,
            lines,
        } => {
            let mut a = json!({"name": name, "service": service, "lines": lines});
            if let Some(s) = slot {
                a["slot"] = json!(s);
            }
            let r = call("stack_logs", a, Duration::from_secs(120))?;
            for (inst, text) in r["logs"].as_object().into_iter().flatten() {
                println!("==> {inst} <==");
                println!("{}", text.as_str().unwrap_or("").trim_end());
            }
            Ok(0)
        }
        StackCmd::Config { name } => {
            let r = call("stack_config", json!({"name": name}), SHORT)?;
            let yaml =
                serde_yaml_ng::to_string(&r["file"]).map_err(|e| Error::Invalid(e.to_string()))?;
            print!("{yaml}");
            Ok(0)
        }
    }
}

/// A secret's value from a file, or from stdin for `-` or none. Never argv,
/// which other users can read in /proc and which lands in shell history.
fn read_value(file: Option<&std::path::Path>) -> Result<Vec<u8>> {
    use std::io::{IsTerminal, Read};
    let v = match file {
        Some(p) if p != std::path::Path::new("-") => std::fs::read(p)
            .map_err(|e| Error::Invalid(format!("cannot read {}: {e}", p.display())))?,
        _ => {
            let stdin = std::io::stdin();
            if stdin.is_terminal() {
                eprintln!("reading the value from stdin; end it with Ctrl-D");
            }
            let mut b = Vec::new();
            stdin.lock().read_to_end(&mut b)?;
            b
        }
    };
    if v.is_empty() {
        return Err(Error::Invalid("the value is empty".into()));
    }
    Ok(v)
}

/// `YYYY-MM-DD HH:MM:SSZ` from unix seconds.
fn fmt_time(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn build_cmd(ctx: &Ctx, a: BuildArgs) -> Result<u8> {
    use serde_json::json;
    let org = ctx
        .global
        .org
        .clone()
        .unwrap_or_else(|| isb::org::DEFAULT_ORG.to_string());
    let dir = std::fs::canonicalize(&a.dir)
        .map_err(|e| Error::Invalid(format!("{}: {e}", a.dir.display())))?;
    let mut args = json!({"org": org, "app": a.app, "context": dir, "untrusted": a.untrusted});
    for (k, v) in [
        ("builder", a.builder),
        ("dockerfile", a.dockerfile),
        ("target", a.target),
        ("tag", a.tag),
        ("subdir", a.subdir),
        ("timeout", a.timeout),
    ] {
        if let Some(v) = v {
            args[k] = json!(v);
        }
    }
    let mut bargs = BTreeMap::new();
    for kv in a.args {
        let (k, v) = kv
            .split_once('=')
            .ok_or_else(|| Error::Invalid(format!("--arg {kv:?}: expected KEY=VALUE")))?;
        bargs.insert(k.to_string(), v.to_string());
    }
    args["args"] = json!(bargs);
    let started = call("build_run", args, SHORT)?;
    let id = started["id"].as_str().unwrap_or_default().to_string();
    if a.detach {
        println!("{id}");
        return Ok(0);
    }
    eprintln!("build {id}");
    let mut since = 0u64;
    loop {
        let r = call(
            "build_logs",
            json!({"org": org, "id": id, "since": since, "wait": 20}),
            SHORT,
        )?;
        for l in r["lines"].as_array().into_iter().flatten() {
            if !ctx.global.quiet {
                eprintln!("{}", l.as_str().unwrap_or_default());
            }
        }
        since = r["next"].as_u64().unwrap_or(since);
        match r["state"].as_str() {
            Some("succeeded") => {
                println!("{}", r["image"].as_str().unwrap_or_default());
                return Ok(0);
            }
            Some("failed") => {
                eprintln!(
                    "isb: build failed: {}",
                    r["error"].as_str().unwrap_or("see the log")
                );
                return Ok(1);
            }
            _ => {}
        }
    }
}

fn registry_cmd(ctx: &Ctx, cmd: RegistryCmd) -> Result<u8> {
    use serde_json::json;
    let org = ctx
        .global
        .org
        .clone()
        .unwrap_or_else(|| isb::org::DEFAULT_ORG.to_string());
    match cmd {
        RegistryCmd::Setup {
            port,
            renew,
            state_dir,
        } => {
            let state = state_dir.unwrap_or_else(isb::daemon::default_state_dir);
            let mut rep = ctx.report();
            let info =
                isb::registry::setup(&ctx.client(Some("default")), &state, port, renew, &mut rep)?;
            println!("local registry at {}", info.url());
            let ca = isb::registry::host_ca_path(&info.addr);
            let installed = std::fs::read_to_string(&ca).ok();
            if installed.as_deref() != Some(info.ca_pem.as_str()) {
                println!(
                    "incus does not trust it yet: run `sudo isb host setup` (installs {})",
                    ca.display()
                );
            }
            println!("restart isb serve to have it push there");
            Ok(0)
        }
        RegistryCmd::Ls { json } => {
            let r = call("registry_list", json!({"org": org}), SHORT)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
                return Ok(0);
            }
            println!("{:<24} {:<20} {:<20} PUSHED", "APP", "TAG", "DIGEST");
            for repo in r["repositories"].as_array().into_iter().flatten() {
                for t in repo["tags"].as_array().into_iter().flatten() {
                    let at = t["pushed_at"].as_u64().unwrap_or(0);
                    println!(
                        "{:<24} {:<20} {:<20} {}",
                        repo["app"].as_str().unwrap_or_default(),
                        t["tag"].as_str().unwrap_or_default(),
                        isb::registry::oci::short(t["digest"].as_str().unwrap_or_default()),
                        if at == 0 {
                            "-".to_string()
                        } else {
                            format!("{}s ago", isb::stack::now_secs().saturating_sub(at))
                        }
                    );
                }
            }
            Ok(0)
        }
        RegistryCmd::Gc { keep, dry_run } => {
            let r = call(
                "registry_gc",
                json!({"keep": keep, "dry_run": dry_run}),
                Duration::from_secs(3600),
            )?;
            for l in r["log"].as_array().into_iter().flatten() {
                eprintln!("{}", l.as_str().unwrap_or_default());
            }
            println!(
                "{} {} manifest(s), kept {} tag(s)",
                if dry_run { "would delete" } else { "deleted" },
                r["deleted"].as_array().map(Vec::len).unwrap_or(0),
                r["kept"]
            );
            Ok(0)
        }
    }
}

fn secret(ctx: &Ctx, cmd: SecretCmd) -> Result<u8> {
    // The global --org picks the org; an explicit one matters for --all.
    let org_given = ctx.global.org.is_some();
    let org = ctx
        .global
        .org
        .clone()
        .unwrap_or_else(|| isb::org::DEFAULT_ORG.to_string());
    use serde_json::json;
    use std::io::Write;
    let b64 = isb::rpc::b64_encode;
    match cmd {
        SecretCmd::Create {
            name,
            file,
            driver,
            labels,
        } => {
            let mut l = BTreeMap::new();
            for kv in labels {
                let (k, v) = kv
                    .split_once('=')
                    .ok_or_else(|| Error::Invalid(format!("label {kv:?}: expected k=v")))?;
                l.insert(k.to_string(), v.to_string());
            }
            let v = read_value(file.as_deref())?;
            let m = call(
                "secret_create",
                json!({"org": org, "name": name, "value": b64(&v), "driver": driver, "labels": l}),
                SHORT,
            )?;
            eprintln!("created {name} (version {})", m["version"]);
        }
        SecretCmd::Set { name, file } => {
            let v = read_value(file.as_deref())?;
            let m = call(
                "secret_set",
                json!({"org": org, "name": name, "value": b64(&v)}),
                SHORT,
            )?;
            print_rolled(&name, &m);
        }
        SecretCmd::Get { name } => {
            let r = call("secret_get", json!({"org": org, "name": name}), SHORT)?;
            let v = isb::rpc::b64_decode(r["value"].as_str().unwrap_or_default())?;
            let mut out = std::io::stdout().lock();
            out.write_all(&v)?;
            out.flush()?;
        }
        SecretCmd::Ls { json } => {
            let r = call("secret_list", json!({"org": org}), SHORT)?;
            if json {
                print_json(&r["secrets"]);
                return Ok(0);
            }
            let mut rows = vec![vec![
                "NAME".into(),
                "DRIVER".into(),
                "VERSION".into(),
                "UPDATED".into(),
                "LABELS".into(),
            ]];
            for s in r["secrets"].as_array().into_iter().flatten() {
                let labels: Vec<String> = s["labels"]
                    .as_object()
                    .into_iter()
                    .flatten()
                    .map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or("")))
                    .collect();
                rows.push(vec![
                    s["name"].as_str().unwrap_or("").into(),
                    s["driver"].as_str().unwrap_or("").into(),
                    s["version"].to_string(),
                    fmt_time(s["updated_at"].as_u64().unwrap_or(0)),
                    labels.join(","),
                ]);
            }
            table(rows);
        }
        SecretCmd::Inspect { name, json } => {
            let m = call("secret_inspect", json!({"org": org, "name": name}), SHORT)?;
            if json {
                print_json(&m);
                return Ok(0);
            }
            for k in ["org", "name", "driver", "version"] {
                let v = &m[k];
                println!(
                    "{k:<8} {}",
                    v.as_str().map(String::from).unwrap_or(v.to_string())
                );
            }
            println!(
                "created  {}",
                fmt_time(m["created_at"].as_u64().unwrap_or(0))
            );
            println!(
                "updated  {}",
                fmt_time(m["updated_at"].as_u64().unwrap_or(0))
            );
            for (k, v) in m["labels"].as_object().into_iter().flatten() {
                println!("label    {k}={}", v.as_str().unwrap_or(""));
            }
        }
        SecretCmd::Rm { names } => {
            for name in names {
                call("secret_delete", json!({"org": org, "name": name}), SHORT)?;
            }
        }
        SecretCmd::Encrypt { file, recipients } => {
            let recipients = if recipients.is_empty() {
                let r = call("secret_recipients", json!({"org": org}), SHORT)?;
                r["recipients"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            } else {
                recipients
            };
            let rs = recipients
                .iter()
                .map(|r| isb::secrets::Recipient::parse(r))
                .collect::<Result<Vec<_>>>()?;
            // Encrypted here: the value never reaches the daemon.
            let v = read_value(file.as_deref())?;
            print!("{}", isb::secrets::encrypt_inline(&v, &rs)?);
        }
        SecretCmd::Reencrypt { all } => {
            if all && org_given {
                return Err(Error::Invalid("pass --all or --org, not both".into()));
            }
            let a = if all {
                json!({"all": true})
            } else {
                json!({"org": org})
            };
            let r = call("secret_reencrypt", a, Duration::from_secs(600))?;
            eprintln!(
                "re-encrypted {} value(s) to {} recipient(s)",
                r["reencrypted"],
                r["recipients"].as_array().map(Vec::len).unwrap_or(0)
            );
        }
        SecretCmd::Refresh { name } => {
            let m = call("secret_refresh", json!({"org": org, "name": name}), SHORT)?;
            print_rolled(&name, &m);
        }
    }
    Ok(0)
}

/// A secret's version after set/refresh, and the stacks now rolling to it.
fn print_rolled(name: &str, m: &serde_json::Value) {
    eprintln!("{name}: version {}", m["version"]);
    for s in m["rolled"].as_array().into_iter().flatten() {
        eprintln!("{}: rolling to the new version", s.as_str().unwrap_or(""));
    }
}

fn print_stack(st: &serde_json::Value) {
    let mut rows = vec![vec![
        "SERVICE".into(),
        "STATE".into(),
        "REPLICAS".into(),
        "INSTANCE".into(),
        "STATUS".into(),
        "HEALTH".into(),
        "IP".into(),
    ]];
    for s in st["services"].as_array().into_iter().flatten() {
        let svc = s["service"].as_str().unwrap_or("").to_string();
        let reps = format!("{}/{}", s["healthy"], s["replicas"]);
        let state = s["state"].as_str().unwrap_or("").to_string();
        let insts = s["instances"].as_array().cloned().unwrap_or_default();
        if insts.is_empty() {
            rows.push(vec![
                svc.clone(),
                state.clone(),
                reps.clone(),
                "-".into(),
                "-".into(),
                "-".into(),
                "-".into(),
            ]);
        }
        for (n, i) in insts.iter().enumerate() {
            let first = n == 0;
            rows.push(vec![
                if first { svc.clone() } else { String::new() },
                if first { state.clone() } else { String::new() },
                if first { reps.clone() } else { String::new() },
                i["name"].as_str().unwrap_or("").into(),
                i["status"].as_str().unwrap_or("").to_uppercase(),
                i["health"].as_str().unwrap_or("").into(),
                i["ip"].as_str().unwrap_or("-").into(),
            ]);
        }
    }
    table(rows);
    for s in st["services"].as_array().into_iter().flatten() {
        if let Some(m) = s["message"].as_str() {
            eprintln!("{}: {m}", s["service"].as_str().unwrap_or(""));
        }
        for p in s["ports"].as_array().into_iter().flatten() {
            eprintln!(
                "{}: {} -> :{} ({} backends){}",
                s["service"].as_str().unwrap_or(""),
                p["listen"].as_str().unwrap_or(""),
                p["target"],
                p["backends"].as_array().map(|a| a.len()).unwrap_or(0),
                p["error"]
                    .as_str()
                    .map(|e| format!(": {e}"))
                    .unwrap_or_default()
            );
        }
        for d in s["domains"].as_array().into_iter().flatten() {
            let what = d["url"].as_str().map(String::from).unwrap_or_else(|| {
                format!(
                    "{}{}",
                    d["host"].as_str().unwrap_or(""),
                    d["path"].as_str().filter(|p| *p != "/").unwrap_or("")
                )
            });
            let n = d["upstreams"].as_array().map(|a| a.len()).unwrap_or(0);
            eprintln!(
                "{}: {what} ({}, cert {}, {n} upstreams){}",
                s["service"].as_str().unwrap_or(""),
                d["state"].as_str().unwrap_or(""),
                d["cert"].as_str().unwrap_or(""),
                d["message"]
                    .as_str()
                    .map(|m| format!(": {m}"))
                    .unwrap_or_default()
            );
        }
    }
}

fn ingress_status(json: bool) -> Result<u8> {
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

fn org(ctx: &Ctx, cmd: OrgCmd) -> Result<u8> {
    use isb::org::{self, OrgId, OrgOptions};
    let c = ctx.client(Some("default"));
    let mut rep = ctx.report();
    match cmd {
        OrgCmd::Create {
            name,
            cpus,
            memory,
            disk,
            instances,
            default_cpus,
            default_memory,
            bind_root,
            allow_egress,
            allow_domain,
            ingress,
            cloudflare_account,
            cloudflare_zone,
        } => {
            let domains = if allow_domain.is_empty() {
                None
            } else if allow_domain == ["none"] {
                Some(Vec::new())
            } else {
                Some(allow_domain)
            };
            let id = OrgId::new(name)?;
            let egress = if allow_egress.is_empty() {
                None
            } else if allow_egress == ["none"] {
                Some(Vec::new())
            } else {
                Some(
                    allow_egress
                        .iter()
                        .map(|e| org::Egress::parse(e))
                        .collect::<Result<Vec<_>>>()?,
                )
            };
            let roots = bind_root
                .into_iter()
                .map(|p| {
                    p.canonicalize()
                        .map_err(|e| Error::Invalid(format!("{}: {e}", p.display())))
                })
                .collect::<Result<Vec<_>>>()?;
            let info = org::ensure(
                &c,
                &id,
                &OrgOptions {
                    cpus,
                    memory,
                    disk,
                    instances,
                    default_cpus,
                    default_memory,
                    bind_roots: roots,
                    egress,
                    domains,
                    ingress,
                    cloudflare_account,
                    cloudflare_zone,
                },
                &mut rep,
            )?;
            // The identity store keeps the org list memberships hang off.
            open_auth(&AuthDb {
                state_dir: std::env::var_os("ISB_SERVE_STATE_DIR").map(PathBuf::from),
            })?
            .ensure_org(&info.name)
            .map_err(|e| Error::Invalid(e.to_string()))?;
            println!(
                "{} (project {}, network {} {})",
                info.name,
                info.project,
                info.network.unwrap_or_default(),
                info.subnet.unwrap_or_default()
            );
            Ok(0)
        }
        OrgCmd::Ls { json } => {
            let all = org::list(&c)?;
            if json {
                print_json(&all);
                return Ok(0);
            }
            let mut rows = vec![vec![
                "ORG".into(),
                "PROJECT".into(),
                "NETWORK".into(),
                "INSTANCES".into(),
                "CPUS".into(),
                "MEMORY".into(),
            ]];
            for o in all {
                rows.push(vec![
                    o.name.to_string(),
                    o.project,
                    o.subnet.or(o.network).unwrap_or_else(|| "-".into()),
                    match o.instances_limit {
                        Some(l) => format!("{}/{l}", o.instances),
                        None => o.instances.to_string(),
                    },
                    o.cpus.unwrap_or_else(|| "-".into()),
                    o.memory.unwrap_or_else(|| "-".into()),
                ]);
            }
            table(rows);
            Ok(0)
        }
        OrgCmd::Show { name, json } => {
            let o = org::get(&c, &OrgId::new(name)?)?;
            if json {
                print_json(&o);
            } else {
                println!("org        {}", o.name);
                println!("project    {}", o.project);
                println!(
                    "network    {} {}",
                    o.network.as_deref().unwrap_or("-"),
                    o.subnet.as_deref().unwrap_or("")
                );
                println!(
                    "instances  {}{}",
                    o.instances,
                    o.instances_limit
                        .map(|l| format!(" of {l}"))
                        .unwrap_or_default()
                );
                println!("cpus       {}", o.cpus.as_deref().unwrap_or("unlimited"));
                println!("memory     {}", o.memory.as_deref().unwrap_or("unlimited"));
                println!("disk       {}", o.disk.as_deref().unwrap_or("unlimited"));
                println!(
                    "bind roots {}",
                    if o.bind_roots.is_empty() {
                        "none".to_string()
                    } else {
                        o.bind_roots.join(", ")
                    }
                );
                println!(
                    "egress     {}",
                    if o.egress.is_empty() {
                        "internet only".to_string()
                    } else {
                        format!("internet, {}", o.egress.join(", "))
                    }
                );
                println!(
                    "domains    {}",
                    if o.domains.is_empty() {
                        "any name, no wildcards".to_string()
                    } else {
                        o.domains.join(", ")
                    }
                );
                let mut ing = o.ingress.clone();
                for (k, v) in [
                    ("account", &o.cloudflare_account),
                    ("zone", &o.cloudflare_zone),
                ] {
                    if let Some(v) = v {
                        ing.push_str(&format!(" ({k} {v})"));
                    }
                }
                println!("ingress    {ing}");
                println!(
                    "names      {}",
                    match &o.dns_dir {
                        Some(d) => format!("<service>.<stack>.{}.isb (from {d})", o.name),
                        None if o.name.is_default() => "instances only".to_string(),
                        None => "instances only (service names are off: run `sudo isb host setup`, then `isb org create` again)".to_string(),
                    }
                );
            }
            Ok(0)
        }
        OrgCmd::Rm { name, force } => {
            let id = OrgId::new(name)?;
            org::remove(&c, &id, force, &mut rep)?;
            // Memberships, invitations and tokens for it go with it.
            open_auth(&AuthDb {
                state_dir: std::env::var_os("ISB_SERVE_STATE_DIR").map(PathBuf::from),
            })?
            .delete_org(&id)
            .map_err(|e| Error::Invalid(e.to_string()))?;
            Ok(0)
        }
    }
}

/// The ufw rules org bridges need on a default-deny host, as argv lists.
/// DHCP is not among them: see [`BEFORE_RULES`].
fn host_rules(uplink: &str, public_ingress: bool) -> Vec<Vec<String>> {
    let v = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
    let mut out = vec![
        v("ufw allow in on isbbr+ to any port 53 comment"),
        v(&format!(
            "ufw route allow in on isbbr+ out on {uplink} comment"
        )),
        // A Cloudflare-tunnel org's cloudflared reaches the ingress on its
        // own bridge address; other orgs' ACLs keep them off it.
        v(&format!(
            "ufw allow in on isbbr+ to any port {} proto tcp comment",
            isb::ingress::DEFAULT_TUNNEL_PORT
        )),
    ];
    let mut comments = vec![
        "isb org bridges: DNS",
        "isb org bridges: egress",
        "isb org bridges: tunnel ingress",
    ];
    if public_ingress {
        out.push(v("ufw allow 80/tcp comment"));
        out.push(v("ufw allow 443/tcp comment"));
        comments.extend(["isb ingress: http", "isb ingress: https"]);
    }
    for (r, c) in out.iter_mut().zip(comments) {
        r.push(c.to_string());
    }
    out
}

/// Lets the daemon's user bind 80 and 443 without root or capabilities.
const SYSCTL_PATH: &str = "/etc/sysctl.d/60-isb-ingress.conf";
const SYSCTL_TEXT: &str = "# isb serve's ingress binds 80 and 443 as an ordinary user.\nnet.ipv4.ip_unprivileged_port_start = 80\n";

/// Rules ufw's own commands cannot express, ahead of its defaults.
///
/// - DHCP, ahead of ufw's conntrack-INVALID drop. With br_netfilter on, the
///   bridged copy of a DHCP broadcast is dropped in FORWARD, and once the
///   bridge carries an incus ACL the copy meant for dnsmasq then counts as
///   INVALID; a `ufw allow` rule comes too late to see it.
/// - Traffic between instances of one org. With br_netfilter on, frames
///   bridged within an org's bridge traverse FORWARD, where ufw's routed
///   default-deny drops them (only ICMP got through). `--physdev-is-bridged`
///   matches only traffic that stays on one bridge; traffic between two org
///   bridges is routed, so it stays denied (and the org ACLs reject it too).
const BEFORE_RULES: &str = "# isb org bridges: begin\n\
-A ufw-before-input -i isbbr+ -p udp --dport 67 -j ACCEPT\n\
-A ufw-before-forward -i isbbr+ -o isbbr+ -m physdev --physdev-is-bridged -j ACCEPT\n\
# isb org bridges: end\n";

const BEFORE_RULES_PATH: &str = "/etc/ufw/before.rules";

/// `before.rules` with isb's block inserted before the first
/// `ufw-before-input` rule (or an older block replaced), or `None` when the
/// current block is already there.
fn with_before_rules(text: &str) -> Option<String> {
    if text.contains(BEFORE_RULES) {
        return None;
    }
    const END: &str = "# isb org bridges: end\n";
    if let (Some(a), Some(b)) = (text.find("# isb org bridges: begin"), text.find(END)) {
        if a < b {
            return Some(format!(
                "{}{BEFORE_RULES}{}",
                &text[..a],
                &text[b + END.len()..]
            ));
        }
    }
    let mut out = String::with_capacity(text.len() + BEFORE_RULES.len());
    let mut done = false;
    for line in text.split_inclusive('\n') {
        if !done && line.starts_with("-A ufw-before-input") {
            out.push_str(BEFORE_RULES);
            done = true;
        }
        out.push_str(line);
    }
    done.then_some(out)
}

/// The command that makes the service-name directory: owned by `user`,
/// group `incus` (dnsmasq's) with setgid so what the daemon writes there is
/// readable by dnsmasq and nobody else. Without an `incus` group (dnsmasq as
/// `nobody`), world-readable instead.
fn dns_dir_command(user: &str, incus_group: bool) -> Vec<String> {
    let root = isb::discovery::root().display().to_string();
    let (mode, group) = if incus_group {
        ("2750", isb::discovery::DNSMASQ_GROUP.to_string())
    } else {
        ("0755", user.to_string())
    };
    ["install", "-d", "-m", mode, "-o", user, "-g", &group, &root]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

fn group_exists(name: &str) -> bool {
    std::fs::read_to_string("/etc/group")
        .map(|t| t.lines().any(|l| l.split(':').next() == Some(name)))
        .unwrap_or(false)
}

fn host_setup(
    uplink: Option<String>,
    user: Option<String>,
    dry_run: bool,
    public_ingress: bool,
) -> Result<u8> {
    let uplink = match uplink {
        Some(u) => u,
        None => default_route_iface()
            .ok_or_else(|| Error::Invalid("no default route; pass --uplink".into()))?,
    };
    let user = user
        .or_else(|| std::env::var("SUDO_USER").ok().filter(|u| !u.is_empty()))
        .or_else(|| std::env::var("USER").ok().filter(|u| !u.is_empty()))
        .ok_or_else(|| Error::Invalid("cannot tell who runs isb; pass --user".into()))?;
    let dns_cmd = dns_dir_command(&user, group_exists(isb::discovery::DNSMASQ_GROUP));
    let ufw_active = std::process::Command::new("ufw")
        .arg("status")
        .output()
        .ok()
        .is_some_and(|o| String::from_utf8_lossy(&o.stdout).contains("Status: active"));
    let rules = host_rules(&uplink, public_ingress);
    // The local registry's CA, where the skopeo inside incusd looks for it.
    let registry = isb::registry::info(&Client::new()).unwrap_or_else(|e| {
        eprintln!("isb: cannot read the local registry's settings: {e}");
        None
    });
    let ca_path = registry
        .as_ref()
        .map(|i| (isb::registry::host_ca_path(&i.addr), i.ca_pem.clone()));
    let ca_current = ca_path
        .as_ref()
        .is_some_and(|(p, pem)| std::fs::read_to_string(p).ok().as_deref() == Some(pem.as_str()));
    if dry_run || !rustix::process::geteuid().is_root() {
        if !dry_run {
            eprintln!(
                "isb host setup needs root to change the firewall; run it with sudo, or do this:"
            );
        }
        println!("# the directory service names are published in:");
        println!("{}", dns_cmd.join(" "));
        match &ca_path {
            Some((p, _)) if ca_current => {
                println!("# the local registry's CA is installed at {}", p.display());
            }
            Some((p, _)) => {
                println!(
                    "# trust the local registry (its CA, from incus project {}):",
                    isb::registry::PROJECT
                );
                println!(
                    "install -d -m 0755 {}",
                    p.parent().expect("has a parent").display()
                );
                println!(
                    "incus project get {} user.isb.registry.ca > {} && chmod 0644 {}",
                    isb::registry::PROJECT,
                    p.display(),
                    p.display()
                );
            }
            None => println!("# no local registry yet (isb registry setup); its CA comes later"),
        }
        println!("# in {BEFORE_RULES_PATH}, before the first -A ufw-before-input line:");
        print!("{BEFORE_RULES}");
        for r in &rules {
            println!(
                "{}",
                r.iter()
                    .map(|a| if a.contains(' ') {
                        format!("'{a}'")
                    } else {
                        a.clone()
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
        println!("ufw reload");
        if public_ingress {
            println!("# {SYSCTL_PATH}:");
            print!("{SYSCTL_TEXT}");
            println!("sysctl -p {SYSCTL_PATH}");
        }
        return Ok(if dry_run { 0 } else { 1 });
    }
    if public_ingress {
        std::fs::write(SYSCTL_PATH, SYSCTL_TEXT)?;
        if !std::process::Command::new("sysctl")
            .args(["-p", SYSCTL_PATH])
            .stdout(std::process::Stdio::null())
            .status()?
            .success()
        {
            return Err(Error::Invalid(format!("sysctl -p {SYSCTL_PATH} failed")));
        }
        println!("{SYSCTL_PATH}: ordinary users may bind ports 80 and up");
    }
    if !std::process::Command::new(&dns_cmd[0])
        .args(&dns_cmd[1..])
        .status()?
        .success()
    {
        return Err(Error::Invalid(format!("{} failed", dns_cmd.join(" "))));
    }
    println!(
        "{}: service names for org stacks, written by {user}",
        isb::discovery::root().display()
    );
    if let Some((p, pem)) = &ca_path {
        if !ca_current {
            use std::os::unix::fs::PermissionsExt;
            let dir = p.parent().expect("has a parent");
            std::fs::create_dir_all(dir)?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755))?;
            std::fs::write(p, pem)?;
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o644))?;
            println!(
                "{}: the local registry's CA (incus pulls trust it)",
                p.display()
            );
        }
    }
    if !ufw_active {
        println!("no active ufw: incus' own firewall rules already let org bridges through");
        return Ok(0);
    }
    let text = std::fs::read_to_string(BEFORE_RULES_PATH)?;
    if let Some(new) = with_before_rules(&text) {
        std::fs::write(format!("{BEFORE_RULES_PATH}.isb-backup"), &text)?;
        std::fs::write(BEFORE_RULES_PATH, new)?;
        println!(
            "{BEFORE_RULES_PATH}: wrote isb's DHCP and same-org rules (backup at {BEFORE_RULES_PATH}.isb-backup)"
        );
    }
    for r in rules {
        let st = std::process::Command::new(&r[0]).args(&r[1..]).status()?;
        if !st.success() {
            return Err(Error::Invalid(format!("{} failed", r.join(" "))));
        }
    }
    if !std::process::Command::new("ufw")
        .arg("reload")
        .status()?
        .success()
    {
        return Err(Error::Invalid("ufw reload failed".into()));
    }
    println!(
        "org bridges (isbbr*) may now reach DHCP and DNS on this host and egress through {uplink}"
    );
    Ok(0)
}

fn default_route_iface() -> Option<String> {
    let t = std::fs::read_to_string("/proc/net/route").ok()?;
    t.lines()
        .skip(1)
        .map(|l| l.split_whitespace().collect::<Vec<_>>())
        .find(|f| f.get(1) == Some(&"00000000"))
        .map(|f| f[0].to_string())
}

// ---- identity: users, invitations, tokens ----
//
// These open `<state>/isb.db` directly rather than calling the daemon: the
// first admin has to exist before anyone can authenticate to the daemon, they
// work while it is down, and the file is the daemon's own (same uid, 0600).
// SQLite in WAL mode lets the daemon and the CLI use it at once, and the
// daemon reads sessions and tokens per request, so changes apply immediately.

fn open_auth(db: &AuthDb) -> Result<isb::auth::AuthStore> {
    let dir = db
        .state_dir
        .clone()
        .unwrap_or_else(isb::daemon::default_state_dir);
    let path = isb::auth::db_path(&dir);
    isb::auth::AuthStore::open(&path)
        .map_err(|e| Error::Invalid(format!("open {}: {e}", path.display())))
}

/// A password from the terminal (asked twice, not echoed) or, when stdin is
/// not a terminal, its first line. Never from argv, where it would show up
/// in `ps` and shell history.
fn read_password(prompt: &str) -> Result<String> {
    use std::io::BufRead;
    let stdin = std::io::stdin();
    if !rustix::termios::isatty(&stdin) {
        let mut line = String::new();
        stdin.lock().read_line(&mut line)?;
        let pw = line.trim_end_matches(['\n', '\r']).to_string();
        if pw.is_empty() {
            return Err(Error::Invalid("no password on stdin".into()));
        }
        return Ok(pw);
    }
    let ask = |p: &str| -> Result<String> {
        eprint!("{p}");
        let saved = rustix::termios::tcgetattr(&stdin).map_err(std::io::Error::from)?;
        let mut quiet = saved.clone();
        quiet.local_modes -= rustix::termios::LocalModes::ECHO;
        rustix::termios::tcsetattr(&stdin, rustix::termios::OptionalActions::Now, &quiet)
            .map_err(std::io::Error::from)?;
        let mut line = String::new();
        let r = stdin.lock().read_line(&mut line);
        let _ = rustix::termios::tcsetattr(&stdin, rustix::termios::OptionalActions::Now, &saved);
        eprintln!();
        r?;
        Ok(line.trim_end_matches(['\n', '\r']).to_string())
    };
    let pw = ask(prompt)?;
    if ask("again: ")? != pw {
        return Err(Error::Invalid("the passwords differ".into()));
    }
    Ok(pw)
}

fn user_cmd(c: UserCmd) -> Result<u8> {
    match c {
        UserCmd::Create {
            email,
            admin,
            name,
            db,
        } => {
            let store = open_auth(&db)?;
            let first = store.setup_needed()?;
            let pw = read_password(&format!("password for {email}: "))?;
            let u = if first {
                store.create_first_admin(&email, &name, &pw)?
            } else {
                store.create_user(&email, &name, Some(&pw), admin)?
            };
            println!(
                "created user {} (id {}){}",
                u.email,
                u.id,
                if first {
                    ": platform admin, owner of org default"
                } else if u.platform_admin {
                    ": platform admin"
                } else {
                    ""
                }
            );
            Ok(0)
        }
        UserCmd::Ls { json, db } => {
            let store = open_auth(&db)?;
            let mut out = Vec::new();
            for u in store.list_users()? {
                let m = store.memberships(u.id)?;
                out.push((u, m));
            }
            if json {
                let v: Vec<serde_json::Value> = out
                    .iter()
                    .map(|(u, m)| serde_json::json!({"user": u, "memberships": m}))
                    .collect();
                print_json(&v);
                return Ok(0);
            }
            let mut rows = vec![vec![
                "ID".into(),
                "EMAIL".into(),
                "NAME".into(),
                "FLAGS".into(),
                "ORGS".into(),
            ]];
            for (u, m) in out {
                let mut flags = Vec::new();
                if u.platform_admin {
                    flags.push("platform-admin");
                }
                if u.disabled {
                    flags.push("disabled");
                }
                if !u.has_password {
                    flags.push("no-password");
                }
                rows.push(vec![
                    u.id.to_string(),
                    u.email,
                    u.name,
                    flags.join(","),
                    m.iter()
                        .map(|m| format!("{}:{}", m.org, m.role))
                        .collect::<Vec<_>>()
                        .join(","),
                ]);
            }
            table(rows);
            Ok(0)
        }
        UserCmd::Passwd { email, db } => {
            let store = open_auth(&db)?;
            let u = store
                .user_by_email(&email)?
                .ok_or_else(|| Error::NotFound(format!("user {email}")))?;
            let pw = read_password(&format!("new password for {}: ", u.email))?;
            store.set_password(u.id, &pw)?;
            println!("password set for {}; their sessions have ended", u.email);
            Ok(0)
        }
    }
}

fn invite_cmd(org: &str, email: &str, role: &str, db: &AuthDb) -> Result<u8> {
    let store = open_auth(db)?;
    let org = isb::org::OrgId::new(org)?;
    let role = isb::auth::Role::parse(role)?;
    let n = store.create_invitation(None, &org, email, role)?;
    let days = (n.invitation.expires_at - n.invitation.created_at) / 86400;
    eprintln!(
        "invited {} to org {org} as {role}; valid for {days} days, shown once:",
        n.invitation.email
    );
    match std::env::var("ISB_PUBLIC_URL")
        .ok()
        .filter(|u| !u.is_empty())
    {
        Some(u) => println!("{}/invite#{}", u.trim_end_matches('/'), n.token),
        None => println!("{}", n.token),
    }
    Ok(0)
}

fn token_cmd(c: TokenCmd) -> Result<u8> {
    match c {
        TokenCmd::Create {
            name,
            org,
            expires,
            user,
            db,
        } => {
            let store = open_auth(&db)?;
            let u = match user {
                Some(e) => store
                    .user_by_email(&e)?
                    .ok_or_else(|| Error::NotFound(format!("user {e}")))?,
                None => {
                    let admins: Vec<_> = store
                        .list_users()?
                        .into_iter()
                        .filter(|u| u.platform_admin && !u.disabled)
                        .collect();
                    match <[_; 1]>::try_from(admins) {
                        Ok([u]) => u,
                        Err(v) => {
                            return Err(Error::Invalid(format!(
                                "{} platform admins: say whose token with --user EMAIL",
                                v.len()
                            )));
                        }
                    }
                }
            };
            let org = org.map(isb::org::OrgId::new).transpose()?;
            let t = store.create_api_token(u.id, org.as_ref(), &name, expires)?;
            eprintln!(
                "token {} ({}) for {}{}{}; shown once:",
                t.info.id,
                t.info.name,
                u.email,
                t.info
                    .org
                    .as_ref()
                    .map(|o| format!(", org {o}"))
                    .unwrap_or_else(|| ", all orgs (platform)".into()),
                match t.info.expires_at {
                    Some(e) => format!(", expires in {} days", (e - t.info.created_at) / 86400),
                    None => ", never expires".into(),
                }
            );
            println!("{}", t.token);
            Ok(0)
        }
        TokenCmd::Ls { json, db } => {
            let store = open_auth(&db)?;
            let tokens = store.list_all_api_tokens()?;
            if json {
                print_json(&tokens);
                return Ok(0);
            }
            let emails: BTreeMap<i64, String> = store
                .list_users()?
                .into_iter()
                .map(|u| (u.id, u.email))
                .collect();
            let when = |t: Option<i64>| {
                t.map(|t| fmt_time(t.max(0) as u64))
                    .unwrap_or_else(|| "-".into())
            };
            let mut rows = vec![vec![
                "ID".into(),
                "NAME".into(),
                "USER".into(),
                "ORG".into(),
                "CREATED".into(),
                "LAST USED".into(),
                "EXPIRES".into(),
            ]];
            for t in tokens {
                rows.push(vec![
                    t.id.to_string(),
                    t.name,
                    emails.get(&t.user_id).cloned().unwrap_or_default(),
                    t.org.map(|o| o.to_string()).unwrap_or_else(|| "*".into()),
                    fmt_time((t.created_at).max(0) as u64),
                    when(t.last_used),
                    when(t.expires_at),
                ]);
            }
            table(rows);
            Ok(0)
        }
        TokenCmd::Revoke { ids, db } => {
            let store = open_auth(&db)?;
            let mut code = 0;
            for id in ids {
                if store.revoke_api_token(id)? {
                    println!("revoked token {id}");
                } else {
                    eprintln!("isb: token {id} not found");
                    code = 1;
                }
            }
            Ok(code)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn auth_commands_parse() {
        let c = Cli::try_parse_from([
            "isb",
            "token",
            "create",
            "ci",
            "--org",
            "ocai",
            "--expires",
            "90d",
        ])
        .unwrap();
        match c.cmd {
            Cmd::Token(TokenCmd::Create { org, expires, .. }) => {
                assert_eq!(org.as_deref(), Some("ocai"));
                assert_eq!(expires, Some(Duration::from_secs(90 * 86400)));
            }
            _ => panic!("wrong command"),
        }
        let c = Cli::try_parse_from(["isb", "invite", "ocai", "a@x.io"]).unwrap();
        assert!(matches!(c.cmd, Cmd::Invite { ref role, .. } if role == "member"));
        // No way to pass a password on the command line.
        assert!(
            Cli::try_parse_from(["isb", "user", "create", "a@x.io", "--password", "x"]).is_err()
        );
        assert_eq!(fmt_time(1_800_000_000), "2027-01-15 08:00:00Z");
    }

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn secret_commands_parse() {
        let c = Cli::try_parse_from([
            "isb", "secret", "create", "db", "-", "--label", "a=b", "--org", "ocai",
        ])
        .unwrap();
        match c.cmd {
            Cmd::Secret(SecretCmd::Create {
                name,
                file,
                driver,
                labels,
            }) => {
                assert_eq!((name.as_str(), driver.as_str()), ("db", "local"));
                assert_eq!(file, Some(PathBuf::from("-")));
                assert_eq!(labels, vec!["a=b".to_string()]);
                assert_eq!(c.global.org.as_deref(), Some("ocai"));
            }
            _ => unreachable!(),
        }
        let ls = Cli::try_parse_from(["isb", "secret", "ls"]).unwrap();
        assert!(ls.global.org.is_none());
        assert!(Cli::try_parse_from(["isb", "secret", "reencrypt", "--all"]).is_ok());
        // A value is never an argument.
        assert!(Cli::try_parse_from(["isb", "secret", "set", "db", "file", "extra"]).is_err());
    }

    #[test]
    fn times_format() {
        assert_eq!(fmt_time(0), "1970-01-01 00:00:00Z");
        assert_eq!(fmt_time(1_791_000_000), "2026-10-03 04:00:00Z");
        assert_eq!(fmt_time(951_825_600), "2000-02-29 12:00:00Z");
    }

    #[test]
    fn file_flag_works_before_and_after_subcommands() {
        let c = Cli::try_parse_from(["isb", "-f", "a.yaml", "up", "-f", "b.yaml"]).unwrap();
        assert_eq!(c.global.files, vec![PathBuf::from("a.yaml")]);
        assert_eq!(c.cmd.files().unwrap().files, vec![PathBuf::from("b.yaml")]);
        let c = Cli::try_parse_from(["isb", "rm", "-f", "x"]).unwrap();
        assert!(matches!(c.cmd, Cmd::Rm { force: true, .. }));
        let c =
            Cli::try_parse_from(["isb", "exec", "-f", "c.yaml", "web", "--", "ls", "-la"]).unwrap();
        match c.cmd {
            Cmd::Exec(a) => assert_eq!(a.argv, vec!["ls", "-la"]),
            _ => unreachable!(),
        }
    }

    #[test]
    fn before_rules_insertion() {
        let t = "*filter\n:ufw-before-input - [0:0]\n# allow all on loopback\n-A ufw-before-input -i lo -j ACCEPT\nCOMMIT\n";
        let out = with_before_rules(t).unwrap();
        let block = out.find("# isb org bridges: begin").unwrap();
        assert!(block < out.find("-A ufw-before-input -i lo").unwrap());
        assert!(with_before_rules(&out).is_none());
        assert!(with_before_rules("no rules here\n").is_none());
        // An older block (DHCP only) is replaced in place, once.
        let old = "*filter\n# isb org bridges: begin\n-A ufw-before-input -i isbbr+ -p udp --dport 67 -j ACCEPT\n# isb org bridges: end\n-A ufw-before-input -i lo -j ACCEPT\nCOMMIT\n";
        let up = with_before_rules(old).unwrap();
        assert!(up.contains("--physdev-is-bridged"));
        assert_eq!(up.matches("# isb org bridges: begin").count(), 1);
        assert!(up.ends_with("-A ufw-before-input -i lo -j ACCEPT\nCOMMIT\n"));
        assert!(with_before_rules(&up).is_none());
    }
}
