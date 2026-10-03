//! Orgs: the trust boundary. An org is an incus project; its people and
//! agents fully administer what is in it, and nothing crosses orgs.
//!
//! Any org `x` is the incus project `isb-x`, and on a fresh host the
//! `default` org is too (`isb-default`), with its own network and service
//! names. A host whose incus `default` project already held workloads when
//! isb first ran keeps the legacy mapping: the `default` org *is* the incus
//! `default` project, unrestricted and without an org network, so what
//! predates orgs keeps working where it is. [`resolve_default`] decides
//! once per process; [`OrgId::is_legacy_default`] tells the two apart.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// A validated org name: `[a-z][a-z0-9-]{0,30}`, not ending in `-`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct OrgId(String);

pub const DEFAULT_ORG: &str = "default";
/// The incus project of a non-legacy default org.
pub const DEFAULT_ORG_PROJECT: &str = "isb-default";

/// 0: not resolved (treated as legacy), 1: legacy, 2: `isb-default`.
static DEFAULT_MODE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

fn default_is_legacy() -> bool {
    DEFAULT_MODE.load(std::sync::atomic::Ordering::Relaxed) != 2
}

fn set_default_mode(legacy: bool) {
    DEFAULT_MODE.store(
        if legacy { 1 } else { 2 },
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// Decide where the default org lives on this host: `isb-default` when that
/// project exists as isb's default org, else the legacy incus `default`
/// project. Cheap after the first call; an unreachable incus means legacy.
pub fn resolve_default(base: &Client) {
    if DEFAULT_MODE.load(std::sync::atomic::Ordering::Relaxed) != 0 {
        return;
    }
    let modern = host(base)
        .get_opt(&format!("/1.0/projects/{DEFAULT_ORG_PROJECT}"))
        .ok()
        .flatten()
        .is_some_and(|p| p["config"][KEY_ORG].as_str() == Some(DEFAULT_ORG));
    set_default_mode(!modern);
}

/// On a fresh host (incus `default` holds no instances and isb has no
/// default-org state, per `has_state`), make the default org a real org in
/// `isb-default`. Returns whether it did. A host that already resolved to
/// either mapping is left as it is.
pub fn adopt_default(
    base: &Client,
    opts: &OrgOptions,
    has_state: bool,
    report: &mut dyn FnMut(&str),
) -> Result<bool> {
    DEFAULT_MODE.store(0, std::sync::atomic::Ordering::Relaxed);
    resolve_default(base);
    if !default_is_legacy() {
        return Ok(false);
    }
    let in_default = host(base)
        .get("/1.0/instances")?
        .as_array()
        .map(Vec::len)
        .unwrap_or(0);
    if has_state || in_default > 0 {
        return Ok(false);
    }
    set_default_mode(false);
    match ensure(base, &OrgId::default_org(), opts, report) {
        Ok(_) => Ok(true),
        Err(e) => {
            set_default_mode(true);
            Err(e)
        }
    }
}
/// Not an org: `isb-system` is [`crate::registry::PROJECT`].
const RESERVED_SYSTEM: &str = "system";

impl OrgId {
    pub fn new(s: impl Into<String>) -> Result<OrgId> {
        let s = s.into();
        let ok = !s.is_empty()
            && s.len() <= 31
            && s.starts_with(|c: char| c.is_ascii_lowercase())
            && !s.ends_with('-')
            && s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if s == RESERVED_SYSTEM {
            Err(Error::invalid(
                "org name \"system\" is reserved: incus project isb-system holds isb's own services",
            ))
        } else if ok {
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

    /// The default org on a host that keeps the legacy mapping: the incus
    /// `default` project, with no org network, ACL or service names.
    pub fn is_legacy_default(&self) -> bool {
        self.is_default() && default_is_legacy()
    }

    /// The incus project holding this org.
    pub fn incus_project(&self) -> String {
        self.project_with(default_is_legacy())
    }

    fn project_with(&self, legacy: bool) -> String {
        if self.is_default() && legacy {
            "default".into()
        } else {
            format!("isb-{}", self.0)
        }
    }

    /// The org a project belongs to, if it is one of isb's.
    pub fn from_incus_project(project: &str) -> Option<OrgId> {
        Self::from_project_with(project, default_is_legacy())
    }

    fn from_project_with(project: &str, legacy: bool) -> Option<OrgId> {
        if project == "default" {
            return legacy.then(OrgId::default_org);
        }
        project
            .strip_prefix("isb-")
            .and_then(|o| OrgId::new(o).ok())
            .filter(|o| !o.is_default() || !legacy)
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
use std::collections::BTreeMap;

/// Config keys isb keeps on the org's project.
const KEY_ORG: &str = "user.isb.org";
const KEY_NETWORK: &str = "user.isb.network";
const KEY_EGRESS: &str = "user.isb.egress";
const KEY_DOMAINS: &str = "user.isb.domains";
const KEY_INGRESS: &str = "user.isb.ingress";
const KEY_CF_ACCOUNT: &str = "user.isb.ingress.cloudflare.account";
const KEY_CF_ZONE: &str = "user.isb.ingress.cloudflare.zone";

/// How an org's domains reach it: Caddy's public listeners (default) or the
/// org's own Cloudflare Tunnel.
pub const INGRESS_CADDY: &str = "caddy";
pub const INGRESS_CLOUDFLARE_TUNNEL: &str = "cloudflare-tunnel";

/// Check an allowlist entry: a domain suffix (`example.com`), or
/// `*.example.com` to allow wildcard hosts under it too.
pub fn check_domain_suffix(s: &str) -> Result<String> {
    let s = s.trim().to_ascii_lowercase();
    let base = s.strip_prefix("*.").unwrap_or(&s);
    if base.starts_with("*.") {
        return Err(Error::invalid(format!(
            "--allow-domain {s:?}: one * at most"
        )));
    }
    crate::ingress::domain::check_host(base)
        .map_err(|e| Error::invalid(format!("--allow-domain {s:?}: {e}")))?;
    Ok(s)
}

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
    /// Private destinations the org may reach despite the default deny.
    /// `None` keeps what the org has; `Some` replaces it.
    pub egress: Option<Vec<Egress>>,
    /// Domain suffixes the org's services may serve; `Some(empty)` clears,
    /// `None` keeps.
    pub domains: Option<Vec<String>>,
    /// `caddy` or `cloudflare-tunnel`; `None` keeps.
    pub ingress: Option<String>,
    /// Cloudflare account and zone ids for the tunnel provider's API calls
    /// (`Some("")` clears).
    pub cloudflare_account: Option<String>,
    pub cloudflare_zone: Option<String>,
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
    /// What an instance gets when its spec sets no limits (the org's
    /// default profile).
    pub default_cpus: Option<String>,
    pub default_memory: Option<String>,
    pub bind_roots: Vec<String>,
    /// Egress exceptions, as `isb org create --allow-egress` takes them.
    pub egress: Vec<String>,
    /// Domain suffixes its services may serve (empty: any concrete name).
    pub domains: Vec<String>,
    /// `caddy` or `cloudflare-tunnel`.
    pub ingress: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cloudflare_account: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cloudflare_zone: Option<String>,
    /// The hosts directory the org's dnsmasq reads service names from, when
    /// service discovery is on.
    pub dns_dir: Option<String>,
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
const PRIVATE: [&str; 5] = [
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "100.64.0.0/10",
    "169.254.0.0/16",
];

fn parse_cidr(s: &str) -> Option<(u32, u32)> {
    let (ip, len) = s.split_once('/')?;
    let ip: std::net::Ipv4Addr = ip.parse().ok()?;
    let len: u32 = len.parse().ok().filter(|l| *l <= 32)?;
    let mask = if len == 0 { 0 } else { u32::MAX << (32 - len) };
    Some((u32::from(ip) & mask, len))
}

fn mask(len: u32) -> u32 {
    if len == 0 { 0 } else { u32::MAX << (32 - len) }
}

fn fmt_cidr(c: (u32, u32)) -> String {
    format!("{}/{}", std::net::Ipv4Addr::from(c.0), c.1)
}

/// Whether two CIDRs share an address (then one contains the other).
fn overlaps(a: (u32, u32), b: (u32, u32)) -> bool {
    let l = a.1.min(b.1);
    a.0 & mask(l) == b.0 & mask(l)
}

/// `range` minus `hole`, as CIDRs: halve the range until the hole is
/// carved out exactly.
fn subtract(range: (u32, u32), hole: (u32, u32), out: &mut Vec<(u32, u32)>) {
    let (net, len) = range;
    if !overlaps(range, hole) {
        out.push(range);
    } else if hole.1 > len {
        let half = 1u32 << (31 - len);
        subtract((net, len + 1), hole, out);
        subtract((net | half, len + 1), hole, out);
    }
    // Otherwise the hole covers the whole range: nothing is left of it.
}

/// What an org's ACL rejects: every private range minus the holes (its own
/// subnet and its egress exceptions). incus applies reject rules before
/// allow rules, so an exception has to be carved out of the ranges rather
/// than allowed on top of them.
fn denied_ranges(holes: &[(u32, u32)]) -> Vec<String> {
    let mut ranges: Vec<(u32, u32)> = PRIVATE
        .iter()
        .map(|r| parse_cidr(r).expect("constant"))
        .collect();
    for h in holes {
        let mut next = Vec::new();
        for r in ranges {
            subtract(r, *h, &mut next);
        }
        ranges = next;
    }
    ranges.into_iter().map(fmt_cidr).collect()
}

/// An egress exception: a private destination an org may reach despite the
/// default deny. Written `CIDR[:PORTS[/PROTO]]`: `10.1.2.0/24` (everything
/// there), `100.79.171.47:1080` (one TCP port), `10.1.2.3:53/udp`,
/// `10.1.2.3:8000-8100,9000/tcp`. A bare address is a /32.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Egress {
    net: (u32, u32),
    /// `None`: every port and protocol.
    ports: Option<(Proto, Vec<(u16, u16)>)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Proto {
    Tcp,
    Udp,
}

impl Proto {
    fn as_str(self) -> &'static str {
        match self {
            Proto::Tcp => "tcp",
            Proto::Udp => "udp",
        }
    }
}

impl Egress {
    pub fn parse(s: &str) -> Result<Egress> {
        let bad = |why: &str| {
            Error::invalid(format!(
                "egress exception {s:?}: {why} (want CIDR[:PORTS[/tcp|udp]], e.g. 100.79.171.47/32:1080/tcp)"
            ))
        };
        let (addr, rest) = match s.split_once(':') {
            Some((a, r)) => (a, Some(r)),
            None => (s, None),
        };
        let addr = if addr.contains('/') {
            addr.to_string()
        } else {
            format!("{addr}/32")
        };
        let net = parse_cidr(&addr).ok_or_else(|| bad("not an IPv4 address or CIDR"))?;
        let ports = match rest {
            None => None,
            Some(r) => {
                let (list, proto) = match r.split_once('/') {
                    Some((l, "tcp")) => (l, Proto::Tcp),
                    Some((l, "udp")) => (l, Proto::Udp),
                    Some(_) => return Err(bad("the protocol must be tcp or udp")),
                    None => (r, Proto::Tcp),
                };
                let mut ranges = Vec::new();
                for p in list.split(',') {
                    let (a, b) = p.split_once('-').unwrap_or((p, p));
                    let a: u16 = a.parse().map_err(|_| bad("bad port"))?;
                    let b: u16 = b.parse().map_err(|_| bad("bad port"))?;
                    if a == 0 || b < a {
                        return Err(bad("bad port range"));
                    }
                    ranges.push((a, b));
                }
                Some((proto, merge_ports(ranges)))
            }
        };
        Ok(Egress { net, ports })
    }

    /// The canonical spelling, as stored on the org.
    pub fn render(&self) -> String {
        let mut s = fmt_cidr(self.net);
        if let Some((proto, ranges)) = &self.ports {
            s.push(':');
            s.push_str(&fmt_ports(ranges));
            s.push('/');
            s.push_str(proto.as_str());
        }
        s
    }
}

impl std::fmt::Display for Egress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.render())
    }
}

impl TryFrom<String> for Egress {
    type Error = Error;
    fn try_from(s: String) -> Result<Egress> {
        Egress::parse(&s)
    }
}

impl From<Egress> for String {
    fn from(e: Egress) -> String {
        e.render()
    }
}

/// Exceptions as stored in `user.isb.egress`: space-separated.
fn parse_egress_list(s: &str) -> Vec<Egress> {
    s.split_whitespace()
        .filter_map(|e| Egress::parse(e).ok())
        .collect()
}

fn merge_ports(mut r: Vec<(u16, u16)>) -> Vec<(u16, u16)> {
    r.sort();
    let mut out: Vec<(u16, u16)> = Vec::new();
    for (a, b) in r {
        match out.last_mut() {
            Some(l) if a as u32 <= l.1 as u32 + 1 => l.1 = l.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

/// Ports 1-65535 not in `r` (merged and sorted).
fn complement_ports(r: &[(u16, u16)]) -> Vec<(u16, u16)> {
    let mut out = Vec::new();
    let mut next: u32 = 1;
    for &(a, b) in r {
        if (a as u32) > next {
            out.push((next as u16, a - 1));
        }
        next = b as u32 + 1;
    }
    if next <= 65535 {
        out.push((next as u16, 65535));
    }
    out
}

fn fmt_ports(r: &[(u16, u16)]) -> String {
    r.iter()
        .map(|&(a, b)| {
            if a == b {
                a.to_string()
            } else {
                format!("{a}-{b}")
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// Exceptions for different networks must not overlap: a port-limited one
/// would otherwise cut into the other. The same network may repeat (its
/// ports add up).
pub fn check_egress(rules: &[Egress]) -> Result<()> {
    for (i, a) in rules.iter().enumerate() {
        for b in &rules[i + 1..] {
            if a.net != b.net && overlaps(a.net, b.net) {
                return Err(Error::invalid(format!(
                    "egress exceptions {a} and {b} overlap; use the same network for both"
                )));
            }
        }
    }
    Ok(())
}

/// The org ACL's egress rules. Reject the private ranges minus the org's
/// subnet and every exception's network; then, since incus orders rejects
/// before allows whatever the rules say, limit a port-specific exception by
/// rejecting the rest of its network's TCP and UDP ports, and ICMP. Other IP
/// protocols to such a network are not filtered.
fn egress_rules(own: Option<(u32, u32)>, egress: &[Egress]) -> Result<Vec<Value>> {
    check_egress(egress)?;
    // An exception outside the private ranges is allowed anyway.
    let private: Vec<(u32, u32)> = PRIVATE
        .iter()
        .map(|r| parse_cidr(r).expect("constant"))
        .collect();
    let egress: Vec<&Egress> = egress
        .iter()
        .filter(|e| private.iter().any(|p| overlaps(*p, e.net)))
        .collect();
    let mut holes: Vec<(u32, u32)> = own.into_iter().collect();
    holes.extend(egress.iter().map(|e| e.net));
    let mut out = vec![json!({
        "action": "reject",
        "destination": denied_ranges(&holes).join(","),
        "state": "enabled",
        "description": "other orgs and private networks",
    })];
    // Per network: `None` = everything allowed, else the allowed ports.
    type Allowed = Option<BTreeMap<Proto, Vec<(u16, u16)>>>;
    let mut nets: BTreeMap<(u32, u32), Allowed> = BTreeMap::new();
    for e in egress {
        let slot = nets.entry(e.net).or_insert_with(|| Some(BTreeMap::new()));
        match (&e.ports, slot.as_mut()) {
            (None, _) => *slot = None,
            (Some((p, r)), Some(m)) => m.entry(*p).or_default().extend(r.iter().copied()),
            (Some(_), None) => {}
        }
    }
    for (net, allowed) in nets {
        let Some(allowed) = allowed else { continue };
        let dest = fmt_cidr(net);
        for proto in [Proto::Tcp, Proto::Udp] {
            let mut rule = json!({
                "action": "reject",
                "destination": dest,
                "protocol": proto.as_str(),
                "state": "enabled",
                "description": format!("egress exception {dest}: other {} ports", proto.as_str()),
            });
            if let Some(r) = allowed.get(&proto) {
                let rest = complement_ports(&merge_ports(r.clone()));
                if rest.is_empty() {
                    continue;
                }
                rule["destination_port"] = json!(fmt_ports(&rest));
            }
            out.push(rule);
        }
        out.push(json!({
            "action": "reject",
            "destination": dest,
            "protocol": "icmp4",
            "state": "enabled",
            "description": format!("egress exception {dest}: ICMP"),
        }));
    }
    Ok(out)
}

/// The client to use for an org: its project.
pub fn client(base: &Client, org: &OrgId) -> Client {
    resolve_default(base);
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
                .map(|(k, v)| {
                    (
                        k.clone(),
                        v.as_str()
                            .map(String::from)
                            .unwrap_or_else(|| v.to_string()),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Create an org, or bring an existing one in line with `opts`. A legacy
/// default org (the incus default project) has no settings.
pub fn ensure(
    base: &Client,
    org: &OrgId,
    opts: &OrgOptions,
    report: &mut dyn FnMut(&str),
) -> Result<OrgInfo> {
    resolve_default(base);
    if org.is_legacy_default() {
        return Err(Error::invalid(
            "the default org on this host is incus' default project (it held workloads before isb's orgs); it has no settings",
        ));
    }
    let h = host(base);
    let project = org.incus_project();
    let proj_path = format!("/1.0/projects/{}", encode_segment(&project));
    let existing = h.get_opt(&proj_path)?;
    if let Some(p) = &existing {
        if p["config"][KEY_ORG].as_str() != Some(org.as_str()) {
            return Err(Error::AlreadyExists(format!(
                "incus project {project} exists but is not isb org {org}"
            )));
        }
    }
    let egress: Vec<Egress> = match &opts.egress {
        Some(e) => e.clone(),
        None => existing
            .as_ref()
            .and_then(|p| p["config"][KEY_EGRESS].as_str())
            .map(parse_egress_list)
            .unwrap_or_default(),
    };
    check_egress(&egress)?;
    let keep = |key: &str| -> String {
        existing
            .as_ref()
            .and_then(|p| p["config"][key].as_str())
            .unwrap_or_default()
            .to_string()
    };
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

    // Service discovery: the org's dnsmasq reads its hosts directory. Set at
    // creation, since changing raw.dnsmasq later restarts dnsmasq.
    let dns_dir = crate::discovery::prepare_org(org)?;
    if dns_dir.is_none() {
        report(&format!(
            "{org}: no writable {}: service names are off (run `sudo isb host setup`, then this again)",
            crate::discovery::root().display()
        ));
    }
    let raw_dnsmasq = dns_dir
        .as_deref()
        .map(crate::discovery::raw_dnsmasq)
        .unwrap_or_default();

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
    let net = h.get(&net_path)?;
    let subnet = net["config"]["ipv4.address"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let own = subnet_of(&subnet).and_then(|s| parse_cidr(&s));

    // Deny private ranges, except the org's own subnet (which holds its DNS)
    // and its exceptions.
    let acl = acl_name(org);
    let acl_body = json!({
        "description": format!("isb org {org}: allow within the org, deny other private networks"),
        "egress": egress_rules(own, &egress)?,
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
    if !raw_dnsmasq.is_empty() && net["config"]["raw.dnsmasq"].as_str() != Some(&raw_dnsmasq) {
        report(&format!(
            "{org}: turning on service names (restarts {bridge}'s DNS)"
        ));
        cfg["raw.dnsmasq"] = json!(raw_dnsmasq);
        changed.push("set raw.dnsmasq");
    }
    if !changed.is_empty() {
        h.mutate(
            "PATCH",
            &net_path,
            Some(&json!({"config": cfg})),
            &format!("{} on {bridge}", changed.join(", ")),
            h.get_timeouts().other,
        )?;
    }

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
        KEY_EGRESS: egress.iter().map(Egress::render).collect::<Vec<_>>().join(" "),
        KEY_DOMAINS: domains,
        KEY_INGRESS: ingress,
        KEY_CF_ACCOUNT: cf_account,
        KEY_CF_ZONE: cf_zone,
    });
    let roots: Vec<String> = opts
        .bind_roots
        .iter()
        .map(|p| p.display().to_string())
        .collect();
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
    match existing {
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
    oc.mutate(
        "PUT",
        "/1.0/profiles/default",
        Some(&profile),
        &format!("set {org}'s default profile"),
        oc.get_timeouts().other,
    )?;
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
    Some(format!(
        "{}/{len}",
        std::net::Ipv4Addr::from(u32::from(ip) & mask)
    ))
}

fn info(base: &Client, org: OrgId, p: &Value) -> Result<OrgInfo> {
    let cfg = strmap(&p["config"]);
    let network = cfg.get(KEY_NETWORK).cloned();
    let net = match &network {
        Some(n) => host(base).get_opt(&format!("/1.0/networks/{}", encode_segment(n)))?,
        None => None,
    };
    let subnet = net
        .as_ref()
        .and_then(|v| v["config"]["ipv4.address"].as_str().map(String::from));
    let dns_dir = net.as_ref().and_then(|v| {
        v["config"]["raw.dnsmasq"]
            .as_str()?
            .lines()
            .find_map(|l| l.trim().strip_prefix("hostsdir=").map(String::from))
    });
    let oc = client(base, &org);
    let instances = oc
        .get("/1.0/instances")?
        .as_array()
        .map(|a| a.len())
        .unwrap_or(0);
    let defaults = strmap(&oc.get_opt("/1.0/profiles/default")?.unwrap_or_default()["config"]);
    Ok(OrgInfo {
        project: org.incus_project(),
        name: org,
        network,
        subnet,
        cpus: cfg.get("limits.cpu").cloned(),
        memory: cfg.get("limits.memory").cloned(),
        disk: cfg.get("limits.disk").cloned(),
        instances_limit: cfg.get("limits.instances").cloned(),
        default_cpus: defaults.get("limits.cpu").cloned(),
        default_memory: defaults.get("limits.memory").cloned(),
        bind_roots: cfg
            .get("restricted.devices.disk.paths")
            .map(|s| {
                s.split(',')
                    .filter(|x| !x.is_empty())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default(),
        egress: cfg
            .get(KEY_EGRESS)
            .map(|s| s.split_whitespace().map(String::from).collect())
            .unwrap_or_default(),
        domains: cfg
            .get(KEY_DOMAINS)
            .map(|s| s.split_whitespace().map(String::from).collect())
            .unwrap_or_default(),
        ingress: cfg
            .get(KEY_INGRESS)
            .filter(|s| !s.is_empty())
            .cloned()
            .unwrap_or_else(|| INGRESS_CADDY.to_string()),
        cloudflare_account: cfg.get(KEY_CF_ACCOUNT).filter(|s| !s.is_empty()).cloned(),
        cloudflare_zone: cfg.get(KEY_CF_ZONE).filter(|s| !s.is_empty()).cloned(),
        dns_dir,
        instances,
    })
}

/// One org.
pub fn get(base: &Client, org: &OrgId) -> Result<OrgInfo> {
    resolve_default(base);
    let h = host(base);
    let p = h
        .get_opt(&format!(
            "/1.0/projects/{}",
            encode_segment(&org.incus_project())
        ))?
        .ok_or_else(|| Error::NotFound(format!("org {org}")))?;
    if !org.is_legacy_default() && p["config"][KEY_ORG].as_str() != Some(org.as_str()) {
        return Err(Error::NotFound(format!("org {org}")));
    }
    info(base, org.clone(), &p)
}

/// Every org: the default one first, then isb's projects by name.
pub fn list(base: &Client) -> Result<Vec<OrgInfo>> {
    resolve_default(base);
    let h = host(base);
    let v = h.get("/1.0/projects?recursion=1")?;
    let mut out = Vec::new();
    for p in v.as_array().into_iter().flatten() {
        let name = p["name"].as_str().unwrap_or_default();
        let Some(org) = OrgId::from_incus_project(name) else {
            continue;
        };
        if !org.is_legacy_default() && p["config"][KEY_ORG].as_str() != Some(org.as_str()) {
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
    for name in oc
        .get("/1.0/instances")?
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        // `/1.0/instances/<name>?project=<project>`
        let n = name.rsplit('/').next().unwrap_or(name);
        let n = n.split('?').next().unwrap_or(n);
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
        match h.mutate(
            "DELETE",
            &format!("/1.0/networks/{}", encode_segment(n)),
            None,
            &format!("delete network {n}"),
            h.get_timeouts().other,
        ) {
            Err(e) if !e.is_not_found() => return Err(e),
            _ => {}
        }
    }
    crate::discovery::remove_org(org);
    let acl = acl_name(org);
    match h.mutate(
        "DELETE",
        &format!("/1.0/network-acls/{}", encode_segment(&acl)),
        None,
        &format!("delete ACL {acl}"),
        h.get_timeouts().other,
    ) {
        Err(e) if !e.is_not_found() => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn the_default_org_project_on_either_kind_of_host() {
        let d = OrgId::default_org();
        let acme = OrgId::new("acme").unwrap();
        // Legacy: the incus default project; isb-default is nobody's.
        assert_eq!(d.project_with(true), "default");
        assert_eq!(OrgId::from_project_with("default", true), Some(d.clone()));
        assert_eq!(OrgId::from_project_with("isb-default", true), None);
        // Fresh host: a real org; the incus default project is no org.
        assert_eq!(d.project_with(false), "isb-default");
        assert_eq!(
            OrgId::from_project_with("isb-default", false),
            Some(d.clone())
        );
        assert_eq!(OrgId::from_project_with("default", false), None);
        // Other orgs never change.
        for legacy in [true, false] {
            assert_eq!(acme.project_with(legacy), "isb-acme");
            assert_eq!(
                OrgId::from_project_with("isb-acme", legacy),
                Some(acme.clone())
            );
        }
    }
    use super::*;

    #[test]
    fn names_and_projects() {
        assert!(OrgId::new("ocai").is_ok());
        assert!(OrgId::new("Ocai").is_err());
        assert!(OrgId::new("a-").is_err());
        assert!(OrgId::new("x".repeat(32)).is_err());
        assert!(OrgId::new("system").is_err());
        assert_eq!(OrgId::from_incus_project(crate::registry::PROJECT), None);
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

    #[test]
    fn denied_ranges_carve_out_the_org() {
        let d = denied_ranges(&[parse_cidr("10.160.44.0/24").unwrap()]);
        assert!(!d.iter().any(|r| r == "10.0.0.0/8"));
        assert!(d.contains(&"172.16.0.0/12".to_string()));
        // 16 halvings from /8 to /24: 16 pieces plus the other 4 ranges.
        assert_eq!(d.len(), 16 + 4);
        let covers = |r: &str, ip: u32| {
            let (n, l) = parse_cidr(r).unwrap();
            let m = if l == 0 { 0 } else { u32::MAX << (32 - l) };
            ip & m == n
        };
        let ip = |s: &str| u32::from(s.parse::<std::net::Ipv4Addr>().unwrap());
        assert!(!d.iter().any(|r| covers(r, ip("10.160.44.7"))));
        for other in [
            "10.160.45.1",
            "10.0.0.1",
            "10.255.255.254",
            "10.238.212.250",
        ] {
            assert!(d.iter().any(|r| covers(r, ip(other))), "{other}");
        }
        assert_eq!(denied_ranges(&[]).len(), 5);
        // A hole that covers a whole range removes it.
        assert_eq!(denied_ranges(&[parse_cidr("10.0.0.0/7").unwrap()]).len(), 4);
    }

    fn covered(ranges: &str, ip: &str) -> bool {
        let ip = u32::from(ip.parse::<std::net::Ipv4Addr>().unwrap());
        ranges.split(',').any(|r| {
            let (n, l) = parse_cidr(r).unwrap();
            ip & mask(l) == n
        })
    }

    #[test]
    fn egress_parses_and_renders() {
        let e = Egress::parse("100.79.171.47/32:1080/tcp").unwrap();
        assert_eq!(e.render(), "100.79.171.47/32:1080/tcp");
        assert_eq!(
            Egress::parse("100.79.171.47:1080").unwrap(),
            e,
            "a bare address is a /32 and tcp is the default"
        );
        assert_eq!(
            Egress::parse("10.1.2.9/24").unwrap().render(),
            "10.1.2.0/24"
        );
        assert_eq!(
            Egress::parse("10.1.2.3:9000,8000-8100,8050/udp")
                .unwrap()
                .render(),
            "10.1.2.3/32:8000-8100,9000/udp"
        );
        for bad in [
            "db.example.com:5432",
            "10.1.2.3:0",
            "10.1.2.3:90-80",
            "10.1.2.3:80/sctp",
            "10.1.2.3/33",
            "10.1.2.3:http",
        ] {
            assert!(Egress::parse(bad).is_err(), "{bad}");
        }
        let j: Vec<Egress> = serde_json::from_str("[\"10.0.0.1:22\"]").unwrap();
        assert_eq!(
            serde_json::to_string(&j).unwrap(),
            "[\"10.0.0.1/32:22/tcp\"]"
        );
        assert_eq!(
            parse_egress_list("10.0.0.1/32:22/tcp  10.2.0.0/16"),
            vec![
                Egress::parse("10.0.0.1:22").unwrap(),
                Egress::parse("10.2.0.0/16").unwrap()
            ]
        );
    }

    #[test]
    fn ports_complement() {
        assert_eq!(
            complement_ports(&[(1080, 1080)]),
            vec![(1, 1079), (1081, 65535)]
        );
        assert_eq!(
            complement_ports(&[(1, 10), (65535, 65535)]),
            vec![(11, 65534)]
        );
        assert_eq!(complement_ports(&[(1, 65535)]), vec![]);
        assert_eq!(
            merge_ports(vec![(5, 9), (1, 4), (20, 30), (25, 40)]),
            vec![(1, 9), (20, 40)]
        );
    }

    #[test]
    fn egress_exceptions_in_the_acl() {
        let own = parse_cidr("10.160.44.0/24");
        let whole = Egress::parse("10.20.0.0/16").unwrap();
        let port = Egress::parse("100.79.171.47:1080").unwrap();
        let udp = Egress::parse("100.79.171.47:53/udp").unwrap();
        let public = Egress::parse("8.8.8.8:53/udp").unwrap();
        let rules = egress_rules(own, &[whole, port, udp, public]).unwrap();
        let deny = rules[0]["destination"].as_str().unwrap();
        // Carved out: the org, the whole exception, the port-limited host.
        assert!(!covered(deny, "10.160.44.9"));
        assert!(!covered(deny, "10.20.200.1"));
        assert!(!covered(deny, "100.79.171.47"));
        // Still denied around them.
        for ip in ["10.21.0.1", "100.79.171.46", "100.79.171.48", "192.168.1.1"] {
            assert!(covered(deny, ip), "{ip}");
        }
        // The port-limited host: every other TCP and UDP port, and ICMP. The
        // public exception and the whole network add nothing.
        let rest: Vec<(String, String, String)> = rules[1..]
            .iter()
            .map(|r| {
                (
                    r["destination"].as_str().unwrap().to_string(),
                    r["protocol"].as_str().unwrap().to_string(),
                    r["destination_port"].as_str().unwrap_or("").to_string(),
                )
            })
            .collect();
        let h = "100.79.171.47/32".to_string();
        assert_eq!(
            rest,
            vec![
                (h.clone(), "tcp".into(), "1-1079,1081-65535".into()),
                (h.clone(), "udp".into(), "1-52,54-65535".into()),
                (h, "icmp4".into(), String::new()),
            ]
        );
        assert!(rules.iter().all(|r| r["action"] == "reject"));

        // A port-limited exception with no UDP rejects all of UDP.
        let rules = egress_rules(own, &[Egress::parse("10.9.9.9:5432").unwrap()]).unwrap();
        assert_eq!(rules[2]["protocol"], "udp");
        assert!(rules[2].get("destination_port").is_none());
        // The same network once whole and once by port is whole.
        let rules = egress_rules(
            own,
            &[
                Egress::parse("10.9.9.9:5432").unwrap(),
                Egress::parse("10.9.9.9").unwrap(),
            ],
        )
        .unwrap();
        assert_eq!(rules.len(), 1);
        // Different, overlapping networks are refused.
        assert!(
            egress_rules(
                own,
                &[
                    Egress::parse("10.9.9.0/24:80").unwrap(),
                    Egress::parse("10.9.9.9:443").unwrap()
                ]
            )
            .is_err()
        );
    }
}
