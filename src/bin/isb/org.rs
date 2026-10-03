//! `isb org ...`: orgs and their isolation.

use super::*;

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)] // parsed once per run
pub(crate) enum OrgCmd {
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
        /// Run the org on this server (`isb server ls`) instead of this
        /// host, through the local daemon. Set once: orgs do not move.
        #[arg(long, value_name = "SERVER")]
        server: Option<String>,
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

#[allow(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
pub(crate) fn org(ctx: &Ctx, cmd: OrgCmd) -> Result<u8> {
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
            server,
        } => {
            if let Some(server) = server.filter(|s| s != "local") {
                if !bind_root.is_empty()
                    || !allow_domain.is_empty()
                    || ingress.is_some()
                    || cloudflare_account.is_some()
                    || cloudflare_zone.is_some()
                {
                    return Err(Error::Invalid(
                        "--server: bind roots, domains and the ingress provider of an org on a server are not set from here yet".into(),
                    ));
                }
                let mut a = serde_json::json!({"org": name, "server": server});
                for (k, v) in [
                    ("cpus", cpus),
                    ("instances", instances),
                    ("default_cpus", default_cpus),
                ] {
                    if let Some(v) = v {
                        a[k] = serde_json::json!(v);
                    }
                }
                for (k, v) in [
                    ("memory", memory),
                    ("disk", disk),
                    ("default_memory", default_memory),
                ] {
                    if let Some(v) = v {
                        a[k] = serde_json::json!(v);
                    }
                }
                if !allow_egress.is_empty() {
                    let e: Vec<String> = if allow_egress == ["none"] {
                        vec![]
                    } else {
                        allow_egress
                    };
                    a["egress"] = serde_json::json!(e);
                }
                let v = call("org_create", a, Duration::from_secs(300))?;
                println!(
                    "{} on server {} (project {}, network {} {})",
                    v["name"].as_str().unwrap_or(""),
                    v["server"].as_str().unwrap_or(""),
                    v["project"].as_str().unwrap_or(""),
                    v["network"].as_str().unwrap_or(""),
                    v["subnet"].as_str().unwrap_or("")
                );
                return Ok(0);
            }
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
            let id = OrgId::new(name)?;
            let o = match org::get(&c, &id) {
                Err(e) if e.is_not_found() => {
                    // Perhaps an org on a server: the daemon knows.
                    let v =
                        call("org_get", serde_json::json!({"org": id}), SHORT).map_err(|_| e)?;
                    print_json(&v);
                    return Ok(0);
                }
                r => r?,
            };
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
                        None if o.name.is_legacy_default() => "instances only (the default org is incus' default project here)".to_string(),
                        None => "instances only (service names are off: run `sudo isb host setup`, then `isb org create` again)".to_string(),
                    }
                );
            }
            Ok(0)
        }
        OrgCmd::Rm { name, force } => {
            let id = OrgId::new(name)?;
            if let Err(e) = org::get(&c, &id) {
                if e.is_not_found() {
                    // Perhaps an org on a server: the daemon deletes it there.
                    call(
                        "org_delete",
                        serde_json::json!({"org": id, "force": force}),
                        Duration::from_secs(300),
                    )
                    .map_err(|_| e)?;
                    return Ok(0);
                }
            }
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
