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
    /// Let the sandbox reach only these hosts: `HOST[:PORT]` (port 443 by
    /// default; `*.example.com` for subdomains), repeatable; `none` denies
    /// all network. Everything else, public or private, is refused, through
    /// the proxy `isb serve` runs (docs/guides/egress.md).
    #[arg(long = "egress", value_name = "HOST[:PORT]|none")]
    pub(crate) egress: Vec<String>,
    /// A secret the sandbox sees only as a placeholder in $NAME, swapped for
    /// the real value (the org secret NAME, or `NAME=SECRET`) on the wire to
    /// these hosts and nowhere else: `NAME[=SECRET]@host1,host2`.
    #[arg(long = "secret", value_name = "NAME@HOSTS")]
    pub(crate) secrets: Vec<String>,
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
    /// Forward stdin even when it is not a terminal (a terminal's stdin, and
    /// `-T`'s, is always forwarded). Without one of those the command sees
    /// EOF, so `isb exec` inside a script never reads the script.
    #[arg(short = 'i', long, conflicts_with = "no_stdin")]
    pub(crate) interactive: bool,
    /// Never forward stdin (the command sees EOF), even from a terminal.
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
    /// Show a named volume, as JSON.
    Inspect {
        name: String,
        #[arg(long)]
        pool: Option<String>,
        /// Accepted so scripts can pass --json everywhere; the output is
        /// always JSON.
        #[arg(long)]
        json: bool,
    },
    /// Delete a named volume (refused while in use).
    #[command(alias = "remove")]
    Rm {
        name: String,
        #[arg(long)]
        pool: Option<String>,
    },
    /// Snapshots of a named volume in an org, now or on a schedule (isb serve).
    #[command(subcommand)]
    Snapshot(volumes::SnapshotCmd),
    /// A volume's snapshots, schedule, backups and staged restores (isb serve).
    Show {
        name: String,
        /// Accepted so scripts can pass --json everywhere; the output is
        /// always JSON.
        #[arg(long)]
        json: bool,
    },
    /// Restore a snapshot or a backup into a NEW volume, mounted at
    /// /restore/<stamp> in the instance using it; the live volume is untouched.
    Restore(volumes::RestoreArgs),
    /// Staged restores (of NAME, or every volume).
    Restores {
        name: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Detach and delete a staged restore.
    Discard { name: String, stamp: String },
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
        /// Print every property of the device as a JSON object instead.
        #[arg(long)]
        json: bool,
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
    spec.egress = egress_spec(&a.egress, &a.secrets)?;
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

/// `--egress` and `--secret` as the spec's `egress:` field.
fn egress_spec(allow: &[String], secrets: &[String]) -> Result<Option<isb::egress::EgressSpec>> {
    use isb::egress::{EgressSecretSpec, EgressSpec};
    if allow.is_empty() && secrets.is_empty() {
        return Ok(None);
    }
    let none = allow.iter().any(|a| a == "none");
    if none && (allow.len() > 1 || !secrets.is_empty()) {
        return Err(Error::Invalid(
            "--egress none cannot be combined with hosts or --secret".into(),
        ));
    }
    Ok(Some(EgressSpec {
        none,
        allow: if none { Vec::new() } else { allow.to_vec() },
        secrets: secrets
            .iter()
            .map(|s| EgressSecretSpec::parse(s))
            .collect::<Result<_>>()?,
    }))
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

/// Stdin is forwarded for a terminal, for `-i`, and for `-T` (the pipe form,
/// `isb exec -T NAME -- server`, whose stdin is the protocol); otherwise the
/// command sees EOF and the caller's stdin stays the caller's.
fn stdin_for(a: &ExecArgs, tty: bool) -> Stdin {
    if !a.no_stdin && (tty || a.interactive || a.no_tty) {
        Stdin::Inherit
    } else {
        Stdin::Null
    }
}

/// Piped input the command will not see is said once on stderr, instead of
/// vanishing: `-i` feeds it, `-n` says the caller meant it.
pub(crate) fn hint_unforwarded_stdin() {
    if isb::exec::stdin_has_input() {
        eprintln!(
            "isb: stdin is not forwarded without -i (pass -i to feed it, -n to drop it quietly)"
        );
    }
}

/// The compose service whose sandbox is the instance `name`, when that is
/// not the service's own key (`container_name:`, or a prefixed name).
fn service_of_instance(p: &isb::compose::Project, name: &str) -> Option<String> {
    p.file
        .services
        .iter()
        .find(|(_, spec)| spec.name.as_deref() == Some(name))
        .map(|(k, _)| k.clone())
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
        // An instance that is some service's own, by its instance name: run
        // it with that service's user and working_dir, as the service name would.
        Some(p) if service_of_instance(&p, &a.target).is_some() => {
            let svc = service_of_instance(&p, &a.target).unwrap_or_default();
            let spec = p.service(&svc)?;
            let c = ctx.client(p.file.incus_project.as_deref());
            let sb = Sandbox::get(&c, &a.target)?.with_exec_defaults(spec.exec_defaults());
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
    let stdin = stdin_for(&a, tty);
    if matches!(stdin, Stdin::Null) && !a.no_stdin {
        hint_unforwarded_stdin();
    }
    let opts = ExecOptions {
        cwd: a.cwd,
        user: a.user,
        env,
        login: a.login.then_some(true),
        tty,
        width,
        height,
        timeout: a.timeout,
        stdin,
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
    let cmd = match v {
        VolumeCmd::Snapshot(s) => volumes::Cmd::Snapshot(s),
        VolumeCmd::Show { name, .. } => volumes::Cmd::Show(name),
        VolumeCmd::Restore(a) => volumes::Cmd::Restore(a),
        VolumeCmd::Restores { name, json } => volumes::Cmd::Restores(name, json),
        VolumeCmd::Discard { name, stamp } => volumes::Cmd::Discard(name, stamp),
        other => return volume_local(ctx, other),
    };
    volumes::run(&ctx.global.org, cmd)
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
        PortCmd::Get {
            name,
            device,
            key,
            json,
        } => {
            let info = Sandbox::get(&c, &name)?.info()?;
            let dev = info
                .devices
                .get(&device)
                .filter(|p| p.get("type").map(String::as_str) == Some("proxy"))
                .ok_or_else(|| Error::NotFound(format!("proxy device {device} on {name}")))?;
            if json {
                print_json(dev);
                return Ok(0);
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn exec_args(args: &[&str]) -> ExecArgs {
        let mut v = vec!["isb", "exec"];
        v.extend_from_slice(args);
        match Cli::try_parse_from(v).unwrap().cmd {
            Cmd::Exec(a) => a,
            _ => unreachable!(),
        }
    }

    fn forwards(a: &[&str], tty: bool) -> bool {
        matches!(stdin_for(&exec_args(a), tty), Stdin::Inherit)
    }

    #[test]
    fn stdin_is_forwarded_for_a_terminal_dash_i_and_dash_t_only() {
        // A script's own stdin stays the script's.
        assert!(!forwards(&["web", "--", "ls"], false));
        // A terminal, `-i`, and the pipe form `-T` (a server speaking on stdin).
        assert!(forwards(&["web", "--", "sh"], true));
        assert!(forwards(&["-i", "web", "--", "cat"], false));
        assert!(forwards(&["-T", "web", "--", "server"], false));
        // `-n` wins, even over a terminal.
        assert!(!forwards(&["-n", "web", "--", "sh"], true));
        assert!(Cli::try_parse_from(["isb", "exec", "-i", "-n", "web", "--", "ls"]).is_err());
    }
}

#[cfg(test)]
mod instance_name_tests {
    use super::*;

    #[test]
    fn an_instance_name_finds_its_service() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("isb.yaml");
        std::fs::write(
            &f,
            "name: shop\nservices:\n  web:\n    image: images:alpine/3.20\n    user: dev\n    working_dir: /app\n  db:\n    image: images:alpine/3.20\n    container_name: shop-database\n",
        )
        .unwrap();
        let p = isb::compose::load(&isb::compose::LoadOptions {
            files: vec![f],
            ..Default::default()
        })
        .unwrap();
        assert_eq!(service_of_instance(&p, "shop-web").as_deref(), Some("web"));
        assert_eq!(
            service_of_instance(&p, "shop-database").as_deref(),
            Some("db")
        );
        assert_eq!(service_of_instance(&p, "web"), None);
        assert_eq!(service_of_instance(&p, "other"), None);
    }
}
