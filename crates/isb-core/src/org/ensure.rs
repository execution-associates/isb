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
    Ok(Kept {
        egress,
        domains,
        ingress,
        cf_account,
        cf_zone,
    })
}

/// Service discovery: the org's dnsmasq reads its hosts directory. Set at
/// creation, since changing raw.dnsmasq later restarts dnsmasq. Empty when
/// the host has no directory for it.
fn raw_dnsmasq(org: &OrgId, report: &mut dyn FnMut(&str)) -> Result<String> {
    let dns_dir = crate::discovery::prepare_org(org)?;
    if dns_dir.is_none() {
        report(&format!(
            "{org}: no writable {}: service names are off (run `sudo isb host setup`, then this again)",
            crate::discovery::root().display()
        ));
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
        "restricted.networks.access": bridge,
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
    config
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
        h.mutate(
            "POST",
            "/1.0/projects",
            Some(&json!({"name": project, "description": format!("isb org {org}"), "config": config})),
            &format!("create project {project}"),
            h.get_timeouts().other,
        )?;
        return Ok(());
    };
    let mut merged = p["config"].clone();
    if let (Some(m), Some(c)) = (merged.as_object_mut(), config.as_object()) {
        for (k, v) in c {
            m.insert(k.clone(), v.clone());
        }
        if !c.contains_key("restricted.devices.disk.paths") {
            m.remove("restricted.devices.disk.paths");
        }
    }
    h.mutate(
        "PUT",
        &format!("/1.0/projects/{}", encode_segment(&project)),
        Some(&json!({"description": p["description"], "config": merged})),
        &format!("update project {project}"),
        h.get_timeouts().other,
    )?;
    Ok(())
}

/// The default profile: root disk, the org NIC, per-instance defaults and
/// an isolated uid range per instance.
fn set_default_profile(base: &Client, h: &Client, org: &OrgId, opts: &OrgOptions) -> Result<()> {
    let oc = client(base, org);
    let pool = crate::sandbox::host_facts(h)?.pick_pool(None)?;
    let profile = json!({
        "description": format!("isb org {org}"),
        "config": {
            "limits.cpu": opts.default_cpus.unwrap_or(1).to_string(),
            "limits.memory": opts.default_memory.clone().unwrap_or_else(|| "512MiB".into()),
            "security.idmap.isolated": "true",
        },
        "devices": {
            "root": {"type": "disk", "path": "/", "pool": pool},
            "eth0": {"type": "nic", "name": "eth0", "network": bridge_name(org)},
        },
    });
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
    let k = kept(opts, existing.as_ref())?;
    let raw_dnsmasq = raw_dnsmasq(org, report)?;
    let net = ensure_network(&h, org, &raw_dnsmasq, report)?;
    ensure_acl(&h, org, &net, &k.egress, report)?;
    attach(&h, org, &net, &raw_dnsmasq, report)?;
    let config = project_config(org, &k, opts, existing.as_ref());
    put_project(&h, org, existing.as_ref(), &config, report)?;
    set_default_profile(base, &h, org, opts)?;
    get(base, org)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept_default() -> Kept {
        Kept {
            egress: Vec::new(),
            domains: String::new(),
            ingress: INGRESS_CADDY.into(),
            cf_account: String::new(),
            cf_zone: String::new(),
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
}
