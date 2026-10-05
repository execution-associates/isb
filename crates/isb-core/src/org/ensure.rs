//! [`ensure`]: create an org, or bring an existing one in line with its
//! options: its bridge, its network ACL, its restricted project and its
//! default profile.

use super::*;

/// The org's settings that `opts` may leave to what the org has now.
struct Kept {
    egress: Vec<Egress>,
    domains: String,
    ingress: String,
    cf_account: String,
    cf_zone: String,
    udp: String,
}

/// `opts` over the existing project's settings, checked.
fn kept(opts: &OrgOptions, existing: Option<&Value>) -> Result<Kept> {
    let keep = |key: &str| -> String {
        existing
            .and_then(|p| p["config"][key].as_str())
            .unwrap_or_default()
            .to_string()
    };
    let egress: Vec<Egress> = match &opts.egress {
        Some(e) => e.clone(),
        None => existing
            .and_then(|p| p["config"][KEY_EGRESS].as_str())
            .map(parse_egress_list)
            .unwrap_or_default(),
    };
    check_egress(&egress)?;
    let domains = match &opts.domains {
        Some(d) => d
            .iter()
            .map(|s| check_domain_suffix(s))
            .collect::<Result<Vec<_>>>()?
            .join(" "),
        None => keep(KEY_DOMAINS),
    };
    let ingress = match &opts.ingress {
        Some(i) if i == INGRESS_CADDY || i == INGRESS_CLOUDFLARE_TUNNEL => i.clone(),
        Some(i) => {
            return Err(Error::invalid(format!(
                "--ingress {i:?}: {INGRESS_CADDY} or {INGRESS_CLOUDFLARE_TUNNEL}"
            )));
        }
        None => keep(KEY_INGRESS),
    };
    let cf_account = opts
        .cloudflare_account
        .clone()
        .unwrap_or_else(|| keep(KEY_CF_ACCOUNT));
    let cf_zone = opts
        .cloudflare_zone
        .clone()
        .unwrap_or_else(|| keep(KEY_CF_ZONE));
    for v in [&cf_account, &cf_zone] {
        if !v.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(Error::invalid(format!(
                "Cloudflare id {v:?}: letters and digits only"
            )));
        }
    }
    let udp = match &opts.udp {
        Some(u) => {
            for a in u {
                check_udp_port(&a.to_string())?;
            }
            udp::render(u)
        }
        None => keep(KEY_UDP),
    };
    Ok(Kept {
        egress,
        domains,
        ingress,
        cf_account,
        cf_zone,
        udp,
    })
}

/// What an org's service names are waiting for.
fn no_directory(org: &OrgId) -> String {
    format!(
        "{org}: no writable {}: service names are off (run `sudo isb host setup`; a running `isb serve` then turns them on, or run this again)",
        crate::discovery::root().display()
    )
}

/// Service discovery: the org's dnsmasq reads its hosts directory. Set at
/// creation, since changing raw.dnsmasq later restarts dnsmasq. Empty when
/// the host has no directory for it.
fn raw_dnsmasq(org: &OrgId, report: &mut dyn FnMut(&str)) -> Result<String> {
    let dns_dir = crate::discovery::prepare_org(org)?;
    if dns_dir.is_none() {
        report(&no_directory(org));
    }
    Ok(dns_dir
        .as_deref()
        .map(crate::discovery::raw_dnsmasq)
        .unwrap_or_default())
}

/// The org's bridge, made if missing; its current state.
fn ensure_network(
    h: &Client,
    org: &OrgId,
    raw_dnsmasq: &str,
    report: &mut dyn FnMut(&str),
) -> Result<Value> {
    let bridge = bridge_name(org);
    let net_path = format!("/1.0/networks/{}", encode_segment(&bridge));
    if h.get_opt(&net_path)?.is_none() {
        report(&format!("{org}: creating network {bridge}"));
        let mut config = json!({
            "ipv4.address": "auto",
            "ipv4.nat": "true",
            "ipv6.address": "none",
            "dns.domain": format!("{org}.isb"),
        });
        if !raw_dnsmasq.is_empty() {
            config["raw.dnsmasq"] = json!(raw_dnsmasq);
        }
        h.mutate(
            "POST",
            "/1.0/networks",
            Some(&json!({
                "name": bridge,
                "type": "bridge",
                "description": format!("isb org {org}"),
                "config": config,
            })),
            &format!("create network {bridge}"),
            h.get_timeouts().other,
        )?;
    }
    h.get(&net_path)
}

/// Deny private ranges, except the org's own subnet (which holds its DNS)
/// and its exceptions: the ACL made or rewritten.
fn ensure_acl(
    h: &Client,
    org: &OrgId,
    net: &Value,
    egress: &[Egress],
    report: &mut dyn FnMut(&str),
) -> Result<()> {
    let subnet = net["config"]["ipv4.address"].as_str().unwrap_or_default();
    let own = subnet_of(subnet).and_then(|s| parse_cidr(&s));
    let acl = acl_name(org);
    let acl_body = json!({
        "description": format!("isb org {org}: allow within the org, deny other private networks"),
        "egress": egress_rules(own, egress)?,
        "ingress": [],
        "config": {},
    });
    let acl_path = format!("/1.0/network-acls/{}", encode_segment(&acl));
    if h.get_opt(&acl_path)?.is_none() {
        report(&format!("{org}: creating ACL {acl}"));
        let mut body = acl_body.clone();
        body["name"] = json!(acl);
        h.mutate(
            "POST",
            "/1.0/network-acls",
            Some(&body),
            &format!("create ACL {acl}"),
            h.get_timeouts().other,
        )?;
    } else {
        h.mutate(
            "PUT",
            &acl_path,
            Some(&acl_body),
            &format!("update ACL {acl}"),
            h.get_timeouts().other,
        )?;
    }
    Ok(())
}

/// The ACL attached to the bridge, and service names turned on.
fn attach(
    h: &Client,
    org: &OrgId,
    net: &Value,
    raw_dnsmasq: &str,
    report: &mut dyn FnMut(&str),
) -> Result<()> {
    let bridge = bridge_name(org);
    let acl = acl_name(org);
    let mut cfg = net["config"].clone();
    let mut changed = Vec::new();
    if net["config"]["security.acls"].as_str() != Some(acl.as_str()) {
        cfg["security.acls"] = json!(acl);
        // Traffic no rule matches passes: ingress from the host and the
        // balancer, egress to the internet.
        cfg["security.acls.default.egress.action"] = json!("allow");
        cfg["security.acls.default.ingress.action"] = json!("allow");
        changed.push("attach ACL");
    }
    // Only ever added: a host without the directory leaves an org's
    // existing setting alone.
    if !raw_dnsmasq.is_empty() && net["config"]["raw.dnsmasq"].as_str() != Some(raw_dnsmasq) {
        report(&format!(
            "{org}: turning on service names (restarts {bridge}'s DNS)"
        ));
        cfg["raw.dnsmasq"] = json!(raw_dnsmasq);
        changed.push("set raw.dnsmasq");
    }
    if !changed.is_empty() {
        h.mutate(
            "PATCH",
            &format!("/1.0/networks/{}", encode_segment(&bridge)),
            Some(&json!({"config": cfg})),
            &format!("{} on {bridge}", changed.join(", ")),
            h.get_timeouts().other,
        )?;
    }
    Ok(())
}

/// Where an org stands on service names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Names {
    /// The org's bridge already reads its hosts directory (or the org has
    /// no bridge to change).
    Present,
    /// Just turned on: the bridge's `raw.dnsmasq` now names the directory.
    TurnedOn,
    /// Off, because this host has no writable directory for it yet.
    Unavailable,
}

/// `raw.dnsmasq` with the `hostsdir=` line for `line` in it, or `None` when
/// it already names a hosts directory. Other lines the operator set stay.
fn with_hostsdir(current: &str, line: &str) -> Option<String> {
    if current.lines().any(|l| l.trim().starts_with("hostsdir=")) {
        return None;
    }
    let keep = current.trim_end();
    Some(if keep.is_empty() {
        line.to_string()
    } else {
        format!("{keep}\n{line}")
    })
}

/// Turn service names on for an existing org whose bridge does not read a
/// hosts directory yet, as `isb org create` does: the bridge's
/// `raw.dnsmasq` gets `hostsdir=<dir>`. `dns_dir` is the org's hosts
/// directory, or `None` when the host has none.
fn converge_names(
    h: &Client,
    org: &OrgId,
    dns_dir: Option<&Path>,
    report: &mut dyn FnMut(&str),
) -> Result<Names> {
    let bridge = bridge_name(org);
    let net_path = format!("/1.0/networks/{}", encode_segment(&bridge));
    let Some(net) = h.get_opt(&net_path)? else {
        return Ok(Names::Present);
    };
    let current = net["config"]["raw.dnsmasq"].as_str().unwrap_or_default();
    if current.lines().any(|l| l.trim().starts_with("hostsdir=")) {
        return Ok(Names::Present);
    }
    let Some(dir) = dns_dir else {
        return Ok(Names::Unavailable);
    };
    let Some(raw) = with_hostsdir(current, &crate::discovery::raw_dnsmasq(dir)) else {
        return Ok(Names::Present);
    };
    report(&format!(
        "{org}: turning on service names (restarts {bridge}'s DNS)"
    ));
    let mut cfg = net["config"].clone();
    cfg["raw.dnsmasq"] = json!(raw);
    h.mutate(
        "PATCH",
        &net_path,
        Some(&json!({"config": cfg})),
        &format!("set raw.dnsmasq on {bridge}"),
        h.get_timeouts().other,
    )?;
    Ok(Names::TurnedOn)
}

/// Bring one existing org's service names in line: when the host has the
/// hosts directory now (`isb host setup` ran after the org was made), make
/// the org's directory and point its bridge at it.
pub fn ensure_service_names(
    base: &Client,
    org: &OrgId,
    report: &mut dyn FnMut(&str),
) -> Result<Names> {
    ensure_service_names_in(base, org, &crate::discovery::prepare_org, report)
}

/// The directory of an org's hosts files, made if the host allows it.
pub(super) type PrepareDir<'a> = &'a dyn Fn(&OrgId) -> Result<Option<PathBuf>>;

/// [`ensure_service_names`] with the directory step given.
pub(super) fn ensure_service_names_in(
    base: &Client,
    org: &OrgId,
    prepare: PrepareDir,
    report: &mut dyn FnMut(&str),
) -> Result<Names> {
    let h = host(base);
    let dir = prepare(org)?;
    let names = converge_names(&h, org, dir.as_deref(), report)?;
    if names == Names::Unavailable {
        report(&no_directory(org));
    }
    Ok(names)
}

/// The networks the project's instances may use: the org's bridge, and the
/// egress bridges of its sandboxes (`isbbrx...`), which isb adds one by one.
fn network_access(bridge: &str, existing: Option<&Value>) -> String {
    let mut names = vec![bridge.to_string()];
    let old = existing
        .and_then(|p| p["config"]["restricted.networks.access"].as_str())
        .unwrap_or_default();
    names.extend(
        old.split(',')
            .map(str::trim)
            .filter(|n| n.starts_with(crate::egress::plumb::NET_PREFIX))
            .map(String::from),
    );
    names.join(",")
}

/// The project's config: restricted to the org's bridge and uid, its
/// limits, isb's own keys, and the disk paths it may bind (the bind roots
/// and its workspaces' host-folder homes).
fn project_config(org: &OrgId, k: &Kept, opts: &OrgOptions, existing: Option<&Value>) -> Value {
    let bridge = bridge_name(org);
    let uid = rustix::process::getuid().as_raw();
    let gid = rustix::process::getgid().as_raw();
    let mut config = json!({
        "features.images": "false",
        "features.profiles": "true",
        "features.storage.volumes": "true",
        "features.storage.buckets": "true",
        "features.networks": "false",
        "restricted": "true",
        "restricted.containers.privilege": "unprivileged",
        // Volume snapshots and exports (crate::volume_backup).
        "restricted.snapshots": "allow",
        "restricted.backups": "allow",
        "restricted.networks.access": network_access(&bridge, existing),
        // The daemon's own uid may be mapped 1:1, so `idmap: auto` keeps
        // bind-mounted files writable; root never.
        "restricted.idmap.uid": uid.to_string(),
        "restricted.idmap.gid": gid.to_string(),
        KEY_ORG: org.as_str(),
        KEY_NETWORK: bridge,
        KEY_EGRESS: k.egress.iter().map(Egress::render).collect::<Vec<_>>().join(" "),
        KEY_DOMAINS: k.domains,
        KEY_INGRESS: k.ingress,
        KEY_CF_ACCOUNT: k.cf_account,
        KEY_CF_ZONE: k.cf_zone,
        // A stack's UDP ports are NAT proxy devices: allowed in the project
        // once the org has any, and kept to them by `check_proxies`.
        "restricted.devices.proxy": if k.udp.is_empty() { "block" } else { "allow" },
        KEY_UDP: k.udp,
    });
    let roots: Vec<String> = opts
        .bind_roots
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    let homes = existing
        .map(|p| homes::recorded(&p["config"]))
        .unwrap_or_default();
    let paths = homes::disk_paths(&roots, &homes);
    if paths.is_empty() {
        config["restricted.devices.disk"] = json!("managed");
    } else {
        config["restricted.devices.disk"] = json!("allow");
        config["restricted.devices.disk.paths"] = json!(paths.join(","));
    }
    for (key, v) in [
        ("limits.cpu", opts.cpus.map(|c| c.to_string())),
        ("limits.memory", opts.memory.clone()),
        ("limits.disk", opts.disk.clone()),
        ("limits.instances", opts.instances.map(|c| c.to_string())),
    ] {
        if let Some(v) = v {
            config[key] = json!(v);
        }
    }
    // Null: removed from the project by `put_project`.
    for l in &opts.lift {
        config[l.key()] = Value::Null;
    }
    config
}

/// Whether the project has `limits.disk` once `config` is written over
/// `existing`.
fn disk_limited(config: &Value, existing: Option<&Value>) -> bool {
    match config.get("limits.disk") {
        Some(v) => v.is_string(),
        None => existing.is_some_and(|p| p["config"]["limits.disk"].is_string()),
    }
}

/// `config` written over the project's current one: a null removes the key
/// (a lifted limit), and bind paths not given are dropped.
fn merged_config(current: &Value, config: &Value) -> Value {
    let mut merged = current.clone();
    if let (Some(m), Some(c)) = (merged.as_object_mut(), config.as_object()) {
        for (k, v) in c {
            if v.is_null() {
                m.remove(k);
            } else {
                m.insert(k.clone(), v.clone());
            }
        }
        if !c.contains_key("restricted.devices.disk.paths") {
            m.remove("restricted.devices.disk.paths");
        }
    }
    merged
}

/// Create the project, or write `config` over what it has.
fn put_project(
    h: &Client,
    org: &OrgId,
    existing: Option<&Value>,
    config: &Value,
    report: &mut dyn FnMut(&str),
) -> Result<()> {
    let project = org.incus_project();
    let Some(p) = existing else {
        report(&format!("{org}: creating project {project}"));
        let config = merged_config(&json!({}), config);
        h.mutate(
            "POST",
            "/1.0/projects",
            Some(&json!({"name": project, "description": format!("isb org {org}"), "config": config})),
            &format!("create project {project}"),
            h.get_timeouts().other,
        )?;
        return Ok(());
    };
    let merged = merged_config(&p["config"], config);
    h.mutate(
        "PUT",
        &format!("/1.0/projects/{}", encode_segment(&project)),
        Some(&json!({"description": p["description"], "config": merged})),
        &format!("update project {project}"),
        h.get_timeouts().other,
    )?;
    Ok(())
}

/// The default profile: root disk (with `root_size`, while the org has a
/// disk limit), the org NIC, per-instance defaults and an isolated uid range
/// per instance.
fn default_profile(org: &OrgId, pool: &str, opts: &OrgOptions, root_size: Option<&str>) -> Value {
    let mut root = json!({"type": "disk", "path": "/", "pool": pool});
    if let Some(size) = root_size {
        root["size"] = json!(size);
    }
    json!({
        "description": format!("isb org {org}"),
        "config": {
            "limits.cpu": opts.default_cpus.unwrap_or(1).to_string(),
            "limits.memory": opts.default_memory.clone().unwrap_or_else(|| "512MiB".into()),
            "security.idmap.isolated": "true",
        },
        "devices": {
            "root": root,
            "eth0": {"type": "nic", "name": "eth0", "network": bridge_name(org)},
        },
    })
}

/// Write the default profile. Under a disk limit incus refuses an instance
/// whose root disk has no size, so the profile gives one to every instance
/// whose spec sets none (stack replicas, job runs, apps): the size the
/// profile has, else [`limits::DEFAULT_ROOT_SIZE`]. incus applies a change to
/// the instances that take their root from the profile, resizing their root
/// volumes. Without a disk limit the profile has no size.
fn set_default_profile(
    base: &Client,
    h: &Client,
    org: &OrgId,
    opts: &OrgOptions,
    disk_limited: bool,
) -> Result<()> {
    let oc = client(base, org);
    let pool = crate::sandbox::host_facts(h)?.pick_pool(None)?;
    let root_size = if disk_limited {
        let current = oc.get_opt("/1.0/profiles/default")?.unwrap_or_default();
        Some(
            current["devices"]["root"]["size"]
                .as_str()
                .unwrap_or(limits::DEFAULT_ROOT_SIZE)
                .to_string(),
        )
    } else {
        None
    };
    let profile = default_profile(org, &pool, opts, root_size.as_deref());
    oc.mutate(
        "PUT",
        "/1.0/profiles/default",
        Some(&profile),
        &format!("set {org}'s default profile"),
        oc.get_timeouts().other,
    )?;
    Ok(())
}

/// Create an org, or bring an existing one in line with `opts`. The project's
/// disk paths are `opts.bind_roots` plus the host-folder workspace homes
/// recorded on it ([`allow_home`]), so rewriting the bind roots never
/// drops a home.
pub fn ensure(
    base: &Client,
    org: &OrgId,
    opts: &OrgOptions,
    report: &mut dyn FnMut(&str),
) -> Result<OrgInfo> {
    let h = host(base);
    let project = org.incus_project();
    let existing = h.get_opt(&format!("/1.0/projects/{}", encode_segment(&project)))?;
    if let Some(p) = &existing {
        if p["config"][KEY_ORG].as_str() != Some(org.as_str()) {
            return Err(Error::AlreadyExists(format!(
                "incus project {project} exists but is not isb org {org}"
            )));
        }
    }
    for l in &opts.lift {
        let given = match l {
            Limit::Cpus => opts.cpus.is_some(),
            Limit::Memory => opts.memory.is_some(),
            Limit::Disk => opts.disk.is_some(),
            Limit::Instances => opts.instances.is_some(),
        };
        if given {
            return Err(Error::invalid(format!(
                "{}: set and lifted at once; give a value or none",
                l.key()
            )));
        }
    }
    let k = kept(opts, existing.as_ref())?;
    let raw_dnsmasq = raw_dnsmasq(org, report)?;
    let net = ensure_network(&h, org, &raw_dnsmasq, report)?;
    ensure_acl(&h, org, &net, &k.egress, report)?;
    attach(&h, org, &net, &raw_dnsmasq, report)?;
    let config = project_config(org, &k, opts, existing.as_ref());
    let disk = disk_limited(&config, existing.as_ref());
    if existing.is_some() && disk {
        // incus refuses a disk limit while an instance has no root size, so
        // the profile gives them one first.
        set_default_profile(base, &h, org, opts, disk)?;
        put_project(&h, org, existing.as_ref(), &config, report)?;
    } else {
        // A lifted disk limit goes before the profile loses its size: incus
        // refuses a sizeless root while the limit stands.
        put_project(&h, org, existing.as_ref(), &config, report)?;
        set_default_profile(base, &h, org, opts, disk)?;
    }
    get(base, org)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::fake::{Route, serve};

    fn bridge_route(prefix: &'static str, config: Value) -> Route {
        Route {
            prefix,
            status: 200,
            body: json!({"config": config}),
        }
    }

    #[test]
    fn hostsdir_is_added_beside_the_operators_own_lines() {
        let line = "hostsdir=/var/lib/isb/dns/default";
        assert_eq!(with_hostsdir("", line).as_deref(), Some(line));
        assert_eq!(
            with_hostsdir("log-queries\n", line).as_deref(),
            Some("log-queries\nhostsdir=/var/lib/isb/dns/default")
        );
        assert_eq!(with_hostsdir("hostsdir=/elsewhere", line), None);
    }

    #[test]
    fn an_org_made_before_host_setup_gets_service_names_afterwards() {
        let org = OrgId::default_org();
        let net = "GET /1.0/networks/";
        let dir = std::path::Path::new("/var/lib/isb/dns/default");
        let mut lines = Vec::new();

        // No directory on this host yet: off, and nothing changed.
        let (_d, c) = serve(vec![bridge_route(net, json!({"ipv4.address": "auto"}))]);
        let n = converge_names(&c, &org, None, &mut |l| lines.push(l.to_string())).unwrap();
        assert_eq!(n, Names::Unavailable);

        // The directory is there now: the bridge is patched.
        let (_d, c) = serve(vec![
            bridge_route(net, json!({"ipv4.address": "auto"})),
            Route {
                prefix: "PATCH /1.0/networks/",
                status: 200,
                body: json!({}),
            },
        ]);
        let n = converge_names(&c, &org, Some(dir), &mut |l| lines.push(l.to_string())).unwrap();
        assert_eq!(n, Names::TurnedOn);
        assert!(lines.iter().any(|l| l.contains("turning on service names")));

        // Without the PATCH route the same call fails: it did try to patch.
        let (_d, c) = serve(vec![bridge_route(net, json!({}))]);
        assert!(converge_names(&c, &org, Some(dir), &mut |_| {}).is_err());

        // Already on, or no bridge at all: left alone (no PATCH route to answer).
        let (_d, c) = serve(vec![bridge_route(
            net,
            json!({"raw.dnsmasq": "hostsdir=/srv/dns"}),
        )]);
        let n = converge_names(&c, &org, Some(dir), &mut |_| {}).unwrap();
        assert_eq!(n, Names::Present);
        let (_d, c) = serve(vec![]);
        let n = converge_names(&c, &org, Some(dir), &mut |_| {}).unwrap();
        assert_eq!(n, Names::Present);
    }

    fn kept_default() -> Kept {
        Kept {
            egress: Vec::new(),
            domains: String::new(),
            ingress: INGRESS_CADDY.into(),
            cf_account: String::new(),
            cf_zone: String::new(),
            udp: String::new(),
        }
    }

    #[test]
    fn rewriting_an_org_keeps_its_workspace_homes_bindable() {
        let org = OrgId::new("lab").unwrap();
        let existing = json!({"config": {
            "restricted.devices.disk": "allow",
            "restricted.devices.disk.paths": "/srv/ws/lab",
            homes::KEY_WORKSPACE_HOMES: "/srv/ws/lab",
        }});
        // `isb org create lab` again, without bind roots.
        let c = project_config(
            &org,
            &kept_default(),
            &OrgOptions::default(),
            Some(&existing),
        );
        assert_eq!(c["restricted.devices.disk"], "allow");
        assert_eq!(c["restricted.devices.disk.paths"], "/srv/ws/lab");
        // With a bind root of its own.
        let opts = OrgOptions {
            bind_roots: vec!["/data/lab".into()],
            cpus: Some(2),
            ..Default::default()
        };
        let c = project_config(&org, &kept_default(), &opts, Some(&existing));
        assert_eq!(c["restricted.devices.disk.paths"], "/data/lab,/srv/ws/lab");
        assert_eq!(c["limits.cpu"], "2");
        // An org without homes or roots binds managed volumes only.
        let c = project_config(&org, &kept_default(), &OrgOptions::default(), None);
        assert_eq!(c["restricted.devices.disk"], "managed");
        assert!(c.get("restricted.devices.disk.paths").is_none());
    }

    #[test]
    fn a_lifted_limit_is_removed_and_others_are_kept() {
        let org = OrgId::new("lab").unwrap();
        let existing = json!({"config": {
            "limits.cpu": "4", "limits.memory": "8GiB", "limits.disk": "50GiB",
        }});
        let opts = OrgOptions {
            lift: vec![Limit::Disk, Limit::Cpus],
            instances: Some(5),
            ..Default::default()
        };
        let c = project_config(&org, &kept_default(), &opts, Some(&existing));
        assert!(!disk_limited(&c, Some(&existing)));
        let m = merged_config(&existing["config"], &c);
        assert!(m.get("limits.disk").is_none(), "{m}");
        assert!(m.get("limits.cpu").is_none(), "{m}");
        assert_eq!(m["limits.memory"], "8GiB");
        assert_eq!(m["limits.instances"], "5");
        // A new project gets no null keys either.
        let m = merged_config(&json!({}), &c);
        assert!(m.as_object().unwrap().values().all(|v| !v.is_null()));

        // Kept, set, or never there.
        let none = project_config(&org, &kept_default(), &OrgOptions::default(), None);
        assert!(disk_limited(&none, Some(&existing)));
        assert!(!disk_limited(&none, None));
        let set = OrgOptions {
            disk: Some("10GiB".into()),
            ..Default::default()
        };
        let c = project_config(&org, &kept_default(), &set, None);
        assert!(disk_limited(&c, None));
    }

    #[test]
    fn the_default_profile_sizes_the_root_disk_only_under_a_disk_limit() {
        let org = OrgId::new("lab").unwrap();
        let opts = OrgOptions::default();
        let p = default_profile(&org, "default", &opts, None);
        assert!(p["devices"]["root"].get("size").is_none(), "{p}");
        assert_eq!(p["config"]["limits.cpu"], "1");
        let p = default_profile(&org, "default", &opts, Some(limits::DEFAULT_ROOT_SIZE));
        assert_eq!(p["devices"]["root"]["size"], "10GiB");
        assert_eq!(p["devices"]["root"]["pool"], "default");
    }
}
