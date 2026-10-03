//! Remote servers (P5.1, P5.2): a control plane places orgs on other hosts,
//! each running incus and `isb serve --agent`, and forwards their calls over
//! mutual TLS. Federation, not incus clustering: every server is a whole
//! isb (controller, ingress, registry, builds, secrets) for the orgs on it.
//! See docs/guides/servers.md.

pub mod bootstrap;
pub mod client;
pub mod health;
pub mod merge;
pub mod pki;
pub mod provision;
pub mod store;
pub mod upgrade;
pub mod vm;
pub mod wire;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::org::OrgId;
use crate::stack::{Controller, now_secs};
pub use client::AgentClient;
pub use store::ServerRecord;
use wire::Assertion;

/// How long a forwarded tool call may take (deploys with `wait`, exec).
pub const CALL_TIMEOUT: Duration = Duration::from_secs(20 * 60);

/// The control plane's servers: their records, placement, CA, health, and
/// the threads that watch them.
pub struct Servers {
    dir: PathBuf,
    ca: pki::Ca,
    tls: Arc<rustls::ClientConfig>,
    store: Mutex<store::Store>,
    health: Mutex<BTreeMap<String, health::Health>>,
    mirrored: Mutex<BTreeSet<String>>,
    /// Servers being added, and recently added or failed.
    pub runs: provision::Runs,
    /// Serializes record and placement changes (held briefly; a bootstrap
    /// runs outside it, one per name through `runs`).
    admin: Mutex<()>,
    /// Servers being upgraded.
    upgrading: Mutex<BTreeSet<String>>,
    stop: Arc<AtomicBool>,
}

impl std::fmt::Debug for Servers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Servers").field("dir", &self.dir).finish()
    }
}

impl Servers {
    /// `<state>/servers/`: the CA under `pki/`, the records, the placement.
    pub fn open(state_dir: &Path) -> Result<Arc<Servers>> {
        let dir = state_dir.join("servers");
        std::fs::create_dir_all(&dir)?;
        let ca = pki::Ca::open(&dir.join("pki"))?;
        let tls = ca.client_config()?;
        let store = store::Store::open(&dir)?;
        Ok(Arc::new(Servers {
            dir,
            ca,
            tls,
            store: Mutex::new(store),
            health: Mutex::new(BTreeMap::new()),
            mirrored: Mutex::new(BTreeSet::new()),
            runs: provision::Runs::default(),
            admin: Mutex::new(()),
            upgrading: Mutex::new(BTreeSet::new()),
            stop: Arc::new(AtomicBool::new(false)),
        }))
    }

    pub fn record(&self, name: &str) -> Result<ServerRecord> {
        self.store
            .lock()
            .unwrap()
            .servers
            .get(name)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("server {name}")))
    }

    pub fn records(&self) -> Vec<ServerRecord> {
        self.store
            .lock()
            .unwrap()
            .servers
            .values()
            .cloned()
            .collect()
    }

    pub fn client(&self, name: &str) -> Result<AgentClient> {
        let r = self.record(name)?;
        Ok(AgentClient::new(
            &r.name,
            &r.address,
            r.port,
            self.tls.clone(),
        ))
    }

    /// The server `org` is placed on; `None` is this daemon.
    pub fn placement(&self, org: &OrgId) -> Option<String> {
        self.store.lock().unwrap().placement.get(org).cloned()
    }

    pub fn placements(&self) -> BTreeMap<OrgId, String> {
        self.store.lock().unwrap().placement.clone()
    }

    pub fn orgs_on(&self, server: &str) -> Vec<OrgId> {
        self.store.lock().unwrap().orgs_on(server)
    }

    pub fn health(&self, name: &str) -> health::Health {
        self.health
            .lock()
            .unwrap()
            .get(name)
            .cloned()
            .unwrap_or_default()
    }

    /// A server as `server_list` and `server_show` answer it.
    pub fn view(&self, r: &ServerRecord) -> Value {
        let mut v = serde_json::to_value(r).unwrap_or_default();
        let h = self.health(&r.name);
        v["kind"] = json!(if r.vm.is_some() { "vm" } else { "ssh" });
        v["orgs"] = json!(self.orgs_on(&r.name));
        v["version"] = version_view(&r.name, &h.heartbeat, r.vm.is_some());
        v["health"] = serde_json::to_value(h).unwrap_or_default();
        v
    }

    /// Refuse to forward to `server` when its agent speaks a protocol this
    /// control plane cannot (`need`: the oldest that will do).
    pub fn check_protocol(&self, server: &str, need: u64) -> Result<()> {
        upgrade::compatible(server, &self.health(server).heartbeat, need)
    }

    /// Place `org` on `server` (the agent is told first, so it accepts
    /// calls for it), or take it off (`None`).
    pub fn place(&self, org: &OrgId, server: Option<&str>) -> Result<()> {
        let _g = self.admin.lock().unwrap();
        match server {
            Some(s) => {
                self.client(s)?.internal(
                    "POST",
                    "/internal/v1/orgs",
                    Some(&json!({"org": org, "placed": true})),
                    health::TIMEOUT,
                )?;
                let mut st = self.store.lock().unwrap();
                st.placement.insert(org.clone(), s.to_string());
                st.save()
            }
            None => {
                let prev = self.store.lock().unwrap().placement.get(org).cloned();
                if let Some(s) = prev {
                    // Best effort: the agent may be gone for good.
                    if let Ok(c) = self.client(&s) {
                        if let Err(e) = c.internal(
                            "POST",
                            "/internal/v1/orgs",
                            Some(&json!({"org": org, "placed": false})),
                            health::TIMEOUT,
                        ) {
                            eprintln!("isb serve: server {s}: unplacing {org}: {e}");
                        }
                    }
                }
                let mut st = self.store.lock().unwrap();
                st.placement.remove(org);
                st.save()
            }
        }
    }

    /// Forward a tool call for `org` (or an unscoped cross-org read) to
    /// `server` as `caller`.
    pub fn call(
        &self,
        server: &str,
        tool: &str,
        args: &Value,
        caller: &crate::server::Caller,
        org: Option<&OrgId>,
        request_id: Option<&str>,
    ) -> Result<Value> {
        let who = Assertion::for_caller(caller)
            .ok_or_else(|| Error::Forbidden(format!("{caller} cannot act on server {server}")))?;
        self.check_protocol(server, upgrade::MIN_PROTOCOL)?;
        self.client(server)?
            .call(tool, args, &who, org, request_id, CALL_TIMEOUT)
    }

    /// What `add` would refuse before it starts: a bad name, target or
    /// address, or a server of that name already.
    pub fn check_add(&self, o: &bootstrap::AddOptions) -> Result<String> {
        bootstrap::validate_name(&o.name)?;
        let (_, host) = bootstrap::ssh_host(&o.ssh)?;
        for c in &o.allow_from {
            bootstrap::check_cidr(c)?;
        }
        let address = o.address.clone().unwrap_or_else(|| host.to_string());
        bootstrap::check_address(&address)?;
        if self.store.lock().unwrap().servers.contains_key(&o.name) {
            return Err(Error::AlreadyExists(format!("server {}", o.name)));
        }
        Ok(address)
    }

    /// Bootstrap a new server over SSH and record it, reporting to `p`.
    pub fn add(
        self: &Arc<Self>,
        o: &bootstrap::AddOptions,
        ctl: Option<&Controller>,
        p: &provision::Provision,
    ) -> Result<ServerRecord> {
        let address = self.check_add(o)?;
        let leaf = self.ca.issue_server(&o.name, &address)?;
        let known = self.dir.join("known_hosts");
        let ssh = bootstrap::Ssh::new(&o.ssh, o.ssh_port, &o.key, &known);
        let scratch = self.dir.join(format!("tmp-{}", o.name));
        std::fs::create_dir_all(&scratch)?;
        let r = bootstrap::run(o, &ssh, &self.ca.cert_pem, &leaf, &scratch, p);
        let _ = std::fs::remove_dir_all(&scratch);
        let incus = r?;
        self.register(
            ServerRecord {
                name: o.name.clone(),
                address,
                port: o.agent_port,
                ssh: o.ssh.clone(),
                ssh_port: o.ssh_port,
                added_at: now_secs(),
                fingerprint: leaf.fingerprint()?,
                cert_not_after: Some(now_secs() + pki::LEAF_DAYS as u64 * 86400),
                isb_version: String::new(),
                allow_from: o.allow_from.clone(),
                vm: None,
            },
            &incus,
            ctl,
            p,
        )
    }

    /// Make a dedicated VM for `org` on this host and record it as server
    /// `vm-<org>` (docs/guides/servers.md#dedicated-vms). Idempotent: a VM or a
    /// record left by an earlier attempt is reused.
    pub fn add_vm(
        self: &Arc<Self>,
        client: &crate::Client,
        org: &OrgId,
        size: &vm::VmSize,
        ctl: Option<&Controller>,
        p: &provision::Provision,
    ) -> Result<ServerRecord> {
        let name = vm::server_name(org);
        if let Ok(r) = self.record(&name) {
            match &r.vm {
                Some(v) if &v.org == org => {
                    let c = self.client(&name)?;
                    if c.internal("GET", "/internal/v1/heartbeat", None, health::TIMEOUT)
                        .is_ok()
                    {
                        p.log(&format!("server {name} is up already"));
                        return Ok(r);
                    }
                    p.log(&format!(
                        "server {name} is recorded but does not answer: bootstrapping it again"
                    ));
                }
                _ => {
                    return Err(Error::AlreadyExists(format!(
                        "server {name} (not org {org}'s dedicated VM)"
                    )));
                }
            }
        }
        let booted = vm::boot(client, org, size, p)?;
        let leaf = self.ca.issue_server(&name, &booted.address)?;
        let binary = bootstrap::own_binary(std::env::consts::ARCH)?;
        let sha = bootstrap::sha256_hex(&binary);
        let upload = "/root/isb-agent.upload";
        let allow = vec![booted.host_address.clone()];
        let script = bootstrap::render_script(
            upload,
            &sha,
            &self.ca.cert_pem,
            &leaf,
            bootstrap::DEFAULT_AGENT_PORT,
            &allow,
            None,
            false,
        );
        let incus = vm::install(client, org, &binary, &script, upload, p)?;
        self.register(
            ServerRecord {
                name: name.clone(),
                address: booted.address,
                port: bootstrap::DEFAULT_AGENT_PORT,
                ssh: String::new(),
                ssh_port: 0,
                added_at: now_secs(),
                fingerprint: leaf.fingerprint()?,
                cert_not_after: Some(now_secs() + pki::LEAF_DAYS as u64 * 86400),
                isb_version: String::new(),
                allow_from: allow,
                vm: Some(store::VmRecord {
                    org: org.clone(),
                    project: vm::PROJECT.to_string(),
                    instance: name,
                    cpus: size.cpus,
                    memory: size.memory.clone(),
                    disk: size.disk.clone(),
                }),
            },
            &incus,
            ctl,
            p,
        )
    }

    /// Wait for a freshly bootstrapped agent, check it presents the
    /// certificate just issued, and record it.
    fn register(
        self: &Arc<Self>,
        mut rec: ServerRecord,
        incus: &str,
        ctl: Option<&Controller>,
        p: &provision::Provision,
    ) -> Result<ServerRecord> {
        p.step("agent");
        p.log(&format!(
            "waiting for the agent on {}:{} ({incus})",
            rec.address, rec.port
        ));
        let c = AgentClient::new(&rec.name, &rec.address, rec.port, self.tls.clone());
        let hb = wait_heartbeat(&c, Duration::from_secs(120))?;
        if c.peer_fingerprint()? != rec.fingerprint {
            return Err(Error::invalid(format!(
                "server {}: the agent answered with a certificate other than the one just issued",
                rec.name
            )));
        }
        rec.isb_version = hb["isb"].as_str().unwrap_or("").to_string();
        {
            let _g = self.admin.lock().unwrap();
            let mut st = self.store.lock().unwrap();
            if let Some(old) = st.servers.get(&rec.name) {
                // Bootstrapped again (a dedicated VM that stopped answering).
                rec.added_at = old.added_at;
            }
            st.servers.insert(rec.name.clone(), rec.clone());
            st.save()?;
        }
        self.health
            .lock()
            .unwrap()
            .entry(rec.name.clone())
            .or_default()
            .observe(Ok(hb), now_secs());
        if let Some(ctl) = ctl {
            self.mirror(&rec.name, ctl.clone());
        }
        p.log(&format!("server {} is up", rec.name));
        Ok(rec)
    }

    /// Forget a server. Refused while orgs are placed on it; the agent
    /// itself keeps running until it is stopped on the box.
    pub fn remove(&self, name: &str) -> Result<ServerRecord> {
        let _g = self.admin.lock().unwrap();
        let mut st = self.store.lock().unwrap();
        let orgs = st.orgs_on(name);
        if !orgs.is_empty() {
            return Err(Error::invalid(format!(
                "server {name} holds orgs ({}); delete them first",
                orgs.iter()
                    .map(|o| o.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        let rec = st
            .servers
            .remove(name)
            .ok_or_else(|| Error::NotFound(format!("server {name}")))?;
        st.save()?;
        self.health.lock().unwrap().remove(name);
        Ok(rec)
    }

    /// Issue the agent a new certificate over the current mTLS connection
    /// and check it answers with it.
    pub fn rotate_cert(&self, name: &str) -> Result<ServerRecord> {
        let _g = self.admin.lock().unwrap();
        let rec = self.record(name)?;
        let leaf = self.ca.issue_server(name, &rec.address)?;
        let c = self.client(name)?;
        c.internal(
            "POST",
            "/internal/v1/cert",
            Some(&json!({"cert": leaf.cert, "key": leaf.key})),
            health::TIMEOUT,
        )?;
        let fp = leaf.fingerprint()?;
        let got = c.peer_fingerprint()?;
        if got != fp {
            return Err(Error::invalid(format!(
                "server {name}: still presents {got} after rotation"
            )));
        }
        let mut st = self.store.lock().unwrap();
        let r = st
            .servers
            .get_mut(name)
            .ok_or_else(|| Error::NotFound(format!("server {name}")))?;
        r.fingerprint = fp;
        r.cert_not_after = Some(now_secs() + pki::LEAF_DAYS as u64 * 86400);
        let r = r.clone();
        st.save()?;
        Ok(r)
    }

    /// Heartbeats for every server, and a mirror of each one's events into
    /// `ctl`'s feed.
    pub fn start(self: &Arc<Self>, ctl: Controller) {
        for r in self.records() {
            self.mirror(&r.name, ctl.clone());
        }
        let me = self.clone();
        let _ = std::thread::Builder::new()
            .name("isb-servers".into())
            .spawn(move || {
                while !me.stop.load(Ordering::SeqCst) {
                    for r in me.records() {
                        me.beat(&r.name, &ctl);
                    }
                    let mut slept = Duration::ZERO;
                    while slept < health::INTERVAL && !me.stop.load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_millis(250));
                        slept += Duration::from_millis(250);
                    }
                }
            });
    }

    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    fn beat(&self, name: &str, ctl: &Controller) {
        let r = self
            .client(name)
            .and_then(|c| c.internal("GET", "/internal/v1/heartbeat", None, health::TIMEOUT))
            .map_err(|e| e.to_string());
        let t = self
            .health
            .lock()
            .unwrap()
            .entry(name.to_string())
            .or_default()
            .observe(r, now_secs());
        let Some(t) = t else { return };
        let h = self.health(name);
        let (kind, level, msg) = match t {
            health::Transition::Unreachable => (
                "server.unreachable",
                "error",
                format!(
                    "server {name} is unreachable: {}",
                    h.last_error.as_deref().unwrap_or("no answer")
                ),
            ),
            health::Transition::Recovered => (
                "server.recovered",
                "info",
                format!("server {name} answers again"),
            ),
        };
        let mut stacks: Vec<String> = self
            .orgs_on(name)
            .iter()
            .map(|o| format!("{o}/@servers"))
            .collect();
        stacks.push("system/@servers".into());
        for s in stacks {
            ctl.relay(Some(kind), level, &s, name, None, msg.clone());
        }
        eprintln!("isb serve: {msg}");
    }

    /// Follow `name`'s event feed into `ctl`, from where it is now. Only
    /// events of orgs placed on it are taken, so a server cannot speak for
    /// another's orgs. Ends when the server is removed.
    fn mirror(self: &Arc<Self>, name: &str, ctl: Controller) {
        if !self.mirrored.lock().unwrap().insert(name.to_string()) {
            return;
        }
        let (me, name) = (self.clone(), name.to_string());
        let _ = std::thread::Builder::new()
            .name(format!("isb-mirror-{name}"))
            .spawn(move || {
                let who = Assertion::control_plane();
                let mut cursor: Option<u64> = None;
                while !me.stop.load(Ordering::SeqCst) {
                    let Ok(c) = me.client(&name) else { break };
                    let since = cursor.unwrap_or(u64::MAX);
                    let args = match cursor {
                        Some(s) => json!({"since": s, "limit": 500, "wait": 25}),
                        // First contact: just learn where the feed is.
                        None => json!({"since": since, "limit": 1, "wait": 0}),
                    };
                    match c.call("events", &args, &who, None, None, Duration::from_secs(40)) {
                        Ok(v) => {
                            let seq = v["seq"].as_u64().unwrap_or(0);
                            let Some(from) = cursor else {
                                cursor = Some(seq);
                                continue;
                            };
                            if seq < from {
                                // The agent restarted: its feed starts over.
                                cursor = Some(0);
                                continue;
                            }
                            let placed = me.orgs_on(&name);
                            for e in v["events"].as_array().into_iter().flatten() {
                                relay(&ctl, e, &placed);
                            }
                            cursor = Some(seq);
                        }
                        Err(_) => std::thread::sleep(Duration::from_secs(5)),
                    }
                }
                me.mirrored.lock().unwrap().remove(&name);
            });
    }
}

/// What a server runs next to what this control plane runs: the version,
/// the build (a hash of the binary, so two builds of one version differ),
/// and whether they speak the same protocol.
fn version_view(name: &str, hb: &Value, vm: bool) -> Value {
    let known = !hb.is_null();
    let build = hb["build"].as_str().unwrap_or("");
    json!({
        "isb": hb["isb"],
        "build": hb["build"],
        "protocol": known.then(|| upgrade::protocol_of(hb)),
        "control_plane": {
            "isb": env!("CARGO_PKG_VERSION"),
            "build": upgrade::build_id(),
            "protocol": upgrade::PROTOCOL,
        },
        // Unknown until it answers; a different build of the same version
        // is skew too.
        "skew": known && (hb["isb"].as_str() != Some(env!("CARGO_PKG_VERSION")) || build != upgrade::build_id()),
        "compatible": upgrade::compatible(name, hb, upgrade::MIN_PROTOCOL).is_ok(),
        "ssh": upgrade::compatible(name, hb, upgrade::SSH_PROTOCOL).is_ok(),
        // A dedicated VM is upgraded through incus, helper or not.
        "upgradable": vm || hb["upgrade"]["helper"] == true,
        "last_upgrade": hb["upgrade"]["last"],
    })
}

/// Re-emit one of a server's events if it belongs to an org placed there.
fn relay(ctl: &Controller, e: &Value, placed: &[OrgId]) {
    let stack = e["stack"].as_str().unwrap_or("");
    let org = stack.split_once('/').map(|(o, _)| o).unwrap_or("");
    if !placed.iter().any(|o| o.as_str() == org) {
        return;
    }
    ctl.relay(
        e["kind"].as_str(),
        e["level"].as_str().unwrap_or("info"),
        stack,
        e["service"].as_str().unwrap_or(""),
        e["instance"].as_str(),
        e["message"].as_str().unwrap_or("").to_string(),
    );
}

fn wait_heartbeat(c: &AgentClient, timeout: Duration) -> Result<Value> {
    let started = std::time::Instant::now();
    loop {
        match c.internal("GET", "/internal/v1/heartbeat", None, health::TIMEOUT) {
            Ok(v) => return Ok(v),
            Err(e) if started.elapsed() >= timeout => {
                return Err(Error::OperationFailed {
                    step: format!("wait for the agent on server {}", c.name),
                    message: e.to_string(),
                });
            }
            Err(_) => std::thread::sleep(Duration::from_secs(2)),
        }
    }
}
