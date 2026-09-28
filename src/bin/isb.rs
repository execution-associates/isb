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

/// Declarative incus sandboxes.
///
/// Talks to incusd over its unix socket ($INCUS_SOCKET, else
/// $INCUS_DIR/unix.socket, else /var/lib/incus/unix.socket).
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
    /// Create or reconcile the compose file's sandboxes.
    Up {
        #[command(flatten)]
        f: Files,
        services: Vec<String>,
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
}

#[derive(Args)]
struct CreateArgs {
    name: String,
    /// Image: local alias/fingerprint, or `images:debian/12` style.
    #[arg(short, long)]
    image: String,
    #[arg(long)]
    cpus: Option<String>,
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
    /// `[IP:]HOST:GUEST[/udp]`, or `listen=..,connect=..[,bind=guest][,name=][,search=]`.
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
            | Cmd::Config { f, .. } => Some(f),
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
        if let Some(p) = self.global.project.as_deref().or(project) {
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
            ExitCode::from(1)
        }
    }
}

fn run(ctx: &Ctx, cmd: Cmd) -> Result<u8> {
    match cmd {
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
                Some(p) if p.file.sandboxes.contains_key(&name) => {
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
            prune_devices,
            no_ready,
            json,
            ..
        } => up(ctx, services, prune_devices, no_ready, json),
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
                for s in p.file.sandboxes.keys() {
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
        let (g, vol) = shorthand::volume(v)?;
        spec.volumes.insert(g, vol);
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
            let c = ctx.client(p.file.project.as_deref());
            let all: BTreeMap<String, SandboxInfo> = Sandbox::list(&c)?
                .into_iter()
                .map(|i| (i.name.clone(), i))
                .collect();
            for s in p.select(&services)? {
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
        Some(p) if p.file.sandboxes.contains_key(&a.target) => {
            let spec = p.service(&a.target)?;
            let c = ctx.client(p.file.project.as_deref());
            let name = spec.name.clone().unwrap_or_default();
            let sb = Sandbox::get(&c, &name)?.with_exec_defaults(spec.exec.clone());
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

fn up(
    ctx: &Ctx,
    services: Vec<String>,
    prune_devices: bool,
    no_ready: bool,
    json: bool,
) -> Result<u8> {
    let p = ctx.load()?;
    let opts = EnsureOptions {
        diff: DiffOptions { prune_devices },
        wait_ready: !no_ready,
        ..Default::default()
    };
    let mut rep = ctx.report();
    let reports = compose::up(&ctx.client(None), &p, &services, opts, &mut rep)?;
    if json {
        let r: Vec<_> = reports.iter().map(|(_, r)| r).collect();
        print_json(&r);
    } else {
        for (s, r) in &reports {
            for (dev, listen) in &r.ports {
                println!("{s} {dev} {listen}");
            }
        }
    }
    Ok(0)
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
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
}
