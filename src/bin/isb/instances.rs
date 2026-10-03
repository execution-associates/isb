//! `isb create`, `ps`, `exec` and the `volume`, `port` and `device` commands: sandboxes one at a time.

use super::*;

#[derive(Args)]
pub(crate) struct CreateArgs {
    pub(crate) name: String,
    /// Image: local alias/fingerprint, or `images:debian/12` style.
    #[arg(short, long)]
    pub(crate) image: String,
    /// Number of CPUs (limits.cpu).
    #[arg(long)]
    pub(crate) cpus: Option<String>,
    /// CPUs to pin to, e.g. 0-3 (limits.cpu).
    #[arg(long)]
    pub(crate) cpuset_cpus: Option<String>,
    /// Memory limit: 512m, 8g, 8GiB.
    #[arg(short, long)]
    pub(crate) memory: Option<String>,
    /// Storage pool (`auto`: incus-zfs, else default, else first).
    #[arg(short, long)]
    pub(crate) storage: Option<String>,
    /// `auto`, `none`, `always`, or a raw.idmap value.
    #[arg(long)]
    pub(crate) idmap: Option<String>,
    #[arg(long)]
    pub(crate) privileged: Option<bool>,
    /// Label `key=value` (stored as user.key).
    #[arg(short, long = "label")]
    pub(crate) labels: Vec<String>,
    /// Instance environment `KEY=VALUE`.
    #[arg(short, long = "env")]
    pub(crate) env: Vec<String>,
    /// `SRC:GUEST[:ro,owner=U,device=N]` (SRC is a host path or a volume name).
    #[arg(short, long = "volume")]
    pub(crate) volumes: Vec<String>,
    /// `[IP:]PUBLISHED:TARGET[/udp]` (PUBLISHED may be a range), or
    /// `listen=..,connect=..[,bind=guest][,name=][,search=]`.
    #[arg(short, long = "port")]
    pub(crate) ports: Vec<String>,
    /// Readiness check (repeatable): running, default_route, user_exists=U,
    /// path_writable=P, command=ARG,ARG.
    #[arg(long)]
    pub(crate) ready: Vec<String>,
    #[arg(long)]
    pub(crate) ready_timeout: Option<String>,
    /// Raw config `key=value`.
    #[arg(short, long = "config")]
    pub(crate) config: Vec<String>,
    #[arg(long = "profile")]
    pub(crate) profiles: Vec<String>,
    /// Create a virtual machine.
    #[arg(long)]
    pub(crate) vm: bool,
    /// Reconcile if it exists instead of failing.
    #[arg(long)]
    pub(crate) ensure: bool,
    /// Skip readiness checks.
    #[arg(long)]
    pub(crate) no_ready: bool,
}

#[derive(Args)]
pub(crate) struct ExecArgs {
    #[command(flatten)]
    pub(crate) f: Files,
    /// Compose service or instance name.
    pub(crate) target: String,
    /// Guest user (name, uid or uid:gid).
    #[arg(short, long)]
    pub(crate) user: Option<String>,
    /// Working directory.
    #[arg(short = 'w', long)]
    pub(crate) cwd: Option<String>,
    /// `KEY=VALUE` (repeatable).
    #[arg(short, long = "env")]
    pub(crate) env: Vec<String>,
    /// Run via the user's login shell.
    #[arg(short, long)]
    pub(crate) login: bool,
    /// Force a pseudo-terminal.
    #[arg(short = 't', long, conflicts_with = "no_tty")]
    pub(crate) tty: bool,
    /// Never allocate a pseudo-terminal.
    #[arg(short = 'T', long)]
    pub(crate) no_tty: bool,
    /// Do not forward stdin (the command sees EOF).
    #[arg(short = 'n', long)]
    pub(crate) no_stdin: bool,
    /// Kill the command after this long (default: no limit).
    #[arg(long, value_parser = dur)]
    pub(crate) timeout: Option<Duration>,
    /// The command and its arguments, passed as-is (no shell).
    #[arg(last = true, required = true)]
    pub(crate) argv: Vec<String>,
}

#[derive(Subcommand)]
pub(crate) enum VolumeCmd {
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
pub(crate) enum PortCmd {
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
pub(crate) enum DeviceCmd {
    /// List instance-local devices.
    Ls {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Remove instance-local devices by name.
    Rm { name: String, devices: Vec<String> },
}

pub(crate) fn create(ctx: &Ctx, a: CreateArgs) -> Result<u8> {
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

pub(crate) fn ps(ctx: &Ctx, services: Vec<String>, json: bool) -> Result<u8> {
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

pub(crate) fn exec(ctx: &Ctx, a: ExecArgs) -> Result<u8> {
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

pub(crate) fn volume(ctx: &Ctx, v: VolumeCmd) -> Result<u8> {
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

pub(crate) fn port(ctx: &Ctx, p: PortCmd) -> Result<u8> {
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

pub(crate) fn device(ctx: &Ctx, d: DeviceCmd) -> Result<u8> {
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
