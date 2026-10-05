//! Ingress: public hostnames for stack services (docs/guides/domains.md).
//!
//! A service's `domains:` ([`crate::spec::DomainSpec`]) become routes on an
//! HTTP(S) edge, Caddy ([`caddy`]), run by `isb serve`. The controller tells
//! the [`Manager`] which replicas are in rotation (it is the controller's
//! [`Observer`]), and the manager regenerates Caddy's whole config and loads
//! it whenever the routes or their replicas change. Certificates come from
//! ACME through Caddy.
//!
//! Per org, the domains go out through the server's public listeners
//! (`caddy`, the default) or through the org's own Cloudflare Tunnel
//! ([`cloudflare`]). An org may only serve names in its allowlist, and a
//! name served by one org is refused to every other ([`domain::resolve`]).

pub mod caddy;
pub mod cloudflare;
pub mod domain;
mod extra;
mod status;
mod tunnels;
pub use extra::{ExtraRoute, port_service, workspace_route, workspace_stack};

use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};

use crate::client::Client;
use crate::error::{Error, Result};
use crate::org::OrgId;
use crate::secrets::Secrets;
use crate::stack::controller::Observer;
use crate::stack::{Controller, StackDef};
use caddy::{Ca, Served, TunnelListener, Via};
use domain::{Claim, Conflict, Route};

/// The port each tunnel org's listener uses on its bridge address.
pub const DEFAULT_TUNNEL_PORT: u16 = 8480;

/// `isb serve`'s ingress settings.
#[derive(Debug, Clone)]
pub struct IngressConfig {
    /// Public listeners. With neither, only tunnel orgs are served.
    pub http: Option<SocketAddr>,
    pub https: Option<SocketAddr>,
    pub ca: Ca,
    pub email: Option<String>,
    /// For `host: auto` names.
    pub public_ip: Option<IpAddr>,
    pub tunnel_port: u16,
    /// A Caddy binary to run instead of the pinned download.
    pub caddy_bin: Option<PathBuf>,
    /// The Cloudflare API (a fake one in tests).
    pub cloudflare_api: String,
}

impl Default for IngressConfig {
    fn default() -> Self {
        IngressConfig {
            http: None,
            https: None,
            ca: Ca::Acme(caddy::LETSENCRYPT.into()),
            email: None,
            public_ip: None,
            tunnel_port: DEFAULT_TUNNEL_PORT,
            caddy_bin: None,
            cloudflare_api: cloudflare::API_BASE.into(),
        }
    }
}

/// The host's public IPv4 address, if its default route leaves from one:
/// the source address the kernel picks toward the internet. No packet is
/// sent. Behind NAT this is a private address and `None` comes back.
pub fn detect_public_ip() -> Option<IpAddr> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("1.1.1.1:53").ok()?;
    match s.local_addr().ok()?.ip() {
        IpAddr::V4(v4)
            if !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1])) =>
        {
            Some(IpAddr::V4(v4))
        }
        _ => None,
    }
}

/// One domain of a service, as `stack_status` reports it.
#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct DomainStatus {
    pub host: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub https: bool,
    /// `caddy` or `cloudflare-tunnel`.
    pub provider: String,
    /// `serving`, `redirect`, `no-replicas`, `conflict`, `refused` (allowlist,
    /// settings) or `off` (no listener for it).
    pub state: String,
    /// `issued`, `pending`, `failed`, `unsupported` (a wildcard needs a DNS
    /// challenge), `cloudflare` (TLS ends at Cloudflare), or `none` (plain HTTP).
    pub cert: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub upstreams: Vec<String>,
    /// The ingress listener the domain's requests come in on: the org's
    /// tunnel listener (what its cloudflared forwards to,
    /// `http://10.64.3.1:8480`), or Caddy's public one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

/// A certificate's state, from Caddy's log and storage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CertState {
    /// `issued`, `failed`, `pending`.
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub issuer: String,
    /// Unix seconds.
    pub at: u64,
}

/// An org's ingress settings, from its project.
#[derive(Debug, Clone, Default, PartialEq)]
struct OrgIngress {
    domains: Vec<String>,
    tunnel: bool,
    account: Option<String>,
    zone: Option<String>,
    /// The bridge address (`10.64.3.1/24`), for the tunnel listener.
    subnet: Option<String>,
}

/// Per tunnel org: what isb last did for it.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TunnelStatus {
    pub org: String,
    /// What cloudflared should send the org's hostnames to.
    pub origin: Option<String>,
    /// Whether the cloudflared stack is deployed.
    pub stack: bool,
    /// Whether isb manages the tunnel's ingress rules and DNS (an API token
    /// is in the org's secrets).
    pub api_managed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_sync: Option<cloudflare::SyncReport>,
    pub last_sync_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip)]
    synced_hosts: Vec<String>,
}

/// Refused routes by (qualified stack, service): (host, path, reason).
type Refused = BTreeMap<(String, String), Vec<(String, String, String)>>;
/// In-rotation replica addresses by (qualified stack, service).
type Rotation = BTreeMap<(String, String), Vec<IpAddr>>;

#[derive(Default)]
struct State {
    defs: Vec<Arc<StackDef>>,
    rotation: Rotation,
    /// Routes outside any stack: workspace ports ([`extra`]).
    extras: Vec<ExtraRoute>,
    /// Bumped by every change; the applier works up to it.
    generation: u64,
    claims: Vec<Claim>,
    certs: BTreeMap<String, CertState>,
    // What the last computation found.
    served: Vec<Served>,
    conflicts: Vec<Conflict>,
    /// Routes that cannot be served, by (qualified stack, service): the
    /// reason (allowlist, a bad `auto`, a tunnel org without settings).
    refused: Refused,
    reported: BTreeSet<String>,
    orgs: BTreeMap<OrgId, (Instant, OrgIngress)>,
    tunnels: BTreeMap<OrgId, TunnelStatus>,
    last_error: Option<String>,
}

/// The ingress: keeps Caddy's config in line with the stacks.
pub struct Manager {
    cfg: IngressConfig,
    client: Client,
    secrets: Arc<Secrets>,
    dir: PathBuf,
    state: Mutex<State>,
    wake: Condvar,
    applied: Mutex<u64>,
    applied_cv: Condvar,
    edge: OnceLock<Arc<caddy::Edge>>,
    ctl: OnceLock<Controller>,
}

/// How long org settings are trusted before they are read again.
const ORG_TTL: Duration = Duration::from_secs(15);
/// How often the applier wakes without a change (org settings, tunnel sync).
const TICK: Duration = Duration::from_secs(15);
/// How often an API-managed tunnel is re-synced when nothing changed.
const RESYNC: u64 = 600;

impl Manager {
    /// Create the manager. Caddy starts with [`Manager::start`], once the
    /// controller exists.
    pub fn new(
        cfg: IngressConfig,
        client: Client,
        secrets: Arc<Secrets>,
        state_dir: &std::path::Path,
    ) -> Result<Arc<Manager>> {
        let dir = state_dir.join("ingress");
        std::fs::create_dir_all(&dir)?;
        let claims = load_claims(&dir.join("claims.json"));
        Ok(Arc::new(Manager {
            cfg,
            client,
            secrets,
            dir,
            state: Mutex::new(State {
                claims,
                ..Default::default()
            }),
            wake: Condvar::new(),
            applied: Mutex::new(0),
            applied_cv: Condvar::new(),
            edge: OnceLock::new(),
            ctl: OnceLock::new(),
        }))
    }

    /// Caddy's admin socket, in a directory only we can open. A unix socket
    /// path holds at most 107 bytes: a long state directory moves it under
    /// the runtime directory, named by a hash of the state directory.
    fn admin_socket(&self) -> PathBuf {
        let p = self.dir.join("run").join("admin.sock");
        if p.as_os_str().len() <= 100 {
            return p;
        }
        let mut h: u32 = 0x811c9dc5;
        for b in self.dir.as_os_str().as_encoded_bytes() {
            h ^= *b as u32;
            h = h.wrapping_mul(0x01000193);
        }
        let base = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(format!("/tmp/isb-{}", rustix::process::getuid().as_raw()))
            });
        base.join("isb")
            .join(format!("caddy-{h:08x}"))
            .join("admin.sock")
    }

    fn storage(&self) -> PathBuf {
        self.dir.join("caddy")
    }

    fn params(&self) -> caddy::Params {
        caddy::Params {
            admin_socket: self.admin_socket(),
            storage: self.storage(),
            http: self.cfg.http,
            https: self.cfg.https,
            ca: self.cfg.ca.clone(),
            email: self.cfg.email.clone(),
        }
    }

    /// Get Caddy (downloading the pinned release if needed), start it, and
    /// start the applier.
    pub fn start(self: &Arc<Self>, ctl: Controller) -> Result<()> {
        let _ = self.ctl.set(ctl);
        let bin = caddy::ensure_binary(&self.dir.join("bin"), self.cfg.caddy_bin.as_deref())?;
        let initial = caddy::render(&self.params(), &[], &[]);
        let weak = Arc::downgrade(self);
        let on_log: caddy::OnLog = Arc::new(move |ev| {
            if let Some(m) = weak.upgrade() {
                m.cert_event(ev);
            }
        });
        let edge = caddy::Edge::start(bin, &self.dir, self.admin_socket(), initial, on_log)?;
        let _ = self.edge.set(edge);
        let m = self.clone();
        std::thread::Builder::new()
            .name("isb-ingress".into())
            .spawn(move || m.applier())?;
        self.bump();
        Ok(())
    }

    /// The address generated (`host: auto`) names point at.
    pub fn public_ip(&self) -> Option<IpAddr> {
        self.cfg.public_ip
    }

    pub fn shutdown(&self) {
        if let Some(e) = self.edge.get() {
            e.shutdown();
        }
    }

    fn bump(&self) {
        let mut st = self.state.lock().unwrap();
        st.generation += 1;
        self.wake.notify_all();
    }

    fn cert_event(&self, ev: caddy::LogEvent) {
        let issuer_key = self.cfg.ca.issuer_key();
        let (host, cs, kind, level, msg) = match ev {
            caddy::LogEvent::Obtained { host, issuer } => {
                if !issuer.is_empty() && issuer != issuer_key {
                    return;
                }
                let msg = format!("certificate issued for {host} ({issuer})");
                (
                    host,
                    CertState {
                        state: "issued".into(),
                        error: None,
                        issuer,
                        at: crate::stack::now_secs(),
                    },
                    "cert.issued",
                    "info",
                    msg,
                )
            }
            caddy::LogEvent::Failed { host, error } => {
                let msg = format!("certificate for {host} failed: {error}");
                (
                    host,
                    CertState {
                        state: "failed".into(),
                        error: Some(error),
                        issuer: issuer_key,
                        at: crate::stack::now_secs(),
                    },
                    "cert.failed",
                    "warn",
                    msg,
                )
            }
        };
        let owners: Vec<(String, String)> = {
            let mut st = self.state.lock().unwrap();
            let same = st
                .certs
                .get(&host)
                .is_some_and(|c| c.state == cs.state && c.error == cs.error);
            st.certs.insert(host.clone(), cs);
            if same {
                return;
            }
            st.served
                .iter()
                .filter(|s| s.route.host == host)
                .map(|s| (s.route.qualified_stack(), s.route.service.clone()))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect()
        };
        if let Some(ctl) = self.ctl.get() {
            for (q, svc) in owners {
                ctl.event(kind, level, &q, &svc, msg.clone());
            }
        }
    }

    /// Check a stack about to be deployed: its domains must be allowed for
    /// its org and free of other orgs' (and other services') claims.
    pub fn check(&self, def: &StackDef) -> Result<()> {
        let mut routes = Vec::new();
        for (svc, spec) in &def.file.services {
            if spec.domains.is_empty() {
                continue;
            }
            routes.extend(domain::routes_for(
                &def.org,
                &def.name,
                svc,
                &spec.domains,
                self.cfg.public_ip,
            )?);
        }
        if routes.is_empty() {
            return Ok(());
        }
        let oi = self.org_settings(&def.org, true)?;
        for r in &routes {
            if !r.auto && !domain::allowed(&r.host, &oi.domains) {
                return Err(Error::invalid(not_allowed(&r.host, &def.org, &oi.domains)));
            }
        }
        let (defs, claims) = {
            let st = self.state.lock().unwrap();
            (st.defs.clone(), st.claims.clone())
        };
        let q = def.qualified();
        let mut all: Vec<Route> = routes.clone();
        for d in defs.iter().filter(|d| d.qualified() != q) {
            for (svc, spec) in &d.file.services {
                if let Ok(r) =
                    domain::routes_for(&d.org, &d.name, svc, &spec.domains, self.cfg.public_ip)
                {
                    all.extend(r);
                }
            }
        }
        let res = domain::resolve(&all, &claims, crate::stack::now_secs());
        let mine: Vec<String> = res
            .conflicts
            .iter()
            .filter(|c| c.route.org == def.org && c.route.stack == def.name)
            .map(|c| format!("service {}: {}", c.route.service, c.reason))
            .collect();
        if !mine.is_empty() {
            return Err(Error::invalid(format!(
                "domain conflict: {}",
                mine.join("; ")
            )));
        }
        Ok(())
    }

    /// An org's settings, cached for [`ORG_TTL`] unless `fresh`.
    fn org_settings(&self, org: &OrgId, fresh: bool) -> Result<OrgIngress> {
        if !fresh {
            if let Some((at, oi)) = self.state.lock().unwrap().orgs.get(org) {
                if at.elapsed() < ORG_TTL {
                    return Ok(oi.clone());
                }
            }
        }
        let info = crate::org::get(&self.client, org)?;
        let oi = OrgIngress {
            domains: info.domains,
            tunnel: info.ingress == crate::org::INGRESS_CLOUDFLARE_TUNNEL,
            account: info.cloudflare_account,
            zone: info.cloudflare_zone,
            subnet: info.subnet,
        };
        self.state
            .lock()
            .unwrap()
            .orgs
            .insert(org.clone(), (Instant::now(), oi.clone()));
        Ok(oi)
    }

    fn applier(self: Arc<Self>) {
        let mut done = 0u64;
        let mut last_cfg: Option<Value> = None;
        loop {
            let target_gen = {
                let st = self.state.lock().unwrap();
                let (st, _) = self
                    .wake
                    .wait_timeout_while(st, TICK, |s| s.generation == done)
                    .unwrap();
                st.generation
            };
            if let Err(e) = self.apply_once(&mut last_cfg) {
                let msg = e.to_string();
                let mut st = self.state.lock().unwrap();
                if st.last_error.as_deref() != Some(&msg) {
                    eprintln!("isb serve: ingress: {msg}");
                }
                st.last_error = Some(msg);
            } else {
                self.state.lock().unwrap().last_error = None;
            }
            done = target_gen;
            *self.applied.lock().unwrap() = target_gen;
            self.applied_cv.notify_all();
        }
    }

    /// Compute the routes and load them into Caddy when they changed.
    #[expect(
        clippy::too_many_lines,
        clippy::excessive_nesting,
        reason = "predates the lint ratchet; split it when next changed"
    )]
    fn apply_once(&self, last_cfg: &mut Option<Value>) -> Result<()> {
        let (defs, mut rotation, claims, extras) = {
            let st = self.state.lock().unwrap();
            let s = &st;
            (
                s.defs.clone(),
                s.rotation.clone(),
                s.claims.clone(),
                s.extras.clone(),
            )
        };
        let mut orgs: BTreeMap<OrgId, OrgIngress> = BTreeMap::new();
        let mut org_errors: BTreeMap<OrgId, String> = BTreeMap::new();
        let mut want: BTreeSet<OrgId> = defs
            .iter()
            .filter(|d| d.file.services.values().any(|s| !s.domains.is_empty()))
            .map(|d| d.org.clone())
            .collect();
        // Tunnel orgs we serve now, whose settings may have changed.
        want.extend(self.state.lock().unwrap().tunnels.keys().cloned());
        want.extend(extras.iter().map(|e| e.route.org.clone()));
        for o in want {
            match self.org_settings(&o, false) {
                Ok(oi) => {
                    orgs.insert(o, oi);
                }
                Err(e) => {
                    org_errors.insert(o, e.to_string());
                }
            }
        }

        let mut routes = Vec::new();
        let mut refused: Refused = BTreeMap::new();
        for d in &defs {
            for (svc, spec) in &d.file.services {
                if spec.domains.is_empty() {
                    continue;
                }
                let key = (d.qualified(), svc.clone());
                let refuse =
                    |refused: &mut BTreeMap<_, Vec<_>>, host: &str, path: &str, why: String| {
                        refused.entry(key.clone()).or_insert_with(Vec::new).push((
                            host.to_string(),
                            path.to_string(),
                            why,
                        ));
                    };
                if let Some(e) = org_errors.get(&d.org) {
                    refuse(&mut refused, "", "", format!("org settings: {e}"));
                    continue;
                }
                let oi = orgs.get(&d.org).cloned().unwrap_or_default();
                match domain::routes_for(&d.org, &d.name, svc, &spec.domains, self.cfg.public_ip) {
                    Ok(rs) => {
                        for r in rs {
                            if !r.auto && !domain::allowed(&r.host, &oi.domains) {
                                let why = not_allowed(&r.host, &d.org, &oi.domains);
                                refuse(&mut refused, &r.host, &r.path, why);
                            } else {
                                routes.push(r);
                            }
                        }
                    }
                    Err(e) => refuse(&mut refused, "", "", e.to_string()),
                }
            }
        }
        let out = (&mut routes, &mut rotation, &mut refused);
        Self::merge_extras(&extras, &orgs, &org_errors, out);
        let res = domain::resolve(&routes, &claims, crate::stack::now_secs());

        // Tunnel listeners, one per tunnel org with an address.
        let mut tunnels = Vec::new();
        for (o, oi) in &orgs {
            if !oi.tunnel {
                continue;
            }
            if let Some((gw, subnet)) = oi.subnet.as_deref().and_then(gateway) {
                tunnels.push(TunnelListener {
                    org: o.clone(),
                    listen: SocketAddr::new(gw, self.cfg.tunnel_port),
                    subnet,
                });
            }
        }
        let served: Vec<Served> = res
            .accepted
            .iter()
            .map(|r| {
                let ips = rotation
                    .get(&(r.qualified_stack(), r.service.clone()))
                    .cloned()
                    .unwrap_or_default();
                let upstreams = match r.port {
                    Some(p) => ips.iter().map(|ip| SocketAddr::new(*ip, p)).collect(),
                    None => Vec::new(),
                };
                let via = if orgs.get(&r.org).is_some_and(|o| o.tunnel) {
                    Via::Tunnel(r.org.clone())
                } else {
                    Via::Public
                };
                Served {
                    route: r.clone(),
                    upstreams,
                    via,
                }
            })
            .collect();

        // New conflicts become events, once each.
        let mut new_events = Vec::new();
        {
            let mut st = self.state.lock().unwrap();
            let mut now_reported = BTreeSet::new();
            for c in &res.conflicts {
                let k = format!(
                    "{}|{}|{}|{}",
                    c.route.qualified_stack(),
                    c.route.service,
                    c.route.host,
                    c.route.path
                );
                if !st.reported.contains(&k) {
                    new_events.push((
                        c.route.qualified_stack(),
                        c.route.service.clone(),
                        format!("domain conflict: {}", c.reason),
                    ));
                }
                now_reported.insert(k);
            }
            for (k, list) in &refused {
                for (h, p, why) in list {
                    let key = format!("{}|{}|{h}|{p}|refused", k.0, k.1);
                    if !st.reported.contains(&key) {
                        new_events.push((
                            k.0.clone(),
                            k.1.clone(),
                            format!("domain refused: {why}"),
                        ));
                    }
                    now_reported.insert(key);
                }
            }
            st.reported = now_reported;
            if st.claims != res.claims {
                st.claims = res.claims.clone();
                save_claims(&self.dir.join("claims.json"), &st.claims);
            }
            st.served = served.clone();
            st.conflicts = res.conflicts.clone();
            st.refused = refused;
        }
        if let Some(ctl) = self.ctl.get() {
            for (q, svc, msg) in new_events {
                ctl.service_event("warn", &q, &svc, msg);
            }
        }

        let cfg = caddy::render(&self.params(), &served, &tunnels);
        let mut result = Ok(());
        if last_cfg.as_ref() != Some(&cfg) {
            if let Some(edge) = self.edge.get() {
                match edge.load(cfg.clone()) {
                    Ok(()) => *last_cfg = Some(cfg),
                    Err(e) => result = Err(e),
                }
            }
        }
        self.tunnels(&orgs, &served, &tunnels);
        result
    }
}

fn not_allowed(host: &str, org: &OrgId, list: &[String]) -> String {
    if list.is_empty() {
        format!(
            "{host}: org {org} may not serve wildcard hosts (no wildcard in its --allow-domain list)"
        )
    } else {
        format!(
            "{host} is outside org {org}'s domains ({})",
            list.join(", ")
        )
    }
}

/// The bridge's own address and its subnet: `10.64.3.1/24` ->
/// (10.64.3.1, 10.64.3.0/24).
fn gateway(cidr: &str) -> Option<(IpAddr, String)> {
    let (ip, len) = cidr.split_once('/')?;
    let ip: std::net::Ipv4Addr = ip.parse().ok()?;
    let len: u32 = len.parse().ok()?;
    if len > 32 {
        return None;
    }
    let mask = if len == 0 { 0 } else { u32::MAX << (32 - len) };
    let net = std::net::Ipv4Addr::from(u32::from(ip) & mask);
    Some((IpAddr::V4(ip), format!("{net}/{len}")))
}

fn load_claims(p: &std::path::Path) -> Vec<Claim> {
    std::fs::read(p)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_claims(p: &std::path::Path, claims: &[Claim]) {
    let tmp = p.with_extension("json.tmp");
    let ok = serde_json::to_vec_pretty(claims)
        .ok()
        .and_then(|b| std::fs::write(&tmp, b).ok())
        .and_then(|_| std::fs::rename(&tmp, p).ok());
    if ok.is_none() {
        eprintln!("isb serve: ingress: cannot save {}", p.display());
    }
}

impl Observer for Manager {
    fn rotation(&self, stack: &str, service: &str, ips: &[IpAddr]) {
        let mut st = self.state.lock().unwrap();
        let k = (stack.to_string(), service.to_string());
        if st.rotation.get(&k).map(|v| v.as_slice()) == Some(ips) {
            return;
        }
        if ips.is_empty() {
            st.rotation.remove(&k);
        } else {
            st.rotation.insert(k, ips.to_vec());
        }
        // Only services with domains change the config.
        let routed = st
            .served
            .iter()
            .any(|s| s.route.service == service && s.route.qualified_stack() == stack)
            || st.defs.iter().any(|d| {
                d.qualified() == stack
                    && d.file
                        .services
                        .get(service)
                        .is_some_and(|s| !s.domains.is_empty())
            });
        if routed {
            st.generation += 1;
            self.wake.notify_all();
        }
    }

    fn drain(&self, stack: &str, service: &str, ip: IpAddr, timeout: Duration) {
        let started = Instant::now();
        let target_gen = {
            let st = self.state.lock().unwrap();
            let routed = st
                .served
                .iter()
                .any(|s| s.route.service == service && s.route.qualified_stack() == stack);
            if !routed {
                return;
            }
            st.generation
        };
        // The config without the replica is loaded (or Caddy is down).
        {
            let g = self.applied.lock().unwrap();
            let _ = self
                .applied_cv
                .wait_timeout_while(g, timeout, |a| *a < target_gen)
                .unwrap();
        }
        let Some(edge) = self.edge.get() else { return };
        let prefix = format!("{ip}:");
        while started.elapsed() < timeout {
            match edge.upstreams() {
                Ok(ups) => {
                    if !ups.iter().any(|(a, n)| a.starts_with(&prefix) && *n > 0) {
                        return;
                    }
                }
                Err(_) => return,
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn stacks_changed(&self, defs: Vec<Arc<StackDef>>) {
        let mut st = self.state.lock().unwrap();
        st.defs = defs;
        st.generation += 1;
        self.wake.notify_all();
    }

    fn domains(&self, stack: &str, service: &str) -> Vec<DomainStatus> {
        let st = self.state.lock().unwrap();
        let mut out: Vec<DomainStatus> = st
            .served
            .iter()
            .filter(|s| s.route.service == service && s.route.qualified_stack() == stack)
            .map(|s| self.domain_status_of(&st, s))
            .collect();
        for c in st
            .conflicts
            .iter()
            .filter(|c| c.route.service == service && c.route.qualified_stack() == stack)
        {
            out.push(DomainStatus {
                host: c.route.host.clone(),
                path: c.route.path.clone(),
                https: c.route.https,
                state: "conflict".into(),
                cert: "none".into(),
                message: Some(c.reason.clone()),
                ..Default::default()
            });
        }
        if let Some(list) = st.refused.get(&(stack.to_string(), service.to_string())) {
            for (h, p, why) in list {
                out.push(DomainStatus {
                    host: h.clone(),
                    path: p.clone(),
                    state: "refused".into(),
                    cert: "none".into(),
                    message: Some(why.clone()),
                    ..Default::default()
                });
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateways() {
        assert_eq!(
            gateway("10.64.3.1/24"),
            Some(("10.64.3.1".parse().unwrap(), "10.64.3.0/24".into()))
        );
        assert_eq!(gateway("nope"), None);
    }

    #[test]
    fn claims_round_trip() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("claims.json");
        assert!(load_claims(&p).is_empty());
        let c = vec![Claim {
            host: "a.example.com".into(),
            path: "/".into(),
            org: "acme".into(),
            stack: "s".into(),
            service: "w".into(),
            since: 5,
        }];
        save_claims(&p, &c);
        assert_eq!(load_claims(&p), c);
    }
}
