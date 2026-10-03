//! Orgs: the trust boundary. An org is an incus project; its people and
//! agents fully administer what is in it, and nothing crosses orgs.
//!
//! The `default` org is the incus `default` project, so everything that
//! predates orgs keeps working where it is. Any other org `x` is the incus
//! project `isb-x`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// A validated org name: `[a-z][a-z0-9-]{0,30}`, not ending in `-`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct OrgId(String);

pub const DEFAULT_ORG: &str = "default";

impl OrgId {
    pub fn new(s: impl Into<String>) -> Result<OrgId> {
        let s = s.into();
        let ok = !s.is_empty()
            && s.len() <= 31
            && s.starts_with(|c: char| c.is_ascii_lowercase())
            && !s.ends_with('-')
            && s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if ok {
            Ok(OrgId(s))
        } else {
            Err(Error::invalid(format!(
                "org name {s:?}: up to 31 characters of [a-z0-9-], starting with a letter"
            )))
        }
    }

    pub fn default_org() -> OrgId {
        OrgId(DEFAULT_ORG.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_default(&self) -> bool {
        self.0 == DEFAULT_ORG
    }

    /// The incus project holding this org.
    pub fn incus_project(&self) -> String {
        if self.is_default() {
            "default".into()
        } else {
            format!("isb-{}", self.0)
        }
    }

    /// The org a project belongs to, if it is one of isb's.
    pub fn from_incus_project(project: &str) -> Option<OrgId> {
        if project == "default" {
            return Some(OrgId::default_org());
        }
        project.strip_prefix("isb-").and_then(|o| OrgId::new(o).ok())
    }

    /// This org's directory under a daemon state directory.
    pub fn dir(&self, state: &Path) -> PathBuf {
        state.join("orgs").join(&self.0)
    }
}

impl std::fmt::Display for OrgId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for OrgId {
    type Error = Error;
    fn try_from(s: String) -> Result<OrgId> {
        OrgId::new(s)
    }
}

impl From<OrgId> for String {
    fn from(o: OrgId) -> String {
        o.0
    }
}


// ----------------------------------------------------------------------------
// The org runtime: an incus project, a bridge, an ACL and a default profile.
// ----------------------------------------------------------------------------

use crate::client::{Client, encode_segment};
use serde_json::{Value, json};

/// Config keys isb keeps on the org's project.
const KEY_ORG: &str = "user.isb.org";
const KEY_NETWORK: &str = "user.isb.network";

/// Limits for an org as a whole (the incus project's limits) and the
/// defaults each instance gets when its spec sets none.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OrgOptions {
    /// Total CPUs across the org's instances.
    pub cpus: Option<u32>,
    /// Total memory, e.g. `16GiB`.
    pub memory: Option<String>,
    /// Total disk, e.g. `100GiB`.
    pub disk: Option<String>,
    pub instances: Option<u32>,
    /// Per-instance defaults (incus requires one once the project is limited).
    pub default_cpus: Option<u32>,
    pub default_memory: Option<String>,
    /// Host directories the org's instances may bind-mount from.
    pub bind_roots: Vec<PathBuf>,
}

/// An org as it exists in incus.
#[derive(Debug, Clone, Serialize)]
pub struct OrgInfo {
    pub name: OrgId,
    pub project: String,
    /// The org's bridge, `None` for the default org.
    pub network: Option<String>,
    /// The bridge's IPv4 address, e.g. `10.64.3.1/24`.
    pub subnet: Option<String>,
    pub cpus: Option<String>,
    pub memory: Option<String>,
    pub disk: Option<String>,
    pub instances_limit: Option<String>,
    pub bind_roots: Vec<String>,
    /// Instances in the org right now.
    pub instances: usize,
}

/// The bridge for an org: `isbbr` + 8 hex digits of the name's hash, inside
/// the kernel's 15-character limit on interface names.
pub fn bridge_name(org: &OrgId) -> String {
    let mut h: u32 = 0x811c9dc5;
    for b in org.as_str().bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x01000193);
    }
    format!("isbbr{h:08x}")
}

fn acl_name(org: &OrgId) -> String {
    format!("isb-{org}")
}

/// Private ranges an org may not reach, apart from its own subnet.
const PRIVATE: &str = "10.0.0.0/8,172.16.0.0/12,192.168.0.0/16,100.64.0.0/10,169.254.0.0/16";

/// The client to use for an org: its project.
pub fn client(base: &Client, org: &OrgId) -> Client {
    base.clone().project(org.incus_project())
}

/// A client on the default project, for host-wide objects (networks, ACLs,
/// projects).
fn host(base: &Client) -> Client {
    base.clone().project("default")
}

fn strmap(v: &Value) -> std::collections::BTreeMap<String, String> {
    v.as_object()
        .map(|m| {
            m.iter()
                .map(|(k, v)| (k.clone(), v.as_str().map(String::from).unwrap_or_else(|| v.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// Create an org, or bring an existing one in line with `opts`. The default
/// org is the incus default project and is never restricted.
pub fn ensure(base: &Client, org: &OrgId, opts: &OrgOptions, report: &mut dyn FnMut(&str)) -> Result<OrgInfo> {
    if org.is_default() {
        return Err(Error::invalid("the default org is incus' default project; it has no settings"));
    }
    let h = host(base);
    let bridge = bridge_name(org);
    let net_path = format!("/1.0/networks/{}", encode_segment(&bridge));
    if h.get_opt(&net_path)?.is_none() {
        report(&format!("{org}: creating network {bridge}"));
        h.mutate(
            "POST",
            "/1.0/networks",
            Some(&json!({
                "name": bridge,
                "type": "bridge",
                "description": format!("isb org {org}"),
                "config": {
                    "ipv4.address": "auto",
                    "ipv4.nat": "true",
                    "ipv6.address": "none",
                    "dns.domain": format!("{org}.isb"),
                },
            })),
            &format!("create network {bridge}"),
            h.get_timeouts().other,
        )?;
    }
    let net = h.get(&net_path)?;
    let subnet = net["config"]["ipv4.address"].as_str().unwrap_or_default().to_string();
    let own = subnet_of(&subnet).unwrap_or_default();

    // Deny private ranges, except the org's own subnet (which holds its DNS).
    let acl = acl_name(org);
    let mut egress = Vec::new();
    if !own.is_empty() {
        egress.push(json!({"action": "allow", "destination": own, "state": "enabled", "description": "own subnet"}));
    }
    egress.push(json!({"action": "reject", "destination": PRIVATE, "state": "enabled", "description": "other orgs and private networks"}));
    let acl_body = json!({
        "description": format!("isb org {org}: allow within the org, deny other private networks"),
        "egress": egress,
        "ingress": [],
        "config": {},
    });
    let acl_path = format!("/1.0/network-acls/{}", encode_segment(&acl));
    if h.get_opt(&acl_path)?.is_none() {
        report(&format!("{org}: creating ACL {acl}"));
        let mut body = acl_body.clone();
        body["name"] = json!(acl);
        h.mutate("POST", "/1.0/network-acls", Some(&body), &format!("create ACL {acl}"), h.get_timeouts().other)?;
    } else {
        h.mutate("PUT", &acl_path, Some(&acl_body), &format!("update ACL {acl}"), h.get_timeouts().other)?;
    }
    if net["config"]["security.acls"].as_str() != Some(acl.as_str()) {
        let mut cfg = net["config"].clone();
        cfg["security.acls"] = json!(acl);
        // Traffic no rule matches passes: ingress from the host and the
        // balancer, egress to the internet.
        cfg["security.acls.default.egress.action"] = json!("allow");
        cfg["security.acls.default.ingress.action"] = json!("allow");
        h.mutate(
            "PATCH",
            &net_path,
            Some(&json!({"config": cfg})),
            &format!("attach ACL to {bridge}"),
            h.get_timeouts().other,
        )?;
    }

    let project = org.incus_project();
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
        "restricted.networks.access": bridge,
        // The daemon's own uid may be mapped 1:1, so `idmap: auto` keeps
        // bind-mounted files writable; root never.
        "restricted.idmap.uid": uid.to_string(),
        "restricted.idmap.gid": gid.to_string(),
        KEY_ORG: org.as_str(),
        KEY_NETWORK: bridge,
    });
    let roots: Vec<String> = opts.bind_roots.iter().map(|p| p.display().to_string()).collect();
    if roots.is_empty() {
        config["restricted.devices.disk"] = json!("managed");
    } else {
        config["restricted.devices.disk"] = json!("allow");
        config["restricted.devices.disk.paths"] = json!(roots.join(","));
    }
    for (k, v) in [
        ("limits.cpu", opts.cpus.map(|c| c.to_string())),
        ("limits.memory", opts.memory.clone()),
        ("limits.disk", opts.disk.clone()),
        ("limits.instances", opts.instances.map(|c| c.to_string())),
    ] {
        if let Some(v) = v {
            config[k] = json!(v);
        }
    }
    let proj_path = format!("/1.0/projects/{}", encode_segment(&project));
    match h.get_opt(&proj_path)? {
        None => {
            report(&format!("{org}: creating project {project}"));
            h.mutate(
                "POST",
                "/1.0/projects",
                Some(&json!({"name": project, "description": format!("isb org {org}"), "config": config})),
                &format!("create project {project}"),
                h.get_timeouts().other,
            )?;
        }
        Some(p) => {
            if p["config"][KEY_ORG].as_str() != Some(org.as_str()) {
                return Err(Error::AlreadyExists(format!(
                    "incus project {project} exists but is not isb org {org}"
                )));
            }
            let mut merged = p["config"].clone();
            for (k, v) in config.as_object().unwrap() {
                merged[k] = v.clone();
            }
            if roots.is_empty() {
                if let Some(m) = merged.as_object_mut() {
                    m.remove("restricted.devices.disk.paths");
                }
            }
            h.mutate(
                "PUT",
                &proj_path,
                Some(&json!({"description": p["description"], "config": merged})),
                &format!("update project {project}"),
                h.get_timeouts().other,
            )?;
        }
    }

    // The default profile: root disk, the org NIC, per-instance defaults and
    // an isolated uid range per instance.
    let oc = client(base, org);
    let facts = crate::sandbox::host_facts(&h)?;
    let pool = facts.pick_pool(None)?;
    let profile = json!({
        "description": format!("isb org {org}"),
        "config": {
            "limits.cpu": opts.default_cpus.unwrap_or(1).to_string(),
            "limits.memory": opts.default_memory.clone().unwrap_or_else(|| "512MiB".into()),
            "security.idmap.isolated": "true",
        },
        "devices": {
            "root": {"type": "disk", "path": "/", "pool": pool},
            "eth0": {"type": "nic", "name": "eth0", "network": bridge},
        },
    });
    oc.mutate("PUT", "/1.0/profiles/default", Some(&profile), &format!("set {org}'s default profile"), oc.get_timeouts().other)?;
    get(base, org)
}

/// The network part of a CIDR address: `10.64.3.1/24` -> `10.64.3.0/24`.
fn subnet_of(cidr: &str) -> Option<String> {
    let (ip, len) = cidr.split_once('/')?;
    let ip: std::net::Ipv4Addr = ip.parse().ok()?;
    let len: u32 = len.parse().ok()?;
    if len > 32 {
        return None;
    }
    let mask = if len == 0 { 0 } else { u32::MAX << (32 - len) };
    Some(format!("{}/{len}", std::net::Ipv4Addr::from(u32::from(ip) & mask)))
}

fn info(base: &Client, org: OrgId, p: &Value) -> Result<OrgInfo> {
    let cfg = strmap(&p["config"]);
    let network = cfg.get(KEY_NETWORK).cloned();
    let subnet = match &network {
        Some(n) => host(base)
            .get_opt(&format!("/1.0/networks/{}", encode_segment(n)))?
            .and_then(|v| v["config"]["ipv4.address"].as_str().map(String::from)),
        None => None,
    };
    let instances = client(base, &org)
        .get("/1.0/instances")?
        .as_array()
        .map(|a| a.len())
        .unwrap_or(0);
    Ok(OrgInfo {
        project: org.incus_project(),
        name: org,
        network,
        subnet,
        cpus: cfg.get("limits.cpu").cloned(),
        memory: cfg.get("limits.memory").cloned(),
        disk: cfg.get("limits.disk").cloned(),
        instances_limit: cfg.get("limits.instances").cloned(),
        bind_roots: cfg
            .get("restricted.devices.disk.paths")
            .map(|s| s.split(',').filter(|x| !x.is_empty()).map(String::from).collect())
            .unwrap_or_default(),
        instances,
    })
}

/// One org.
pub fn get(base: &Client, org: &OrgId) -> Result<OrgInfo> {
    let h = host(base);
    let p = h
        .get_opt(&format!("/1.0/projects/{}", encode_segment(&org.incus_project())))?
        .ok_or_else(|| Error::NotFound(format!("org {org}")))?;
    if !org.is_default() && p["config"][KEY_ORG].as_str() != Some(org.as_str()) {
        return Err(Error::NotFound(format!("org {org}")));
    }
    info(base, org.clone(), &p)
}

/// Every org: the default one first, then isb's projects by name.
pub fn list(base: &Client) -> Result<Vec<OrgInfo>> {
    let h = host(base);
    let v = h.get("/1.0/projects?recursion=1")?;
    let mut out = Vec::new();
    for p in v.as_array().into_iter().flatten() {
        let name = p["name"].as_str().unwrap_or_default();
        let Some(org) = OrgId::from_incus_project(name) else { continue };
        if !org.is_default() && p["config"][KEY_ORG].as_str() != Some(org.as_str()) {
            continue;
        }
        out.push(info(base, org, p)?);
    }
    out.sort_by(|a, b| (!a.name.is_default(), &a.name).cmp(&(!b.name.is_default(), &b.name)));
    Ok(out)
}

/// Delete an org: its project (with `force`, everything in it), its network
/// and its ACL. Refuses a non-empty org without `force`.
pub fn remove(base: &Client, org: &OrgId, force: bool, report: &mut dyn FnMut(&str)) -> Result<()> {
    if org.is_default() {
        return Err(Error::invalid("the default org cannot be removed"));
    }
    let o = get(base, org)?;
    if o.instances > 0 && !force {
        return Err(Error::invalid(format!(
            "org {org} has {} instance(s); remove them, or pass force",
            o.instances
        )));
    }
    let h = host(base);
    let oc = client(base, org);
    for name in oc.get("/1.0/instances")?.as_array().into_iter().flatten().filter_map(Value::as_str) {
        let n = name.rsplit('/').next().unwrap_or(name);
        report(&format!("{org}: deleting {n}"));
        crate::sandbox::Sandbox::remove(&oc, n, true)?;
    }
    report(&format!("{org}: deleting project {}", o.project));
    // force also takes the org's volumes, profiles and buckets with it.
    h.mutate(
        "DELETE",
        &format!("/1.0/projects/{}?force=true", encode_segment(&o.project)),
        None,
        &format!("delete project {}", o.project),
        h.get_timeouts().other,
    )?;
    if let Some(n) = &o.network {
        report(&format!("{org}: deleting network {n}"));
        match h.mutate("DELETE", &format!("/1.0/networks/{}", encode_segment(n)), None, &format!("delete network {n}"), h.get_timeouts().other) {
            Err(e) if !e.is_not_found() => return Err(e),
            _ => {}
        }
    }
    let acl = acl_name(org);
    match h.mutate("DELETE", &format!("/1.0/network-acls/{}", encode_segment(&acl)), None, &format!("delete ACL {acl}"), h.get_timeouts().other) {
        Err(e) if !e.is_not_found() => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_projects() {
        assert!(OrgId::new("ocai").is_ok());
        assert!(OrgId::new("Ocai").is_err());
        assert!(OrgId::new("a-").is_err());
        assert!(OrgId::new("x".repeat(32)).is_err());
        let o = OrgId::new("ocai").unwrap();
        assert_eq!(o.incus_project(), "isb-ocai");
        assert_eq!(OrgId::default_org().incus_project(), "default");
        assert_eq!(OrgId::from_incus_project("isb-ocai"), Some(o));
        assert_eq!(OrgId::from_incus_project("titan-ocai-ct"), None);
        let j: OrgId = serde_json::from_str("\"norm\"").unwrap();
        assert_eq!(j.as_str(), "norm");
        assert!(serde_json::from_str::<OrgId>("\"Bad Name\"").is_err());
    }

    #[test]
    fn bridges_and_subnets() {
        let b = bridge_name(&OrgId::new("a-very-long-org-name-indeed").unwrap());
        assert!(b.len() <= 15 && b.starts_with("isbbr"), "{b}");
        assert_ne!(b, bridge_name(&OrgId::new("other").unwrap()));
        assert_eq!(subnet_of("10.64.3.1/24").as_deref(), Some("10.64.3.0/24"));
        assert_eq!(subnet_of("10.180.0.1/16").as_deref(), Some("10.180.0.0/16"));
        assert_eq!(subnet_of("nope"), None);
    }
}
