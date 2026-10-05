//! `isb org ...`: orgs and their isolation.

use super::*;

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)] // parsed once per run
pub(crate) enum OrgCmd {
    /// Create an org, or set limits and settings on an existing one (`isb
    /// org update` also lifts them).
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
        /// A UDP port the org's stacks may publish on the host, IP:PORT
        /// with a specific host address, e.g. 203.0.113.7:10000
        /// (repeatable). Replaces the list; `none` clears it.
        #[arg(long, value_name = "IP:PORT")]
        allow_udp: Vec<String>,
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
        /// Run the org in a dedicated VM the local daemon makes on this
        /// host: its own kernel, registered as server vm-<org>. Takes a few
        /// minutes; rerun to retry.
        #[arg(long, conflicts_with = "server")]
        vm: bool,
        /// The dedicated VM's CPUs (default 2).
        #[arg(long, requires = "vm", value_name = "N")]
        vm_cpus: Option<u32>,
        /// The dedicated VM's memory (default 4GiB, at least 2GiB).
        #[arg(long, requires = "vm", value_name = "SIZE")]
        vm_memory: Option<String>,
        /// The dedicated VM's disk (default 40GiB, at least 10GiB).
        #[arg(long, requires = "vm", value_name = "SIZE")]
        vm_disk: Option<String>,
    },
    /// Change an existing org's limits, per-instance defaults, egress
    /// exceptions or UDP ports, through the daemon (the org_update tool).
    /// Flags left out keep their value; `none` lifts a limit.
    Update {
        name: String,
        /// Total CPUs across the org (the sum of every instance's
        /// limits.cpu, stopped ones included), or `none`.
        #[arg(long, value_name = "N|none")]
        cpus: Option<String>,
        /// Total memory, e.g. 16GiB, or `none`.
        #[arg(long, value_name = "SIZE|none")]
        memory: Option<String>,
        /// Total disk, e.g. 100GiB, or `none`. While set, each new instance
        /// gets a root size of its own (`raw_devices.root.size`, else 10GiB);
        /// refused while an existing instance has none.
        #[arg(long, value_name = "SIZE|none")]
        disk: Option<String>,
        /// Most instances the org may have, or `none`.
        #[arg(long, value_name = "N|none")]
        instances: Option<String>,
        /// CPUs an instance gets when its spec sets none.
        #[arg(long)]
        default_cpus: Option<u32>,
        /// Memory an instance gets when its spec sets none.
        #[arg(long)]
        default_memory: Option<String>,
        /// A private destination the org may reach, CIDR[:PORTS[/tcp|udp]]
        /// (repeatable). Replaces the org's exceptions; `none` clears them.
        #[arg(long, value_name = "DEST")]
        allow_egress: Vec<String>,
        /// A UDP port the org's stacks may publish, IP:PORT (repeatable).
        /// Replaces the list; `none` clears it.
        #[arg(long, value_name = "IP:PORT")]
        allow_udp: Vec<String>,
        #[arg(long)]
        json: bool,
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
    /// Superadmins: whether the org's workspace (and nothing else in it)
    /// may run Docker, with `security.nesting`. `on` or `off` changes it.
    Nesting {
        name: String,
        #[arg(value_parser = ["on", "off"])]
        state: Option<String>,
    },
    /// Delete an org (with --force, everything in it).
    #[command(alias = "remove")]
    Rm {
        name: String,
        #[arg(long)]
        force: bool,
        /// For an org in a dedicated VM: delete the VM (and its server
        /// registration) too.
        #[arg(long)]
        delete_vm: bool,
    },
}

#[expect(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
#[expect(
    clippy::cognitive_complexity,
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
            allow_udp,
            ingress,
            cloudflare_account,
            cloudflare_zone,
            server,
            vm,
            vm_cpus,
            vm_memory,
            vm_disk,
        } => {
            let placement = if vm {
                let mut size = serde_json::json!({});
                if let Some(c) = vm_cpus {
                    size["cpus"] = serde_json::json!(c);
                }
                if let Some(m) = vm_memory {
                    size["memory"] = serde_json::json!(m);
                }
                if let Some(d) = vm_disk {
                    size["disk"] = serde_json::json!(d);
                }
                Some(serde_json::json!({"vm": size}))
            } else {
                server
                    .filter(|s| s != "local")
                    .map(|s| serde_json::json!({"server": s}))
            };
            if let Some(placement) = placement {
                if !bind_root.is_empty()
                    || !allow_domain.is_empty()
                    || ingress.is_some()
                    || cloudflare_account.is_some()
                    || cloudflare_zone.is_some()
                {
                    return Err(Error::Invalid(
                        "--server, --vm: bind roots, domains and the ingress provider of an org on a server are not set from here yet".into(),
                    ));
                }
                let mut a = serde_json::json!({"org": name, "placement": placement});
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
                for (k, list) in [("egress", allow_egress), ("udp", allow_udp)] {
                    if !list.is_empty() {
                        let e: Vec<String> = if list == ["none"] { vec![] } else { list };
                        a[k] = serde_json::json!(e);
                    }
                }
                let v = if vm {
                    a["wait"] = serde_json::json!(false);
                    eprintln!(
                        "making a dedicated VM for {name} (vm-{name}; a few minutes the first time)"
                    );
                    call("org_create", a, SHORT)?;
                    servers::follow(&format!("vm-{name}"))?
                } else {
                    call("org_create", a, Duration::from_secs(300))?
                };
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
            let udp = match allow_udp.as_slice() {
                [] => None,
                [n] if n == "none" => Some(Vec::new()),
                l => Some(
                    l.iter()
                        .map(|u| org::check_udp_port(u))
                        .collect::<Result<Vec<_>>>()?,
                ),
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
                    lift: Vec::new(),
                    default_cpus,
                    default_memory,
                    bind_roots: roots,
                    egress,
                    domains,
                    ingress,
                    cloudflare_account,
                    cloudflare_zone,
                    udp,
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
        OrgCmd::Update {
            name,
            cpus,
            memory,
            disk,
            instances,
            default_cpus,
            default_memory,
            allow_egress,
            allow_udp,
            json,
        } => {
            let mut a = serde_json::json!({"org": OrgId::new(name)?});
            for (k, v) in [("cpus", cpus), ("instances", instances)] {
                if let Some(v) = v {
                    // A count goes as a number; `none` (or anything else, for
                    // the tool to refuse) as text.
                    a[k] = match v.trim().parse::<u64>() {
                        Ok(n) => serde_json::json!(n),
                        Err(_) => serde_json::json!(v),
                    };
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
            if let Some(n) = default_cpus {
                a["default_cpus"] = serde_json::json!(n);
            }
            for (k, list) in [("egress", allow_egress), ("udp", allow_udp)] {
                if !list.is_empty() {
                    let e: Vec<String> = if list == ["none"] { vec![] } else { list };
                    a[k] = serde_json::json!(e);
                }
            }
            let v = call("org_update", a, Duration::from_secs(300))?;
            if json {
                print_json(&v);
                return Ok(0);
            }
            for n in v["notes"].as_array().into_iter().flatten() {
                eprintln!("{}", n.as_str().unwrap_or(""));
            }
            println!("org        {}", v["name"].as_str().unwrap_or(""));
            for l in limit_lines(&v) {
                println!("{l}");
            }
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
                for l in limit_lines(&serde_json::to_value(&o).unwrap_or_default()) {
                    println!("{l}");
                }
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
                println!(
                    "udp        {}",
                    if o.udp.is_empty() {
                        "none".to_string()
                    } else {
                        o.udp.join(", ")
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
                if o.allow_nesting {
                    println!(
                        "nesting    allowed for its workspace (Docker; more of the host kernel is exposed)"
                    );
                }
                println!(
                    "names      {}",
                    match &o.dns_dir {
                        Some(d) => format!("<service>.<stack>.{}.isb (from {d})", o.name),
                        None => "instances only (service names are off: run `sudo isb host setup`, then `isb org create` again)".to_string(),
                    }
                );
            }
            Ok(0)
        }
        OrgCmd::Nesting { name, state } => {
            let mut a = serde_json::json!({"org": OrgId::new(name)?});
            if let Some(s) = state {
                a["allow_nesting"] = serde_json::json!(s == "on");
            }
            let v = call("org_nesting", a, SHORT)?;
            let on = v["allow_nesting"].as_bool() == Some(true);
            println!(
                "org {}: nesting {}",
                v["org"].as_str().unwrap_or(""),
                if on {
                    "allowed for its workspace"
                } else {
                    "blocked"
                }
            );
            for w in v["workspaces"].as_array().into_iter().flatten() {
                if w["restart_needed"].as_bool() == Some(true) {
                    println!(
                        "workspace {}: restart it for Docker to work (isb workspace restart --yes)",
                        w["name"].as_str().unwrap_or("")
                    );
                }
            }
            Ok(0)
        }
        OrgCmd::Rm {
            name,
            force,
            delete_vm,
        } => {
            let id = OrgId::new(name)?;
            if let Err(e) = org::get(&c, &id) {
                if e.is_not_found() {
                    // Perhaps an org on a server: the daemon deletes it there.
                    let v = call(
                        "org_delete",
                        serde_json::json!({"org": id, "force": force, "delete_vm": delete_vm}),
                        Duration::from_secs(300),
                    )
                    .map_err(|d| if d.is_not_found() { e } else { d })?;
                    if let Some(vm) = v["deleted_vm"].as_str() {
                        eprintln!("deleted its VM {vm}");
                    }
                    for n in v["notes"].as_array().into_iter().flatten() {
                        eprintln!("note: {}", n.as_str().unwrap_or(""));
                    }
                    return Ok(0);
                }
            }
            if delete_vm {
                return Err(Error::Invalid(format!(
                    "--delete-vm: org {id} runs on this host, not in a dedicated VM"
                )));
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

/// An org's limits as `org show` prints them, from its JSON (`org_get` or
/// [`isb::org::OrgInfo`]): each limit with what its instances are allocated
/// against it, and the per-instance defaults.
fn limit_lines(v: &serde_json::Value) -> Vec<String> {
    let budget = |name: &str| {
        let b = &v["allocation"][name];
        Some(isb::org::Budget {
            limit: b["limit"].as_i64()?,
            allocated: b["allocated"].as_i64()?,
            free: b["free"].as_i64()?,
        })
    };
    let mut out = vec![format!(
        "instances  {}{}",
        v["instances"].as_u64().unwrap_or(0),
        v["instances_limit"]
            .as_str()
            .map(|l| format!(" of {l}, stopped ones included"))
            .unwrap_or_default()
    )];
    let mut any = false;
    for (label, key, name, in_bytes) in [
        ("cpus      ", "cpus", "cpu", false),
        ("memory    ", "memory", "memory", true),
        ("disk      ", "disk", "disk", true),
    ] {
        let line = match (budget(name), v[key].as_str()) {
            (Some(b), _) => {
                any = true;
                b.text(in_bytes)
            }
            (None, Some(l)) => l.to_string(),
            (None, None) => "unlimited".to_string(),
        };
        out.push(format!("{label} {line}"));
    }
    if any {
        out.push(
            "           (allocated: the sum of every instance's limit, stopped ones included)"
                .into(),
        );
    }
    let mut defaults = vec![
        format!("{} CPU", v["default_cpus"].as_str().unwrap_or("1")),
        format!(
            "{} memory",
            v["default_memory"].as_str().unwrap_or("512MiB")
        ),
    ];
    if let Some(d) = v["default_disk"].as_str() {
        defaults.push(format!("{d} root disk"));
    }
    out.push(format!(
        "defaults   {} per instance whose spec sets none",
        defaults.join(", ")
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::limit_lines;
    use serde_json::json;

    #[test]
    fn limits_show_their_allocation() {
        let v = json!({
            "instances": 3, "instances_limit": "5", "cpus": "4", "memory": "4GiB",
            "default_cpus": "1", "default_memory": "512MiB", "default_disk": "10GiB",
            "allocation": {
                "cpu": {"limit": 4, "allocated": 3, "free": 1},
                "memory": {"limit": 4i64 << 30, "allocated": 1536i64 << 20, "free": 2560i64 << 20},
            },
        });
        let l = limit_lines(&v);
        assert_eq!(l[0], "instances  3 of 5, stopped ones included");
        assert_eq!(l[1], "cpus       3 of 4 allocated, 1 free");
        assert_eq!(l[2], "memory     1.5GiB of 4GiB allocated, 2.5GiB free");
        assert_eq!(l[3], "disk       unlimited");
        assert!(l[4].contains("stopped ones included"));
        assert_eq!(
            l[5],
            "defaults   1 CPU, 512MiB memory, 10GiB root disk per instance whose spec sets none"
        );
        // An org on a server whose daemon reports no allocation: the limit.
        let l = limit_lines(&json!({"instances": 0, "cpus": "2"}));
        assert_eq!(l[1], "cpus       2");
        assert!(l[4].starts_with("defaults"));
    }
}
