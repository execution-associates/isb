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
#[path = "isb/audit.rs"]
mod audit;
#[path = "isb/auth.rs"]
mod auth;
#[path = "isb/data.rs"]
mod data;
#[path = "isb/host.rs"]
mod host;
#[path = "isb/instances.rs"]
mod instances;
#[path = "isb/kube.rs"]
mod kube;
#[path = "isb/machine.rs"]
mod machine;
#[path = "isb/notify.rs"]
mod notify;
#[path = "isb/org.rs"]
mod org;
#[path = "isb/project.rs"]
mod project;
#[path = "isb/registry.rs"]
mod registry;
#[path = "isb/secret.rs"]
mod secret;
#[path = "isb/serve.rs"]
mod serve;
#[path = "isb/servers.rs"]
mod servers;
#[path = "isb/ssh.rs"]
mod ssh;
#[path = "isb/stack.rs"]
mod stack;
#[path = "isb/templates.rs"]
mod templates;
#[path = "isb/update.rs"]
mod update;

use audit::*;
use auth::*;
use host::*;
use instances::*;
use machine::*;
use org::*;
use project::*;
use registry::*;
use secret::*;
use serve::*;
use stack::*;
#[path = "isb/volumes.rs"]
mod volumes;
#[path = "isb/workspaces.rs"]
mod workspaces;

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

    /// The org to work in: its incus project, `isb-<org>` (`isb org ls`).
    /// Without it, `isb create` and `isb up` make plain sandboxes in incus'
    /// `default` project, outside every org; `--org default` is the
    /// default org, `isb-default`.
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
    /// tunnels (docs/guides/domains.md).
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
    /// Manage an org's secrets on the `isb serve` daemon (docs/guides/secrets.md).
    #[command(subcommand)]
    Secret(SecretCmd),
    /// Projects and their environments, for apps (docs/guides/deploy-apps.md).
    #[command(subcommand)]
    Project(apps::ProjectCmd),
    /// Apps on the `isb serve` daemon: an image or a repository, deployed
    /// into its project environment's stack (docs/guides/deploy-apps.md).
    #[command(subcommand)]
    App(apps::AppCmd),
    /// Instances in the org, like kubectl's pods: ls, get, exec, restart
    /// (docs/guides/kubectl.md).
    #[command(subcommand)]
    Instance(kube::InstanceCmd),
    /// Copy a small file to or from an instance: `isb cp ./f web-1:/etc/f`.
    Cp { src: String, dst: String },
    /// One-click apps: deploy a template from the catalog into a project
    /// environment (docs/guides/templates.md).
    #[command(subcommand)]
    Template(templates::TemplateCmd),
    /// Databases: Postgres, MySQL, MariaDB, MongoDB, Redis as apps with
    /// generated credentials (docs/guides/databases.md).
    #[command(subcommand)]
    Db(data::DbCmd),
    /// Database backups to S3-compatible storage, and restores
    /// (docs/guides/databases.md).
    #[command(subcommand)]
    Backup(data::BackupCmd),
    /// Scheduled jobs: commands on a cron schedule (docs/guides/jobs.md).
    #[command(subcommand)]
    Job(data::JobCmd),
    /// Build a source directory into an image in the org's local registry,
    /// in a fresh sandbox, through `isb serve` (docs/guides/builds.md).
    Build(BuildArgs),
    /// The local OCI registry builds push to and stacks pull from
    /// (docs/guides/builds.md).
    #[command(subcommand)]
    Registry(RegistryCmd),
    /// Notification channels of an org on the `isb serve` daemon: webhook,
    /// Slack, Discord, Telegram, email (docs/guides/notifications.md).
    #[command(subcommand)]
    Notify(notify::NotifyCmd),
    /// Servers this control plane places orgs on: add one over SSH, list,
    /// show, remove, rotate its certificate (docs/guides/servers.md).
    #[command(subcommand)]
    Server(servers::ServerCmd),
    /// The org's workspace on the `isb serve` daemon: its long-lived
    /// machine with a home and an org token, and the sandboxes beside it
    /// (docs/concepts/workspaces.md).
    #[command(subcommand)]
    Workspace(workspaces::WorkspaceCmd),
    /// A live dashboard of stacks and sandboxes (`isb serve`'s view; with no
    /// daemon, sandboxes only).
    Tui,
    /// Replace this isb with the latest release (or VERSION), checked
    /// against the release's SHA256SUMS.
    Update(update::UpdateArgs),
    /// Users of `isb serve` (its identity store, `<state>/isb.db`).
    #[command(subcommand)]
    User(UserCmd),
    /// Invite someone to an org: prints the invitation token, shown once
    /// (and its link when ISB_PUBLIC_URL is set).
    Invite {
        org: String,
        email: String,
        /// viewer, member, admin or owner.
        #[arg(long, default_value = "member")]
        role: String,
        #[command(flatten)]
        db: AuthDb,
    },
    /// API tokens for `isb serve`.
    #[command(subcommand)]
    Token(TokenCmd),
    /// SSH public keys on isb accounts: what `isb ssh-proxy` lets into an
    /// org's instances (docs/guides/ssh.md).
    #[command(subcommand)]
    Key(ssh::KeyCmd),
    /// SSH's stdio over isb serve's websocket to an instance of an org, for
    /// ssh's ProxyCommand (`isb ssh-config` writes it). Nothing listens in
    /// the instance and no port opens anywhere.
    SshProxy {
        /// ORG/INSTANCE, or INSTANCE in --org.
        target: String,
        /// Whose isb SSH keys to let in, for the local socket (which has no
        /// account of its own).
        #[arg(long = "as")]
        keys_of: Option<String>,
        #[command(flatten)]
        remote: ssh::RemoteArgs,
    },
    /// `Host` blocks for ~/.ssh/config (ProxyCommand isb ssh-proxy, the
    /// instance's host key pinned in isb's known_hosts), so plain ssh, scp,
    /// editors and `herdr machine add` reach an org's instances.
    SshConfig(ssh::ConfigArgs),
    /// The audit log of `isb serve` (`<state>/audit.db`): who did what.
    #[command(subcommand)]
    Audit(AuditCmd),
    /// The history of `isb serve`: the controller's events, incus lifecycle
    /// events in every project (with who requested them), audit rows, and
    /// markers for when nothing was watching (docs/operations/history.md).
    History(HistoryArgs),
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
        let org = self
            .global
            .org
            .as_deref()
            .and_then(|o| isb::org::OrgId::new(o).ok());
        let org_project = org.map(|o| o.incus_project());
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

/// Apply `f` to each named sandbox in turn, stopping at the first error.
fn each(ctx: &Ctx, names: Vec<String>, f: impl Fn(&Client, &str) -> Result<()>) -> Result<u8> {
    let c = ctx.client(None);
    for n in names {
        f(&c, &n)?;
    }
    Ok(0)
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

#[expect(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
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
            sandbox_egress,
        }) => host_setup(uplink, user, dry_run, public_ingress, sandbox_egress),
        Cmd::Machine(m) => machine(ctx, m),
        Cmd::Secret(s) => secret(ctx, s),
        Cmd::Project(p) => apps::project(&ctx.global.org, p),
        Cmd::App(a) => apps::app(&ctx.global.org, a),
        Cmd::Instance(c) => kube::instance(&ctx.global.org, c),
        Cmd::Cp { src, dst } => kube::cp(&ctx.global.org, &src, &dst),
        Cmd::Template(t) => templates::template(&ctx.global.org, t),
        Cmd::Db(c) => data::db(&ctx.global.org, c),
        Cmd::Backup(c) => data::backup(&ctx.global.org, c),
        Cmd::Job(c) => data::job(&ctx.global.org, c),
        Cmd::Build(a) => build_cmd(ctx, a),
        Cmd::Registry(r) => registry_cmd(ctx, r),
        Cmd::Notify(n) => notify::notify(&ctx.global.org, n),
        Cmd::Server(c) => servers::server(c),
        Cmd::Workspace(w) => workspaces::workspace(&ctx.global.org, w),
        Cmd::Tui => {
            isb::tui::run(ctx.client(None), isb::server::default_socket_path())?;
            Ok(0)
        }
        Cmd::Update(a) => update::update(ctx, a),
        Cmd::User(c) => user_cmd(c),
        Cmd::Invite {
            org,
            email,
            role,
            db,
        } => invite_cmd(&org, &email, &role, &db),
        Cmd::Token(c) => token_cmd(c),
        Cmd::Key(c) => ssh::key(c),
        Cmd::SshProxy {
            target,
            keys_of,
            remote,
        } => ssh::proxy(&ctx.global.org, &target, keys_of, &remote),
        Cmd::SshConfig(a) => ssh::config(&ctx.global.org, a),
        Cmd::Audit(c) => audit_cmd(c),
        Cmd::History(a) => history_cmd(a),
        Cmd::Create(a) => create(ctx, a),
        Cmd::Start { names } => each(ctx, names, |c, n| Sandbox::get(c, n)?.start()),
        Cmd::Stop {
            names,
            force,
            timeout,
        } => each(ctx, names, |c, n| Sandbox::get(c, n)?.stop(force, timeout)),
        Cmd::Restart { names } => each(ctx, names, |c, n| Sandbox::get(c, n)?.restart()),
        Cmd::Rm { names, force } => each(ctx, names, |c, n| Sandbox::remove(c, n, force)),
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

fn volume_local(ctx: &Ctx, v: VolumeCmd) -> Result<u8> {
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
        _ => unreachable!("isb serve's volume commands are handled in volume()"),
    }
    Ok(0)
}

/// Call a tool on the local daemon.
fn call(tool: &str, args: serde_json::Value, timeout: Duration) -> Result<serde_json::Value> {
    let socket = isb::server::default_socket_path();
    // Inside an org's workspace there is no daemon socket: isb serve's URL
    // (the org bridge's, `$ISB_URL`) with the workspace's token
    // (`$ISB_TOKEN`), as `isb ssh-config` and `isb key` use it.
    if !socket.exists() {
        let remote = ssh::RemoteArgs::default().or_env();
        if remote.url.is_some() {
            let org = args
                .get("org")
                .and_then(serde_json::Value::as_str)
                .map(String::from)
                .or_else(|| std::env::var("ISB_ORG").ok().filter(|o| !o.is_empty()))
                .unwrap_or_else(|| "default".into());
            return remote.remote()?.call_tool(&org, tool, args);
        }
    }
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

// ---- identity: users, invitations, tokens ----
//
// These open `<state>/isb.db` directly rather than calling the daemon: the
// first admin has to exist before anyone can authenticate to the daemon, they
// work while it is down, and the file is the daemon's own (same uid, 0600).
// SQLite in WAL mode lets the daemon and the CLI use it at once, and the
// daemon reads sessions and tokens per request, so changes apply immediately.

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
        // A superadmin token is nobody's and unscoped.
        let c = Cli::try_parse_from(["isb", "token", "create", "agent", "--superadmin"]).unwrap();
        assert!(matches!(
            c.cmd,
            Cmd::Token(TokenCmd::Create {
                superadmin: true,
                ..
            })
        ));
        for extra in [["--org", "ocai"], ["--user", "a@x.io"], ["--scope", "read"]] {
            let mut argv = vec!["isb", "token", "create", "agent", "--superadmin"];
            argv.extend(extra);
            assert!(Cli::try_parse_from(argv).is_err(), "{extra:?}");
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
