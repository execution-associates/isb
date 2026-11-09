//! Instance addresses on isb's org bridges: pinned, never only leased.
//!
//! incus' bridges hand out addresses by DHCP with a one-hour lease, and an
//! OCI container never renews its lease. An hour after one starts, dnsmasq
//! considers its address free and gives it to the next instance that asks:
//! on 2026-11-09 a new instance in org exa got the address a four-week-old
//! Postgres held, and the apps using it broke until the newcomer moved. A
//! NIC's `ipv4.address` makes dnsmasq keep the address for that NIC alone,
//! whatever happens to the lease, so isb pins every address on an org
//! bridge:
//!
//! - At create ([`pin_for_create`]), on an address no instance on the
//!   bridge holds or is pinned to, checked again once the instance exists
//!   ([`Pin::verify`]): incus accepts two NICs pinned to one address, then
//!   refuses to start either.
//! - The daemon's check ([`watch`]), every minute, pins an instance that
//!   has no pin at the address it has (in place: it keeps running, and
//!   keeps its address), gives a stopped one a free address, and records
//!   each pin in `user.isb.ipv4.<nic>`, which adopts pins set by hand. It
//!   raises `net.failed` when two instances share an address, when a
//!   pinned instance has another one, or when an instance cannot be pinned.
//!
//! Only isb's org projects (`isb-<org>`) and their managed bridges are
//! touched. Egress bridges (`isbbrx*`) hold one sandbox each, so nothing on
//! them can collide; they are left to DHCP.

use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde_json::{Value, json};

use crate::client::{Client, encode_segment};
use crate::error::{Error, Result};
use crate::org::OrgId;
use crate::plan::Props;

/// The config key recording the address isb pinned a NIC to.
pub fn pin_key(device: &str) -> String {
    format!("user.isb.ipv4.{device}")
}

/// How often the daemon checks every org's addresses.
pub const CHECK_EVERY: Duration = Duration::from_secs(60);

/// A managed bridge's IPv4 subnet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Subnet {
    gateway: Ipv4Addr,
    prefix: u8,
}

impl Subnet {
    /// From `GET /1.0/networks/<name>`: a managed bridge with an IPv4
    /// address (`10.1.2.1/24`), not an egress bridge.
    pub fn from_network(net: &Value) -> Option<Subnet> {
        let name = net["name"].as_str().unwrap_or_default();
        if net["type"].as_str() != Some("bridge")
            || net["managed"] == Value::Bool(false)
            || name.starts_with(crate::egress::plumb::NET_PREFIX)
        {
            return None;
        }
        let (ip, prefix) = net["config"]["ipv4.address"].as_str()?.split_once('/')?;
        let prefix: u8 = prefix.trim().parse().ok()?;
        (8..=30).contains(&prefix).then_some(())?;
        Some(Subnet {
            gateway: ip.trim().parse().ok()?,
            prefix,
        })
    }

    fn mask(&self) -> u32 {
        u32::MAX << (32 - self.prefix)
    }

    pub fn contains(&self, ip: Ipv4Addr) -> bool {
        u32::from(ip) & self.mask() == u32::from(self.gateway) & self.mask()
    }

    /// An address an instance may take: in the subnet, and neither its
    /// network, broadcast nor gateway address.
    fn assignable(&self, ip: Ipv4Addr) -> bool {
        let host = u32::from(ip) & !self.mask();
        self.contains(ip) && ip != self.gateway && host != 0 && host != !self.mask()
    }

    /// A free address, searched from a point `seed` picks, so two callers
    /// with different seeds rarely race for one.
    pub fn pick_free(&self, used: &BTreeSet<Ipv4Addr>, seed: &str) -> Option<Ipv4Addr> {
        let base = u32::from(self.gateway) & self.mask();
        let size = !self.mask() as u64 + 1;
        let start = fnv(seed) % size;
        (0..size)
            .map(|i| Ipv4Addr::from(base + ((start + i) % size) as u32))
            .find(|ip| self.assignable(*ip) && !used.contains(ip))
    }
}

fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf29ce484222325, |h, b| {
        (h ^ b as u64).wrapping_mul(0x100000001b3)
    })
}

/// One instance's NIC on a bridge, from an instance listed with
/// `recursion=2`.
#[derive(Debug, Clone)]
pub struct Claim {
    pub project: String,
    pub instance: String,
    pub device: String,
    /// The NIC's `ipv4.address`.
    pub pin: Option<Ipv4Addr>,
    /// What [`pin_key`] records.
    pub recorded: Option<String>,
    /// Its IPv4 addresses on the bridge's subnet (running instances only).
    pub addrs: Vec<Ipv4Addr>,
    /// Running, but incus has lost the NIC's host side name
    /// (`volatile.<nic>.host_name`): incus cannot change the NIC until the
    /// instance restarts, and an org project may not set the name back.
    pub veth_lost: bool,
    pub running: bool,
    /// The NIC as the instance has it (expanded: from a profile or its own).
    pub props: Props,
    pub stack: Option<String>,
    pub service: Option<String>,
    pub created_at: String,
}

impl Claim {
    fn id(&self) -> (String, String) {
        (self.project.clone(), self.instance.clone())
    }
}

/// Every NIC on `network` among `instances` (in isb's org projects).
pub fn claims(instances: &[Value], network: &str, subnet: &Subnet) -> Vec<Claim> {
    let mut out = Vec::new();
    for i in instances {
        let project = i["project"].as_str().unwrap_or("default");
        if OrgId::from_incus_project(project).is_none() {
            continue;
        }
        let Some(devs) = i["expanded_devices"].as_object() else {
            continue;
        };
        for (dev, d) in devs {
            if d["type"] != "nic" || d["network"].as_str() != Some(network) {
                continue;
            }
            out.push(claim(i, project, dev, d, subnet));
        }
    }
    out
}

fn claim(i: &Value, project: &str, dev: &str, d: &Value, subnet: &Subnet) -> Claim {
    let cfg = &i["config"];
    let label = |k: &str| cfg[format!("user.{k}")].as_str().map(String::from);
    let running = i["status"].as_str() == Some("Running");
    Claim {
        project: project.to_string(),
        instance: i["name"].as_str().unwrap_or_default().to_string(),
        device: dev.to_string(),
        pin: d["ipv4.address"].as_str().and_then(|a| a.parse().ok()),
        recorded: cfg[pin_key(dev)].as_str().map(String::from),
        addrs: if running {
            nic_addrs(i, dev, d, subnet)
        } else {
            Vec::new()
        },
        running,
        veth_lost: running && cfg[format!("volatile.{dev}.host_name")].is_null(),
        props: d
            .as_object()
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default(),
        stack: label("isb.stack"),
        service: label("isb.service"),
        created_at: i["created_at"].as_str().unwrap_or_default().to_string(),
    }
}

/// The NIC's addresses on `subnet`, from the instance's state: the guest
/// interface with the NIC's MAC (a VM names it enp5s0, say), else the one
/// named as the NIC says.
fn nic_addrs(i: &Value, dev: &str, d: &Value, subnet: &Subnet) -> Vec<Ipv4Addr> {
    let Some(ifaces) = i["state"]["network"].as_object() else {
        return Vec::new();
    };
    let mac = i["config"][format!("volatile.{dev}.hwaddr")]
        .as_str()
        .map(str::to_ascii_lowercase);
    let by_mac = mac.and_then(|m| {
        ifaces
            .values()
            .find(|v| v["hwaddr"].as_str().map(str::to_ascii_lowercase) == Some(m.clone()))
    });
    let iface = by_mac.or_else(|| ifaces.get(d["name"].as_str().unwrap_or(dev)));
    iface
        .and_then(|v| v["addresses"].as_array())
        .into_iter()
        .flatten()
        .filter(|a| a["family"] == "inet")
        .filter_map(|a| a["address"].as_str()?.parse().ok())
        .filter(|ip| subnet.contains(*ip))
        .collect()
}

/// Who holds each address: its pin, and what a running instance has.
fn holders(claims: &[Claim]) -> BTreeMap<Ipv4Addr, BTreeSet<(String, String)>> {
    let mut out: BTreeMap<Ipv4Addr, BTreeSet<(String, String)>> = BTreeMap::new();
    for c in claims {
        for ip in c.pin.iter().chain(&c.addrs) {
            out.entry(*ip).or_default().insert(c.id());
        }
    }
    out
}

/// What the check changes.
#[derive(Debug, Clone, PartialEq)]
pub enum Fix {
    /// Record a pin isb did not set (one set by hand) as isb's.
    Adopt {
        project: String,
        instance: String,
        device: String,
        ip: Ipv4Addr,
    },
    /// Pin a NIC: at the address it has when running, else a free one.
    Pin {
        project: String,
        instance: String,
        device: String,
        props: Props,
        ip: Ipv4Addr,
        running: bool,
    },
}

/// What the check reports (as `net.failed`).
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    /// The same problem has the same key on every pass.
    pub key: String,
    pub project: String,
    pub instance: String,
    pub stack: Option<String>,
    pub service: Option<String>,
    pub message: String,
}

impl Finding {
    fn about(c: &Claim, key: String, message: String) -> Finding {
        Finding {
            key,
            project: c.project.clone(),
            instance: c.instance.clone(),
            stack: c.stack.clone(),
            service: c.service.clone(),
            message,
        }
    }
}

/// Decide the fixes and findings for one bridge's claims. `reserved`:
/// addresses being given to instances that do not exist yet.
pub fn plan(
    network: &str,
    subnet: &Subnet,
    claims: &[Claim],
    reserved: &BTreeSet<Ipv4Addr>,
) -> (Vec<Fix>, Vec<Finding>) {
    let held = holders(claims);
    let mut findings = conflicts(network, claims, &held);
    let mut used: BTreeSet<Ipv4Addr> = held
        .keys()
        .copied()
        .chain(reserved.iter().copied())
        .collect();
    let mut fixes = Vec::new();
    for c in claims {
        if let Some(pin) = c.pin {
            // A shared pin is reported above, never made isb's.
            let alone = held.get(&pin).is_some_and(|h| h.len() == 1);
            if alone && c.recorded.as_deref() != Some(pin.to_string().as_str()) {
                fixes.push(Fix::Adopt {
                    project: c.project.clone(),
                    instance: c.instance.clone(),
                    device: c.device.clone(),
                    ip: pin,
                });
            }
            if c.running && !c.addrs.is_empty() && !c.addrs.contains(&pin) {
                let have = join(&c.addrs);
                findings.push(Finding::about(
                    c,
                    format!(
                        "drift {} {} {} {pin} {have}",
                        c.project, c.instance, c.device
                    ),
                    format!(
                        "{}'s {} is pinned to {pin} but has {have}: its next start takes {pin}",
                        c.instance, c.device
                    ),
                ));
            }
            continue;
        }
        if c.veth_lost {
            findings.push(veth_lost(c));
            continue;
        }
        let ip = match (c.running, c.addrs.as_slice()) {
            // Its own address, unless another instance holds it too (a
            // conflict, reported above): pinning it would keep it.
            (true, [a]) if held.get(a).is_some_and(|h| h.len() == 1) => Some(*a),
            // Starting (no address yet), or sharing one: the next pass.
            (true, _) => None,
            (false, _) => {
                let free = subnet.pick_free(&used, &format!("{}/{}", c.project, c.instance));
                if free.is_none() {
                    findings.push(Finding::about(
                        c,
                        format!("full {} {}", c.project, c.instance),
                        format!(
                            "{}: no free address on {network} to pin {} to",
                            c.instance, c.device
                        ),
                    ));
                }
                free
            }
        };
        if let Some(ip) = ip {
            used.insert(ip);
            let mut props = c.props.clone();
            props.insert("ipv4.address".into(), ip.to_string());
            fixes.push(Fix::Pin {
                project: c.project.clone(),
                instance: c.instance.clone(),
                device: c.device.clone(),
                props,
                ip,
                running: c.running,
            });
        }
    }
    (fixes, findings)
}

/// An unpinned instance whose NIC incus cannot change while it runs
/// (2026-11-09, analytics-capture in org exa): what to do about it.
fn veth_lost(c: &Claim) -> Finding {
    let (i, d, p) = (&c.instance, &c.device, &c.project);
    Finding::about(
        c,
        format!("veth {p} {i} {d}"),
        format!(
            "{i}'s {d} cannot be pinned while it runs: incus lost its host side name \
             (volatile.{d}.host_name), and an org project may not set it. isb pins it after its \
             next restart; until then {} can be given to another instance once its lease lapses. \
             To pin it without a restart: incus project set {p} restricted.containers.lowlevel=allow, \
             incus config set {i} volatile.{d}.host_name=<the host_name `incus query \
             /1.0/instances/{i}/state?project={p}` gives {d}>, incus project unset {p} \
             restricted.containers.lowlevel; the next check pins it",
            if c.addrs.is_empty() {
                "its address".to_string()
            } else {
                join(&c.addrs)
            }
        ),
    )
}

fn join(ips: &[Ipv4Addr]) -> String {
    ips.iter()
        .map(Ipv4Addr::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Every address two or more instances hold, as one finding about the
/// newest of them (most likely the one that took it).
fn conflicts(
    network: &str,
    claims: &[Claim],
    held: &BTreeMap<Ipv4Addr, BTreeSet<(String, String)>>,
) -> Vec<Finding> {
    let mut out = Vec::new();
    for (ip, ids) in held.iter().filter(|(_, h)| h.len() > 1) {
        let mut who: Vec<&Claim> = claims.iter().filter(|c| ids.contains(&c.id())).collect();
        who.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        who.dedup_by(|a, b| a.id() == b.id());
        let how = |c: &Claim| match (c.pin == Some(*ip), c.addrs.contains(ip)) {
            (true, true) => "pinned, has it",
            (true, false) => "pinned",
            _ => "has it",
        };
        let list: Vec<String> = who
            .iter()
            .map(|c| format!("{} ({})", c.instance, how(c)))
            .collect();
        let names: Vec<&str> = who.iter().map(|c| c.instance.as_str()).collect();
        out.push(Finding::about(
            who[0],
            format!("conflict {network} {ip} {}", names.join(" ")),
            format!(
                "{ip} on {network} is held by {}: traffic for one reaches another. isb moves \
                 neither; pin one elsewhere (incus config device set <instance> {} ipv4.address=<free>) \
                 and restart it",
                list.join(", "),
                who[0].device
            ),
        ));
    }
    out
}

/// Addresses being given to instances in the middle of their create, by
/// network: what this process's other creates and checks must not pick.
fn reserved() -> &'static Mutex<BTreeMap<String, BTreeSet<Ipv4Addr>>> {
    static R: OnceLock<Mutex<BTreeMap<String, BTreeSet<Ipv4Addr>>>> = OnceLock::new();
    R.get_or_init(Mutex::default)
}

fn reserved_on(network: &str) -> BTreeSet<Ipv4Addr> {
    let r = reserved().lock().unwrap_or_else(|e| e.into_inner());
    r.get(network).cloned().unwrap_or_default()
}

/// Every instance with its state: on every project when the client may
/// list them (the daemon), else on its own.
fn list(client: &Client) -> Result<Vec<Value>> {
    let v = match client.get("/1.0/instances?recursion=2&all-projects=true") {
        Ok(v) => v,
        Err(_) => client.get("/1.0/instances?recursion=2")?,
    };
    Ok(v.as_array().cloned().unwrap_or_default())
}

/// An address pinned for an instance being created. Held until the create
/// is done; [`Pin::verify`] checks nobody else took it in between.
pub struct Pin {
    network: String,
    subnet: Subnet,
    device: String,
    ip: Ipv4Addr,
}

impl Drop for Pin {
    fn drop(&mut self) {
        let mut r = reserved().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = r.get_mut(&self.network) {
            s.remove(&self.ip);
        }
    }
}

impl Pin {
    pub fn ip(&self) -> Ipv4Addr {
        self.ip
    }

    /// After the create: when another instance holds the address too (a
    /// create by another process that picked it at the same moment), move
    /// this one, which has not started yet.
    pub fn verify(mut self, client: &Client, name: &str) -> Result<()> {
        let me = (client.project_name().to_string(), name.to_string());
        for _ in 0..3 {
            let all = claims(&list(client)?, &self.network, &self.subnet);
            let held = holders(&all);
            if held
                .get(&self.ip)
                .is_none_or(|h| h.iter().all(|id| *id == me))
            {
                return Ok(());
            }
            let mut used: BTreeSet<Ipv4Addr> = held.keys().copied().collect();
            used.extend(reserved_on(&self.network));
            let ip = self
                .subnet
                .pick_free(&used, &format!("{name}/{}", self.ip))
                .ok_or_else(|| Error::invalid(format!("no free address on {}", self.network)))?;
            set_pin(client, name, &self.device, None, ip)?;
            self.reserve(ip);
        }
        Err(Error::invalid(format!(
            "{name}: could not find an address on {} no other instance takes",
            self.network
        )))
    }

    fn reserve(&mut self, ip: Ipv4Addr) {
        let mut r = reserved().lock().unwrap_or_else(|e| e.into_inner());
        let s = r.entry(self.network.clone()).or_default();
        s.remove(&self.ip);
        s.insert(ip);
        self.ip = ip;
    }
}

/// Pin `eth0` in a create body (`POST /1.0/instances`) for an instance in
/// one of isb's org projects whose `eth0` joins a managed bridge: the
/// body's own NIC, else its profiles'. Anything else is returned as is.
pub fn pin_for_create(client: &Client, mut body: Value) -> Result<(Value, Option<Pin>)> {
    if OrgId::from_incus_project(client.project_name()).is_none() {
        return Ok((body, None));
    }
    let Some(mut nic) = eth0(client, &body)? else {
        return Ok((body, None));
    };
    let Some(network) = nic["network"].as_str().map(String::from) else {
        return Ok((body, None));
    };
    if !nic["ipv4.address"].is_null() {
        return Ok((body, None));
    }
    let path = format!("/1.0/networks/{}", encode_segment(&network));
    let Some(subnet) = client
        .get_opt(&path)?
        .as_ref()
        .and_then(Subnet::from_network)
    else {
        return Ok((body, None));
    };
    let name = body["name"].as_str().unwrap_or_default().to_string();
    let held = holders(&claims(&list(client)?, &network, &subnet));
    let ip = {
        let mut r = reserved().lock().unwrap_or_else(|e| e.into_inner());
        let res = r.entry(network.clone()).or_default();
        let used: BTreeSet<Ipv4Addr> = held.keys().chain(res.iter()).copied().collect();
        let ip = subnet
            .pick_free(&used, &format!("{}/{name}", client.project_name()))
            .ok_or_else(|| Error::invalid(format!("{name}: no free address on {network}")))?;
        res.insert(ip);
        ip
    };
    let pin = Pin {
        network,
        subnet,
        device: "eth0".into(),
        ip,
    };
    nic["ipv4.address"] = json!(ip.to_string());
    body["devices"]["eth0"] = nic;
    body["config"][pin_key("eth0")] = json!(ip.to_string());
    Ok((body, Some(pin)))
}

/// The `eth0` NIC an instance created from `body` gets: its own, else the
/// last of its profiles' (a later profile overrides an earlier one).
fn eth0(client: &Client, body: &Value) -> Result<Option<Value>> {
    if let Some(d) = body["devices"].get("eth0") {
        return Ok((d["type"] == "nic").then(|| d.clone()));
    }
    let mut nic = None;
    for p in body["profiles"].as_array().into_iter().flatten() {
        let Some(p) = p.as_str() else { continue };
        let path = format!("/1.0/profiles/{}", encode_segment(p));
        if let Some(d) = client
            .get_opt(&path)?
            .and_then(|v| v["devices"].get("eth0").cloned())
        {
            nic = (d["type"] == "nic").then_some(d);
        }
    }
    Ok(nic)
}

/// Pin an instance's NIC to `ip`, in place: `props` is the NIC to write
/// (from a profile), or `None` to change the instance's own.
fn set_pin(
    client: &Client,
    name: &str,
    device: &str,
    props: Option<&Props>,
    ip: Ipv4Addr,
) -> Result<()> {
    crate::sandbox::update_instance(
        client,
        name,
        &format!("pin {name} {device}"),
        &mut |config, devices| {
            let mut nic = match props {
                Some(p) => json!(p),
                None => devices
                    .get(device)
                    .cloned()
                    .ok_or_else(|| Error::invalid(format!("{name} has no device {device}")))?,
            };
            nic["ipv4.address"] = json!(ip.to_string());
            devices.insert(device.to_string(), nic);
            config.insert(pin_key(device), json!(ip.to_string()));
            Ok(())
        },
    )
}

/// A desired NIC that names no address is satisfied by one pinned to any:
/// the pin is isb's, not the spec's (for [`crate::plan::device_matches`]).
pub fn ignore_pin(desired: &Props, actual: &mut Props) {
    if desired.get("type").is_some_and(|t| t == "nic") && !desired.contains_key("ipv4.address") {
        actual.remove("ipv4.address");
    }
}

/// Whether an instance's device is a NIC isb pinned (never pruned).
pub fn is_pin(device: &str, props: &Props, config: &Props) -> bool {
    props.get("type").is_some_and(|t| t == "nic")
        && props.get("ipv4.address").is_some()
        && props.get("ipv4.address") == config.get(&pin_key(device))
}

/// An instance's devices but the NICs isb pinned: what pruning may remove.
pub fn unpinned<'a>(
    devices: &'a BTreeMap<String, Props>,
    config: &'a Props,
) -> impl Iterator<Item = (&'a String, &'a Props)> {
    devices.iter().filter(|(n, p)| !is_pin(n, p, config))
}

/// One pass over every org bridge: apply the fixes, return the findings.
/// `only`: the bridges of this incus project alone (a test's org). `report`
/// hears each fix.
pub fn check(
    client: &Client,
    only: Option<&str>,
    report: &mut dyn FnMut(&str),
) -> Result<Vec<Finding>> {
    let mut nets: BTreeMap<String, Subnet> = client
        .get("/1.0/networks?recursion=1")?
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| Some((n["name"].as_str()?.to_string(), Subnet::from_network(n)?)))
        .collect();
    let instances = list(client)?;
    if let Some(p) = only {
        let mine = |i: &Value| i["project"].as_str() == Some(p);
        let used: BTreeSet<&str> = instances
            .iter()
            .filter(|i| mine(i))
            .filter_map(|i| i["expanded_devices"].as_object())
            .flat_map(|d| d.values().filter_map(|n| n["network"].as_str()))
            .collect();
        let used: BTreeSet<String> = used.into_iter().map(String::from).collect();
        nets.retain(|n, _| used.contains(n));
    }
    let mut findings = Vec::new();
    for (network, subnet) in &nets {
        let cl = claims(&instances, network, subnet);
        if cl.is_empty() {
            continue;
        }
        let (fixes, found) = plan(network, subnet, &cl, &reserved_on(network));
        findings.extend(found);
        for fix in fixes {
            if let Some(f) = apply(client, &fix, &cl, report) {
                findings.push(f);
            }
        }
    }
    Ok(findings)
}

/// Apply one fix; a failure is a finding.
fn apply(
    client: &Client,
    fix: &Fix,
    cl: &[Claim],
    report: &mut dyn FnMut(&str),
) -> Option<Finding> {
    let (project, instance, device, r) = match fix {
        Fix::Adopt {
            project,
            instance,
            device,
            ip,
        } => {
            let c = client.clone().project(project.as_str());
            let r = crate::sandbox::update_instance(
                &c,
                instance,
                &format!("adopt {instance}'s pin"),
                &mut |config, _| {
                    config.insert(pin_key(device), json!(ip.to_string()));
                    Ok(())
                },
            );
            if r.is_ok() {
                report(&format!(
                    "{project}/{instance}: {device}'s pin to {ip} is isb's now"
                ));
            }
            (project, instance, device, r)
        }
        Fix::Pin {
            project,
            instance,
            device,
            props,
            ip,
            running,
        } => {
            let c = client.clone().project(project.as_str());
            let r = set_pin(&c, instance, device, Some(props), *ip);
            if r.is_ok() {
                let how = if *running {
                    "its address"
                } else {
                    "a free address"
                };
                report(&format!(
                    "{project}/{instance}: pinned {device} to {ip} ({how})"
                ));
            }
            (project, instance, device, r)
        }
    };
    r.err().map(|e| {
        let c = cl
            .iter()
            .find(|c| c.project == *project && c.instance == *instance);
        let mut f = Finding {
            key: format!("unpinned {project} {instance} {device}"),
            project: project.clone(),
            instance: instance.clone(),
            stack: None,
            service: None,
            message: format!(
                "{instance}'s {device} has no pin isb could set ({e}): once its lease lapses, \
                 another instance can be given its address"
            ),
        };
        if let Some(c) = c {
            (f.stack, f.service) = (c.stack.clone(), c.service.clone());
        }
        f
    })
}

/// The daemon's check, on a thread of its own: [`check`] every
/// [`CHECK_EVERY`], raising `net.failed` once per problem until it is gone.
pub fn watch(client: Client, ctl: crate::stack::Controller) {
    let _ = std::thread::Builder::new()
        .name("isb-ipam".into())
        .spawn(move || {
            let mut raised: BTreeSet<String> = BTreeSet::new();
            loop {
                let mut log = |m: &str| eprintln!("isb serve: addresses: {m}");
                match check(&client, None, &mut log) {
                    Ok(found) => {
                        let now: BTreeSet<String> = found.iter().map(|f| f.key.clone()).collect();
                        for f in found.iter().filter(|f| !raised.contains(&f.key)) {
                            raise(&ctl, f);
                        }
                        raised = now;
                    }
                    Err(e) => log(&format!("check failed: {e}")),
                }
                std::thread::sleep(CHECK_EVERY);
            }
        });
}

fn raise(ctl: &crate::stack::Controller, f: &Finding) {
    let org = OrgId::from_incus_project(&f.project).unwrap_or_else(OrgId::default_org);
    let stack = crate::stack::qualified(&org, f.stack.as_deref().unwrap_or_default());
    ctl.instance_event(
        "net.failed",
        "error",
        &stack,
        f.service.as_deref().unwrap_or_default(),
        &f.instance,
        f.message.clone(),
    );
}

#[cfg(test)]
mod tests;
