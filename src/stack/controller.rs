//! The reconciler behind `isb serve`: one worker thread per service keeps its
//! replicas created, current, running, healthy and in the load balancer.
//!
//! A worker never holds a lock across incus calls, and a slow service (an
//! image pull, a long rollout) never delays another's health checks. Workers
//! are told about a new deployment through their shared slot and woken early;
//! a worker whose service went away deletes its own instances and exits, so
//! nothing ever has to join a worker from inside another.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

use super::{
    LABEL_REV, LABEL_SERVICE, LABEL_SLOT, LABEL_STACK, StackDef, Store, instance_name, new_id,
    now_secs, validate_stack_name,
};
use crate::balance::Balancer;
use crate::client::{Client, encode_query, encode_segment};
use crate::error::{Error, Result};
use crate::org::OrgId;
use crate::plan::{Desired, split_addr};
use crate::sandbox::{EnsureOptions, Sandbox};
use crate::spec::{
    DependCondition, FailureAction, HealthProbe, PortBind, RestartCondition, RestartMode,
    SandboxSpec, UpdateConfig, UpdateOrder,
};
use crate::supervise;

/// How long a replaced instance's connections may drain before it is stopped.
const DRAIN: Duration = Duration::from_secs(10);
/// How often an unhealthy app is restarted before its instance is replaced.
const RESTARTS_BEFORE_REPLACE: u32 = 3;

/// One replica, as `stack_status` reports it.
#[derive(Debug, Clone, Serialize)]
pub struct InstanceStatus {
    pub name: String,
    pub slot: u32,
    pub rev: String,
    /// incus status: Running, Stopped, ...
    pub status: String,
    /// `healthy`, `unhealthy`, `starting`, or `none` (no healthcheck: the app
    /// is judged by its process alone).
    pub health: String,
    pub ip: Option<String>,
    /// Receiving traffic from the balancer.
    pub in_rotation: bool,
    pub restarts: u32,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub last_probe: String,
    /// Percent of one core; none until two samples were taken.
    pub cpu_pct: Option<f32>,
    pub cpu_history: Vec<f32>,
    pub mem_bytes: Option<u64>,
}

/// A published port served by the balancer.
#[derive(Debug, Clone, Serialize)]
pub struct PortStatus {
    pub listen: String,
    pub target: u16,
    pub backends: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Connections accepted since the daemon started, and per second lately.
    pub accepted: u64,
    pub active: usize,
    pub rate_history: Vec<f32>,
}

/// One service of a stack, as `stack_status` reports it.
#[derive(Debug, Clone, Serialize, Default)]
pub struct ServiceStatus {
    pub service: String,
    pub image: String,
    pub rev: String,
    pub replicas: u32,
    pub running: u32,
    pub healthy: u32,
    /// `starting`, `converged`, `updating`, `paused`, `waiting`, `failing`.
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub instances: Vec<InstanceStatus>,
    pub ports: Vec<PortStatus>,
    /// The rollout in progress, slot by slot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rollout: Option<RolloutStatus>,
    /// Unix seconds of the last completed reconcile pass.
    pub checked_at: u64,
}

/// A rollout in progress.
#[derive(Debug, Clone, Serialize, Default)]
pub struct RolloutStatus {
    pub to_rev: String,
    /// `stop-first` or `start-first`.
    pub order: String,
    pub parallelism: usize,
    pub done: usize,
    pub total: usize,
    /// Unix seconds.
    pub started_at: u64,
    pub slots: Vec<SlotRollout>,
}

/// One slot's handover: the instance going away and the one replacing it.
#[derive(Debug, Clone, Serialize, Default)]
pub struct SlotRollout {
    pub slot: u32,
    pub old: Option<String>,
    pub old_rev: Option<String>,
    /// `serving`, `draining`, `retired`, or `none`.
    pub old_state: String,
    pub new: Option<String>,
    /// `waiting`, `creating`, `probing`, `monitoring`, `serving`, `failed`.
    pub new_state: String,
}

/// Something that happened, for the event feed.
#[derive(Debug, Clone, Serialize)]
pub struct Event {
    /// Increases by one per event; pass the last one seen as `since`.
    pub seq: u64,
    /// Unix milliseconds.
    pub at: u64,
    /// `info`, `warn` or `error`.
    pub level: String,
    pub stack: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub service: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    pub message: String,
}

/// Events kept for late readers.
const EVENTS_KEPT: usize = 1000;

/// The latest host and instance sample.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub host: crate::metrics::HostSample,
    /// Keyed by `<project>/<name>`: names are unique per project only.
    pub instances: BTreeMap<String, crate::metrics::InstanceSample>,
    /// Unix milliseconds; 0 before the first sample.
    pub at: u64,
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A stack, as `stack_status` and `stack_list` report it.
#[derive(Debug, Clone, Serialize)]
pub struct StackStatus {
    pub name: String,
    pub org: String,
    pub deployed_at: u64,
    pub deployed_by: String,
    pub has_previous: bool,
    /// True when every service has its replicas, all current and healthy.
    pub converged: bool,
    pub services: Vec<ServiceStatus>,
}

/// What a deploy is about to do, per service.
#[derive(Debug, Clone, Serialize)]
pub struct DeployChange {
    pub service: String,
    /// `create`, `update` (new revision: rolling replace), `scale`, `remove`,
    /// or `unchanged`.
    pub change: String,
    pub rev: String,
    pub replicas: u32,
}

/// The shared slot a worker reads its instructions from.
struct Slot {
    def: Arc<StackDef>,
    /// Set when the service left its stack: delete its instances and exit.
    remove: bool,
    /// Delete the service's named volumes too (with `remove`).
    remove_volumes: bool,
}

struct WorkerShared {
    slot: Mutex<Slot>,
    wake: Condvar,
    stop: AtomicBool,
}

struct Inner {
    client: Client,
    store: Store,
    balancer: Balancer,
    interval: Duration,
    /// Live workers, by (stack, service).
    workers: Mutex<BTreeMap<(String, String), Arc<WorkerShared>>>,
    /// Deployed stacks, by name.
    stacks: Mutex<BTreeMap<String, Arc<StackDef>>>,
    status: Mutex<BTreeMap<(String, String), ServiceStatus>>,
    events: Mutex<(u64, VecDeque<Event>)>,
    snapshot: Mutex<Snapshot>,
}

impl Inner {
    fn emit(
        &self,
        level: &str,
        stack: &str,
        service: &str,
        instance: Option<&str>,
        message: String,
    ) {
        let mut e = self.events.lock().unwrap();
        e.0 += 1;
        let ev = Event {
            seq: e.0,
            at: now_ms(),
            level: level.into(),
            stack: stack.into(),
            service: service.into(),
            instance: instance.map(String::from),
            message,
        };
        if e.1.len() == EVENTS_KEPT {
            e.1.pop_front();
        }
        e.1.push_back(ev);
    }
}

/// How often the metrics sampler runs.
const SAMPLE_EVERY: Duration = Duration::from_secs(2);

/// The daemon's stack controller.
#[derive(Clone)]
pub struct Controller {
    inner: Arc<Inner>,
}

impl Controller {
    /// Load every stored stack and start reconciling it.
    pub fn start(client: Client, store: Store, interval: Duration) -> Result<Controller> {
        let c = Controller {
            inner: Arc::new(Inner {
                client,
                store,
                balancer: Balancer::new(),
                interval,
                workers: Mutex::new(BTreeMap::new()),
                stacks: Mutex::new(BTreeMap::new()),
                status: Mutex::new(BTreeMap::new()),
                events: Mutex::new((0, VecDeque::new())),
                snapshot: Mutex::new(Snapshot::default()),
            }),
        };
        // A weak handle, so the sampler ends with the controller.
        let weak = Arc::downgrade(&c.inner);
        let _ = std::thread::Builder::new()
            .name("isb-metrics".into())
            .spawn(move || {
                let mut sampler = crate::metrics::Sampler::new();
                while let Some(inner) = weak.upgrade() {
                    match sampler.sample(&inner.client) {
                        Ok((host, insts)) => {
                            *inner.snapshot.lock().unwrap() = Snapshot {
                                host,
                                instances: insts
                                    .into_iter()
                                    .map(|i| (format!("{}/{}", i.project, i.name), i))
                                    .collect(),
                                at: now_ms(),
                            };
                        }
                        Err(e) => eprintln!("isb serve: metrics: {e}"),
                    }
                    drop(inner);
                    std::thread::sleep(SAMPLE_EVERY);
                }
            });
        let defs = c.inner.store.load_all()?;
        // Service names of stacks removed while no daemon ran.
        for def in &defs {
            if let Some(dir) = crate::discovery::org_dir(&def.org) {
                let keep: Vec<(String, String)> = defs
                    .iter()
                    .filter(|d| d.org == def.org)
                    .flat_map(|d| d.file.services.keys().map(|s| (d.name.clone(), s.clone())))
                    .collect();
                crate::discovery::prune(&dir, &keep);
            }
        }
        for def in defs {
            eprintln!("isb serve: resuming stack {}", def.name);
            c.apply(Arc::new(def));
        }
        Ok(c)
    }

    pub fn balancer(&self) -> &Balancer {
        &self.inner.balancer
    }

    /// Events after `since` (0: all kept), oldest first, at most `limit`.
    pub fn events(&self, since: u64, limit: usize) -> (u64, Vec<Event>) {
        let e = self.inner.events.lock().unwrap();
        let out: Vec<Event> = e.1.iter().filter(|x| x.seq > since).cloned().collect();
        let skip = out.len().saturating_sub(limit);
        (e.0, out.into_iter().skip(skip).collect())
    }

    /// Wait up to `timeout` for an event after `since`.
    pub fn wait_events(&self, since: u64, limit: usize, timeout: Duration) -> (u64, Vec<Event>) {
        let started = Instant::now();
        loop {
            let r = self.events(since, limit);
            if !r.1.is_empty() || started.elapsed() >= timeout {
                return r;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    /// The latest metrics sample.
    pub fn snapshot(&self) -> Snapshot {
        self.inner.snapshot.lock().unwrap().clone()
    }

    /// Record an event from outside a worker (a deploy, a removal).
    pub fn note(&self, level: &str, stack: &str, message: String) {
        eprintln!("isb serve: {stack}: {message}");
        self.inner.emit(level, stack, "", None, message);
    }

    /// What deploying `def` would change, without deploying it.
    pub fn plan(&self, def: &StackDef) -> Result<Vec<DeployChange>> {
        self.validate(def)?;
        let old = self.inner.stacks.lock().unwrap().get(&def.name).cloned();
        diff(old.as_deref(), def)
    }

    pub fn client(&self) -> &Client {
        &self.inner.client
    }

    /// Check a stack definition against this host without deploying it:
    /// every service must resolve (image source, paths, ports).
    pub fn validate(&self, def: &StackDef) -> Result<()> {
        validate_stack_name(&def.name)?;
        let host = crate::sandbox::host_facts(&self.inner.client)?;
        for (svc, spec) in &def.file.services {
            let mut s = instance_spec(def, svc, spec, 1, "0000")?;
            s.name = Some(instance_name(&def.name, svc, 1, "0000")?);
            crate::plan::resolve(&s, &def.file.volumes, &host, &def.base_dir)?;
            published(spec)?;
        }
        Ok(())
    }

    /// Deploy (or update) a stack. Returns what will change; the rollout
    /// itself happens in the background.
    pub fn deploy(&self, mut def: StackDef) -> Result<Vec<DeployChange>> {
        self.validate(&def)?;
        let old = self
            .inner
            .stacks
            .lock()
            .unwrap()
            .get(&def.qualified())
            .cloned();
        if let Some(old) = &old {
            let mut prev = (**old).clone();
            prev.previous = None;
            def.previous = Some(Box::new(prev));
            // A forced update survives a redeploy that does not ask for one.
            for (k, v) in &old.force {
                def.force.entry(k.clone()).or_insert(*v);
            }
        }
        let changes = diff(old.as_deref(), &def)?;
        self.inner.store.save(&def)?;
        self.apply(Arc::new(def));
        Ok(changes)
    }

    /// Hand a definition to the workers: update existing ones, start new
    /// ones, and tell those whose service is gone to clean up.
    fn apply(&self, def: Arc<StackDef>) {
        let name = def.qualified();
        self.inner
            .stacks
            .lock()
            .unwrap()
            .insert(name.clone(), def.clone());
        let mut workers = self.inner.workers.lock().unwrap();
        for svc in def.file.services.keys() {
            let key = (name.clone(), svc.clone());
            match workers.get(&key) {
                Some(w) => {
                    let mut slot = w.slot.lock().unwrap();
                    slot.def = def.clone();
                    slot.remove = false;
                    w.wake.notify_all();
                }
                None => {
                    let shared = Arc::new(WorkerShared {
                        slot: Mutex::new(Slot {
                            def: def.clone(),
                            remove: false,
                            remove_volumes: false,
                        }),
                        wake: Condvar::new(),
                        stop: AtomicBool::new(false),
                    });
                    workers.insert(key, shared.clone());
                    spawn_worker(self.inner.clone(), &def, svc.clone(), shared);
                }
            }
        }
        for ((stack, svc), w) in workers.iter() {
            if *stack == name && !def.file.services.contains_key(svc) {
                let mut slot = w.slot.lock().unwrap();
                slot.remove = true;
                w.wake.notify_all();
            }
        }
    }

    /// Remove a stack: every instance and published port; with `volumes`,
    /// its named volumes too. Returns once the workers have cleaned up (or
    /// after `timeout`).
    pub fn remove(&self, name: &str, volumes: bool, timeout: Duration) -> Result<()> {
        let Some(def) = self.inner.stacks.lock().unwrap().remove(name) else {
            return Err(Error::NotFound(format!("stack {name}")));
        };
        self.inner.store.remove(&def.org, &def.name)?;
        let ws: Vec<Arc<WorkerShared>> = self
            .inner
            .workers
            .lock()
            .unwrap()
            .iter()
            .filter(|((s, _), _)| s == name)
            .map(|(_, w)| w.clone())
            .collect();
        for w in &ws {
            let mut slot = w.slot.lock().unwrap();
            slot.remove = true;
            slot.remove_volumes = volumes;
            w.wake.notify_all();
        }
        let started = Instant::now();
        while started.elapsed() < timeout {
            let left = self
                .inner
                .workers
                .lock()
                .unwrap()
                .keys()
                .any(|(s, _)| s == name);
            if !left {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        Err(Error::invalid(format!(
            "stack {name}: still removing after {timeout:?}; it carries on in the background"
        )))
    }

    /// Go back to the previous deployment (the current one becomes the
    /// previous, so a second rollback undoes the first).
    pub fn rollback(&self, name: &str) -> Result<Vec<DeployChange>> {
        let cur = self.get_def(name)?;
        let prev = cur
            .previous
            .clone()
            .ok_or_else(|| Error::invalid(format!("stack {name} has no previous deployment")))?;
        let mut def = *prev;
        def.deployed_at = now_secs();
        let mut cur2 = (*cur).clone();
        cur2.previous = None;
        let changes = diff(Some(&cur), &def)?;
        def.previous = Some(Box::new(cur2));
        self.inner.store.save(&def)?;
        self.apply(Arc::new(def));
        Ok(changes)
    }

    /// Change one service's replica count.
    pub fn scale(&self, name: &str, service: &str, replicas: u32) -> Result<()> {
        let cur = self.get_def(name)?;
        let mut def = (*cur).clone();
        let spec = def
            .file
            .services
            .get_mut(service)
            .ok_or_else(|| Error::NotFound(format!("service {service} in stack {name}")))?;
        spec.deploy.get_or_insert_with(Default::default).replicas = Some(replicas);
        self.inner.store.save(&def)?;
        self.apply(Arc::new(def));
        Ok(())
    }

    /// Replace every instance of a service even though its spec is the same
    /// (`docker service update --force`): picks up a moved image tag or a
    /// changed bind-mounted file.
    pub fn redeploy(&self, name: &str, service: &str) -> Result<()> {
        let cur = self.get_def(name)?;
        cur.service(service)?;
        let mut def = (*cur).clone();
        *def.force.entry(service.to_string()).or_insert(0) += 1;
        self.inner.store.save(&def)?;
        self.apply(Arc::new(def));
        Ok(())
    }

    fn get_def(&self, name: &str) -> Result<Arc<StackDef>> {
        self.inner
            .stacks
            .lock()
            .unwrap()
            .get(name)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("stack {name}")))
    }

    /// Every stored definition, without computing status.
    pub fn definitions(&self) -> Vec<Arc<StackDef>> {
        self.inner
            .stacks
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect()
    }

    /// The stored definition of a stack.
    pub fn definition(&self, name: &str) -> Result<StackDef> {
        self.get_def(name).map(|d| (*d).clone())
    }

    pub fn list(&self) -> Vec<StackStatus> {
        let names: Vec<String> = self.inner.stacks.lock().unwrap().keys().cloned().collect();
        names.iter().filter_map(|n| self.status(n).ok()).collect()
    }

    pub fn status(&self, name: &str) -> Result<StackStatus> {
        let def = self.get_def(name)?;
        let st = self.inner.status.lock().unwrap();
        let services: Vec<ServiceStatus> = def
            .file
            .services
            .keys()
            .map(|svc| {
                st.get(&(name.to_string(), svc.clone()))
                    .cloned()
                    .unwrap_or_else(|| ServiceStatus {
                        service: svc.clone(),
                        state: "starting".into(),
                        ..Default::default()
                    })
            })
            .collect();
        let converged = services.iter().all(|s| s.state == "converged");
        Ok(StackStatus {
            name: def.name.clone(),
            org: def.org.to_string(),
            deployed_at: def.deployed_at,
            deployed_by: def.deployed_by.clone(),
            has_previous: def.previous.is_some(),
            converged,
            services,
        })
    }

    /// Recent output of a service's replicas (or one slot's).
    pub fn logs(
        &self,
        name: &str,
        service: &str,
        slot: Option<u32>,
        lines: usize,
    ) -> Result<BTreeMap<String, String>> {
        let def = self.get_def(name)?;
        let spec = def.service(service)?;
        let oci = crate::plan::ImageSource::parse(&spec.image)?.is_oci();
        let mut out = BTreeMap::new();
        let oc = crate::org::client(&self.inner.client, &def.org);
        for i in list_instances(&oc, &def.name, Some(service))? {
            if slot.is_some_and(|s| s != i.slot) {
                continue;
            }
            let sb = Sandbox::get(&oc, &i.name)?;
            let text = supervise::logs(&sb, service, oci, lines)
                .unwrap_or_else(|e| format!("(no logs: {e})"));
            out.insert(i.name, text);
        }
        Ok(out)
    }

    /// Stop every worker and the balancer. Apps keep running in their
    /// instances; published ports stop until the next start.
    pub fn shutdown(&self) {
        for w in self.inner.workers.lock().unwrap().values() {
            w.stop.store(true, Ordering::SeqCst);
            w.wake.notify_all();
        }
        self.inner.balancer.clear();
    }
}

/// What deploying `new` over `old` changes, per service.
fn diff(old: Option<&StackDef>, new: &StackDef) -> Result<Vec<DeployChange>> {
    let mut out = Vec::new();
    for (svc, spec) in &new.file.services {
        let rev = new.revision(svc)?;
        let replicas = spec.replicas();
        let change = match old.and_then(|o| o.file.services.get(svc).map(|s| (o, s))) {
            None => "create",
            Some((o, os)) => {
                if o.revision(svc)? != rev {
                    "update"
                } else if os.replicas() != replicas {
                    "scale"
                } else {
                    "unchanged"
                }
            }
        };
        out.push(DeployChange {
            service: svc.clone(),
            change: change.into(),
            rev,
            replicas,
        });
    }
    if let Some(o) = old {
        for svc in o.file.services.keys() {
            if !new.file.services.contains_key(svc) {
                out.push(DeployChange {
                    service: svc.clone(),
                    change: "remove".into(),
                    rev: String::new(),
                    replicas: 0,
                });
            }
        }
    }
    Ok(out)
}

/// The spec an instance of `service` is created from: labelled, with its
/// published host ports removed (the balancer serves them), and always
/// long-running, as swarm ignores `restart` in favour of `restart_policy`.
fn instance_spec(
    def: &StackDef,
    service: &str,
    spec: &SandboxSpec,
    slot: u32,
    rev: &str,
) -> Result<SandboxSpec> {
    let mut s = spec.clone();
    s.restart = Some(RestartMode::Always);
    s.ports.retain(|p| p.bind == PortBind::Guest);
    if let Some(d) = &s.deploy {
        s.labels.extend(d.labels.clone());
    }
    s.labels.insert(LABEL_STACK.into(), def.name.clone());
    s.labels.insert(LABEL_SERVICE.into(), service.into());
    s.labels.insert(LABEL_SLOT.into(), slot.to_string());
    s.labels.insert(LABEL_REV.into(), rev.into());
    Ok(s)
}

/// A published host port: where the balancer listens and the guest port it
/// forwards to.
#[derive(Debug, Clone, PartialEq)]
struct Published {
    listen: SocketAddr,
    target: u16,
}

fn published(spec: &SandboxSpec) -> Result<Vec<Published>> {
    let mut out = Vec::new();
    for p in &spec.ports {
        if p.bind == PortBind::Guest {
            continue;
        }
        let listen = crate::plan::normalize_addr(&p.listen, "127.0.0.1").map_err(Error::invalid)?;
        let connect =
            crate::plan::normalize_addr(&p.connect, "127.0.0.1").map_err(Error::invalid)?;
        let (lp, lh, lport) = split_addr(&listen).ok_or_else(|| {
            Error::invalid(format!(
                "port {listen}: a stack publishes single tcp ports (no ranges)"
            ))
        })?;
        let (_, _, cport) = split_addr(&connect).ok_or_else(|| {
            Error::invalid(format!(
                "port {connect}: a stack publishes single tcp ports (no ranges)"
            ))
        })?;
        if lp != "tcp" {
            return Err(Error::invalid(format!(
                "port {listen}: the stack balancer is tcp only"
            )));
        }
        if p.search.is_some() {
            return Err(Error::invalid(
                "a stack's published ports are fixed; port search is for isb up",
            ));
        }
        let host: IpAddr = lh
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse()
            .map_err(|_| {
                Error::invalid(format!("port {listen}: the host must be an IP address"))
            })?;
        out.push(Published {
            listen: SocketAddr::new(host, lport),
            target: cport,
        });
    }
    Ok(out)
}

/// A stack's instance as listed.
#[derive(Debug, Clone)]
struct Inst {
    name: String,
    slot: u32,
    rev: String,
    status: String,
}

impl Inst {
    fn running(&self) -> bool {
        self.status.eq_ignore_ascii_case("running")
    }
}

/// A stack's instances (of one service), using incus' server-side filter.
fn list_instances(client: &Client, stack: &str, service: Option<&str>) -> Result<Vec<Inst>> {
    let mut filter = format!("config.user.{LABEL_STACK} eq {stack}");
    if let Some(s) = service {
        filter.push_str(&format!(" and config.user.{LABEL_SERVICE} eq {s}"));
    }
    let v = client.get(&format!(
        "/1.0/instances?recursion=1&filter={}",
        encode_query(&filter)
    ))?;
    let mut out = Vec::new();
    for i in v.as_array().into_iter().flatten() {
        let info = crate::sandbox::SandboxInfo::from_api(i);
        let c = &info.config;
        // Filter again: an incus without filter support returns everything.
        if c.get(&format!("user.{LABEL_STACK}")).map(String::as_str) != Some(stack) {
            continue;
        }
        let svc = c
            .get(&format!("user.{LABEL_SERVICE}"))
            .cloned()
            .unwrap_or_default();
        if service.is_some_and(|s| s != svc) {
            continue;
        }
        out.push(Inst {
            name: info.name.clone(),
            slot: c
                .get(&format!("user.{LABEL_SLOT}"))
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            rev: c
                .get(&format!("user.{LABEL_REV}"))
                .cloned()
                .unwrap_or_default(),
            status: info.status.clone(),
        });
    }
    out.sort_by(|a, b| (a.slot, &a.name).cmp(&(b.slot, &b.name)));
    Ok(out)
}

/// An instance's init pid (changes on every start) and its first global
/// address on any interface but loopback, IPv4 preferred.
fn instance_state(client: &Client, name: &str) -> Result<(i64, Option<IpAddr>)> {
    let v = client.get(&format!("/1.0/instances/{}/state", encode_segment(name)))?;
    let pid = v.get("pid").and_then(Value::as_i64).unwrap_or(0);
    let mut v4 = None;
    let mut v6 = None;
    if let Some(nets) = v.get("network").and_then(Value::as_object) {
        for (ifname, n) in nets {
            if ifname == "lo" {
                continue;
            }
            for a in n
                .get("addresses")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if a.get("scope").and_then(Value::as_str) != Some("global") {
                    continue;
                }
                let Some(ip) = a
                    .get("address")
                    .and_then(Value::as_str)
                    .and_then(|s| s.parse::<IpAddr>().ok())
                else {
                    continue;
                };
                match ip {
                    IpAddr::V4(_) if v4.is_none() => v4 = Some(ip),
                    IpAddr::V6(_) if v6.is_none() => v6 = Some(ip),
                    _ => {}
                }
            }
        }
    }
    Ok((pid, v4.or(v6)))
}

/// Per-instance memory of a worker.
#[derive(Debug, Default)]
struct InstRt {
    /// The init pid secrets and the unit were last set up for.
    pid: i64,
    /// When that pid was first seen: the start of `start_period`.
    since: Option<Instant>,
    ip: Option<IpAddr>,
    failures: u32,
    healthy: Option<bool>,
    next_probe: Option<Instant>,
    last_probe: String,
    /// App restarts for failing health, since it was last healthy.
    unhealthy_restarts: u32,
    /// Restarts counted against `restart_policy.max_attempts`.
    restarts: VecDeque<Instant>,
    in_rotation: bool,
}

/// A service's worker.
struct Worker {
    inner: Arc<Inner>,
    /// The stack's own name: labels and instance names use it.
    stack: String,
    /// `org/stack`: the controller's key, and how events name it.
    q: String,
    /// A client on the stack's org (its incus project).
    oclient: Client,
    service: String,
    shared: Arc<WorkerShared>,
    rt: BTreeMap<String, InstRt>,
    /// The revision whose rollout failed and paused; not retried until the
    /// revision changes.
    paused: Option<(String, String)>,
    /// Backoff for creating into an empty slot that keeps failing.
    create_backoff: Option<(Instant, Duration)>,
    routes: BTreeMap<String, Published>,
    route_errors: BTreeMap<String, String>,
    /// The resolved spec of the current revision, for exec defaults.
    template: Option<(String, Desired)>,
    state: String,
    message: Option<String>,
    last_error: Option<String>,
    /// The instances as last listed, kept current through a rollout.
    insts: Vec<Inst>,
    rollout: Option<RolloutStatus>,
    /// Per route: accepted count at the last status, when, and conn/s history.
    rates: BTreeMap<String, (u64, Instant, VecDeque<f32>)>,
    /// `depends_on` was met once: from then on the service is reconciled
    /// whatever its dependencies do.
    deps_met: bool,
    org: OrgId,
    /// The addresses last published as the service's name.
    dns_last: Option<Vec<IpAddr>>,
    dns_error: Option<String>,
}

fn spawn_worker(inner: Arc<Inner>, def: &StackDef, service: String, shared: Arc<WorkerShared>) {
    let (stack, q, org) = (def.name.clone(), def.qualified(), def.org.clone());
    let oclient = crate::org::client(&inner.client, &def.org);
    let name = format!("isb-{q}-{service}");
    let r = std::thread::Builder::new().name(name).spawn(move || {
        let mut w = Worker {
            inner,
            stack,
            q,
            oclient,
            service,
            shared,
            rt: BTreeMap::new(),
            paused: None,
            create_backoff: None,
            routes: BTreeMap::new(),
            route_errors: BTreeMap::new(),
            template: None,
            state: "starting".into(),
            message: None,
            last_error: None,
            insts: Vec::new(),
            rollout: None,
            rates: BTreeMap::new(),
            deps_met: false,
            org,
            dns_last: None,
            dns_error: None,
        };
        w.run();
    });
    if let Err(e) = r {
        eprintln!("isb serve: cannot start a worker thread: {e}");
    }
}

impl Worker {
    fn log(&self, msg: &str) {
        self.event("info", None, msg);
    }

    fn event(&self, level: &str, instance: Option<&str>, msg: &str) {
        eprintln!("isb serve: {}/{}: {msg}", self.q, self.service);
        self.inner
            .emit(level, &self.q, &self.service, instance, msg.to_string());
    }

    fn client(&self) -> &Client {
        &self.oclient
    }

    fn key(&self) -> (String, String) {
        (self.q.clone(), self.service.clone())
    }

    fn run(&mut self) {
        loop {
            if self.shared.stop.load(Ordering::SeqCst) {
                return;
            }
            let (def, remove, remove_volumes) = {
                let s = self.shared.slot.lock().unwrap();
                (s.def.clone(), s.remove, s.remove_volumes)
            };
            if remove {
                self.teardown(&def, remove_volumes);
                // The service may have been deployed again meanwhile: then
                // this worker carries on instead of leaving it unattended.
                let mut ws = self.inner.workers.lock().unwrap();
                let slot = self.shared.slot.lock().unwrap();
                if slot.remove || self.shared.stop.load(Ordering::SeqCst) {
                    ws.remove(&self.key());
                    self.inner.status.lock().unwrap().remove(&self.key());
                    return;
                }
                continue;
            }
            if let Err(e) = self.pass(&def) {
                self.state = "failing".into();
                self.message = Some(e.to_string());
                // Once per distinct error, not once per pass.
                if self.last_error.as_deref() != Some(&e.to_string()) {
                    self.event("error", None, &e.to_string());
                    self.last_error = Some(e.to_string());
                }
                self.publish_status(&def);
                let slot = self.shared.slot.lock().unwrap();
                if Arc::ptr_eq(&slot.def, &def) && !slot.remove {
                    let _ = self.shared.wake.wait_timeout(slot, self.inner.interval);
                }
                continue;
            }
            self.last_error = None;
            let slot = self.shared.slot.lock().unwrap();
            if Arc::ptr_eq(&slot.def, &def) && !slot.remove {
                let _ = self.shared.wake.wait_timeout(slot, self.inner.interval);
            }
        }
    }

    /// A deployment arrived (or removal was asked) since `def` was read.
    fn superseded(&self, def: &Arc<StackDef>) -> bool {
        let s = self.shared.slot.lock().unwrap();
        s.remove || !Arc::ptr_eq(&s.def, def) || self.shared.stop.load(Ordering::SeqCst)
    }

    /// Delete this service's instances and routes, then leave.
    fn teardown(&mut self, def: &StackDef, volumes: bool) {
        // The name goes first, whoever published it.
        if let Some(dir) = crate::discovery::org_dir(&self.org).filter(|d| d.is_dir()) {
            if let Err(e) =
                crate::discovery::publish(&dir, &self.org, &self.stack, &self.service, &[])
            {
                self.log(&format!("cannot remove the service name: {e}"));
            }
        }
        self.dns_last = Some(Vec::new());
        for (k, _) in std::mem::take(&mut self.routes) {
            self.inner.balancer.remove_route(&k);
        }
        match list_instances(self.client(), &self.stack, Some(&self.service)) {
            Ok(insts) => {
                for i in insts {
                    self.log(&format!("removing {}", i.name));
                    if let Err(e) = Sandbox::remove(self.client(), &i.name, true) {
                        if !e.is_not_found() {
                            self.log(&format!("cannot remove {}: {e}", i.name));
                        }
                    }
                }
            }
            Err(e) => self.log(&format!("cannot list instances to remove: {e}")),
        }
        if volumes {
            self.remove_volumes(def);
        }
        self.rt.clear();
        self.template = None;
    }

    fn remove_volumes(&self, def: &StackDef) {
        let Ok(spec) = def.service(&self.service) else {
            return;
        };
        let Ok(host) = crate::sandbox::host_facts(self.client()) else {
            return;
        };
        let Ok(pool) = host.pick_pool(spec.storage.as_deref()) else {
            return;
        };
        for v in &spec.volumes {
            if v.mount_type != crate::spec::MountType::Volume {
                continue;
            }
            let d = def.file.volumes.get(&v.source);
            if v.external || d.is_some_and(|d| d.external) {
                continue;
            }
            let name = d
                .and_then(|d| d.name.clone())
                .unwrap_or_else(|| v.source.clone());
            let vpool = match v.pool.as_deref().or(d.and_then(|d| d.pool.as_deref())) {
                Some(p) if p != "auto" => p.to_string(),
                _ => pool.clone(),
            };
            match crate::volume::remove(self.client(), &vpool, &name) {
                Ok(()) => self.log(&format!("volume {name}: deleted")),
                Err(e) if e.is_not_found() => {}
                // Another service of the stack may still be using it.
                Err(e) => self.log(&format!("volume {name}: kept ({e})")),
            }
        }
    }

    /// One reconcile pass.
    fn pass(&mut self, def: &Arc<StackDef>) -> Result<()> {
        let spec = def.service(&self.service)?.clone();
        let rev = def.revision(&self.service)?;
        let replicas = spec.replicas();
        let oci = crate::plan::ImageSource::parse(&spec.image)?.is_oci();
        let probe = spec.health_probe().map_err(Error::invalid)?;

        if !self.deps_met {
            if let Some(msg) = self.waiting_for(&spec) {
                self.state = "waiting".into();
                self.message = Some(msg);
                self.publish_status(def);
                return Ok(());
            }
            self.deps_met = true;
        }
        if self.template.as_ref().is_none_or(|(r, _)| *r != rev) {
            let mut s = instance_spec(def, &self.service, &spec, 1, &rev)?;
            s.name = Some(instance_name(&self.stack, &self.service, 1, "0000")?);
            let d = crate::sandbox::resolve(self.client(), &s, &def.file.volumes, &def.base_dir)?;
            self.template = Some((rev.clone(), d));
        }
        self.set_routes(&spec);

        let mut insts = list_instances(self.client(), &self.stack, Some(&self.service))?;
        self.rt.retain(|n, _| insts.iter().any(|i| i.name == *n));

        // Scale down, highest slots first.
        let extra: Vec<Inst> = insts
            .iter()
            .filter(|i| i.slot > replicas || i.slot == 0)
            .cloned()
            .collect();
        for i in extra.iter().rev() {
            self.log(&format!("scaling down: removing {}", i.name));
            self.retire(&i.name)?;
        }
        insts.retain(|i| i.slot >= 1 && i.slot <= replicas);
        self.insts = insts.clone();

        // Keep what exists running, set up and healthy.
        for i in &insts {
            self.maintain(def, i, &spec, oci, probe.as_ref())?;
        }
        // An interrupted rollout can leave an old instance next to a current
        // one in the same slot: once the current one serves, drop the old.
        for slot in 1..=replicas {
            let current_ok = insts.iter().any(|i| {
                i.slot == slot
                    && i.rev == rev
                    && self.rt.get(&i.name).is_some_and(|r| r.in_rotation)
            });
            if current_ok {
                for i in insts.iter().filter(|i| i.slot == slot && i.rev != rev) {
                    self.log(&format!("removing leftover {}", i.name));
                    self.retire(&i.name)?;
                }
            }
            let mut current: Vec<&Inst> = insts
                .iter()
                .filter(|i| i.slot == slot && i.rev == rev)
                .collect();
            // Two current instances in one slot (a crash mid-create): keep one.
            while current.len() > 1 {
                let i = current.pop().unwrap();
                self.log(&format!("removing duplicate {}", i.name));
                self.retire(&i.name)?;
            }
        }
        self.sync_routes();
        self.publish_status(def);

        // Roll out: slots without a current instance.
        let mut pending: Vec<(u32, Option<String>)> = Vec::new();
        for slot in 1..=replicas {
            if insts.iter().any(|i| i.slot == slot && i.rev == rev) {
                continue;
            }
            let old = insts
                .iter()
                .find(|i| i.slot == slot)
                .map(|i| i.name.clone());
            pending.push((slot, old));
        }
        if pending.is_empty() {
            self.create_backoff = None;
            let all_ok = insts
                .iter()
                .all(|i| i.rev == rev && self.rt.get(&i.name).is_some_and(|r| r.in_rotation));
            self.state = if all_ok { "converged" } else { "failing" }.into();
            if all_ok {
                self.message = None;
            } else if self.message.is_none() {
                self.message = Some("some replicas are not healthy".into());
            }
            self.publish_status(def);
            return Ok(());
        }
        if self.paused.as_ref().is_some_and(|(r, _)| *r == rev) {
            self.state = "paused".into();
            self.message = self.paused.as_ref().map(|(_, m)| m.clone());
            self.publish_status(def);
            return Ok(());
        }
        if let Some((at, wait)) = self.create_backoff {
            if at.elapsed() < wait {
                return Ok(());
            }
        }
        self.state = "updating".into();
        self.message = None;
        self.publish_status(def);

        let uc: UpdateConfig = spec
            .deploy
            .as_ref()
            .and_then(|d| d.update_config.clone())
            .unwrap_or_default();
        let parallel = match uc.parallelism.unwrap_or(1) {
            0 => pending.len(),
            n => n as usize,
        };
        let delay = uc
            .delay
            .as_deref()
            .map(crate::flex::parse_duration)
            .transpose()
            .map_err(Error::invalid)?
            .unwrap_or_default();
        let monitor = uc
            .monitor
            .as_deref()
            .map(crate::flex::parse_duration)
            .transpose()
            .map_err(Error::invalid)?
            .unwrap_or(Duration::from_secs(5));
        let order = uc.order.unwrap_or_default();
        let order_name = match order {
            UpdateOrder::StopFirst => "stop-first",
            UpdateOrder::StartFirst => "start-first",
        };
        self.rollout = Some(RolloutStatus {
            to_rev: rev.clone(),
            order: order_name.into(),
            parallelism: parallel,
            done: 0,
            total: pending.len(),
            started_at: now_secs(),
            slots: pending
                .iter()
                .map(|(slot, old)| SlotRollout {
                    slot: *slot,
                    old: old.clone(),
                    old_rev: old
                        .as_ref()
                        .and_then(|o| insts.iter().find(|i| i.name == *o))
                        .map(|i| i.rev.clone()),
                    old_state: if old.is_some() { "serving" } else { "none" }.into(),
                    new: None,
                    new_state: "waiting".into(),
                })
                .collect(),
        });
        let rollout_started = Instant::now();
        self.log(&format!(
            "rolling out rev {rev} to {} slot(s), {order_name}",
            pending.len()
        ));
        self.publish_status(def);
        let r = self.roll(
            def,
            &spec,
            &rev,
            &pending,
            parallel,
            delay,
            monitor,
            order,
            oci,
            probe.as_ref(),
            &uc,
        );
        let rollout = self.rollout.take();
        if let (Ok(true), Some(ro)) = (&r, rollout) {
            self.log(&format!(
                "rollout of rev {rev} complete: {}/{} slot(s) in {:.0?}",
                ro.done,
                ro.total,
                rollout_started.elapsed()
            ));
        }
        self.publish_status(def);
        r.map(|_| ())
    }

    /// Replace the pending slots in batches. Ok(true) when every slot made
    /// it, Ok(false) when the rollout stopped (paused, rolled back, retrying).
    #[allow(clippy::too_many_arguments)]
    fn roll(
        &mut self,
        def: &Arc<StackDef>,
        spec: &SandboxSpec,
        rev: &String,
        pending: &[(u32, Option<String>)],
        parallel: usize,
        delay: Duration,
        monitor: Duration,
        order: UpdateOrder,
        oci: bool,
        probe: Option<&HealthProbe>,
        uc: &UpdateConfig,
    ) -> Result<bool> {
        for (n, batch) in pending.chunks(parallel).enumerate() {
            if n > 0 && !delay.is_zero() {
                std::thread::sleep(delay);
            }
            if self.superseded(def) {
                return Ok(false);
            }
            for (slot, old) in batch {
                let r = self.replace(
                    def,
                    spec,
                    rev,
                    *slot,
                    old.as_deref(),
                    order,
                    oci,
                    probe,
                    monitor,
                );
                if let Err(e) = r {
                    let msg = format!("slot {slot}: {e}");
                    self.event("error", None, &msg);
                    if old.is_none() {
                        // Nothing to protect: keep trying, slower each time.
                        let wait = self
                            .create_backoff
                            .map(|(_, w)| (w * 2).min(Duration::from_secs(300)))
                            .unwrap_or(Duration::from_secs(10));
                        self.create_backoff = Some((Instant::now(), wait));
                        self.state = "failing".into();
                        self.message = Some(format!("{msg}; retrying in {wait:?}"));
                        self.publish_status(def);
                        return Ok(false);
                    }
                    match uc.failure_action.unwrap_or_default() {
                        FailureAction::Continue => continue,
                        FailureAction::Pause => {
                            self.event("warn", None, &format!("rollout of rev {rev} paused"));
                            self.paused = Some((rev.clone(), format!("rollout paused: {msg}")));
                            return Ok(false);
                        }
                        FailureAction::Rollback => {
                            self.paused = Some((rev.clone(), format!("rolled back: {msg}")));
                            let ctl = Controller {
                                inner: self.inner.clone(),
                            };
                            self.event(
                                "warn",
                                None,
                                &format!("rollout of rev {rev} failed; rolling back"),
                            );
                            if let Err(e) = ctl.rollback(&self.q) {
                                self.event("error", None, &format!("rollback failed: {e}"));
                            }
                            return Ok(false);
                        }
                    }
                }
            }
        }
        Ok(true)
    }

    /// Update one slot of the rollout display, and publish it.
    fn slot_state(
        &mut self,
        def: &StackDef,
        slot: u32,
        old_state: Option<&str>,
        new: Option<&str>,
        new_state: Option<&str>,
    ) {
        if let Some(ro) = &mut self.rollout {
            if let Some(s) = ro.slots.iter_mut().find(|s| s.slot == slot) {
                if let Some(o) = old_state {
                    s.old_state = o.into();
                }
                if let Some(n) = new {
                    s.new = Some(n.into());
                }
                if let Some(n) = new_state {
                    s.new_state = n.into();
                    if n == "serving" {
                        ro.done += 1;
                    }
                }
            }
        }
        self.publish_status(def);
    }

    /// Unmet `depends_on`, as a message.
    fn waiting_for(&self, spec: &SandboxSpec) -> Option<String> {
        let st = self.inner.status.lock().unwrap();
        for (dep, d) in &spec.depends_on {
            let s = st.get(&(self.q.clone(), dep.clone()));
            let ok = match d.condition {
                DependCondition::ServiceStarted => s.is_some_and(|s| s.running > 0),
                DependCondition::ServiceHealthy => s.is_some_and(|s| s.healthy > 0),
            };
            if !ok {
                return Some(format!("waiting for {dep} ({:?})", d.condition));
            }
        }
        None
    }

    fn handle(&self, name: &str) -> Sandbox {
        let (_, d) = self.template.as_ref().expect("resolved in pass");
        Sandbox::like(self.client(), name, d)
    }

    /// Keep one instance running, set up after every boot, health-checked,
    /// and in or out of rotation.
    fn maintain(
        &mut self,
        def: &StackDef,
        i: &Inst,
        spec: &SandboxSpec,
        oci: bool,
        probe: Option<&HealthProbe>,
    ) -> Result<()> {
        let policy = spec.deploy.as_ref().and_then(|d| d.restart_policy.clone());
        let condition = policy
            .as_ref()
            .and_then(|p| p.condition)
            .unwrap_or_default();
        let sb = self.handle(&i.name);
        if !i.running() {
            self.set_rotation(&i.name, false);
            if condition == RestartCondition::None {
                return Ok(());
            }
            if self.restart_budget_spent(&i.name, policy.as_ref()) {
                self.message = Some(format!("{}: restart limit reached", i.name));
                return Ok(());
            }
            self.event(
                "warn",
                Some(&i.name),
                &format!("{} is {}; starting it", i.name, i.status),
            );
            self.count_restart(&i.name);
            if let Err(e) = sb.start() {
                self.log(&format!("cannot start {}: {e}", i.name));
                return Ok(());
            }
        }
        let (pid, ip) = match instance_state(self.client(), &i.name) {
            Ok(s) => s,
            Err(e) if e.is_not_found() => return Ok(()),
            Err(e) => return Err(e),
        };
        let rt = self.rt.entry(i.name.clone()).or_default();
        rt.ip = ip;
        if rt.pid != pid {
            // A (re)boot: /run/secrets is a fresh tmpfs, and the unit may be
            // from an older isb. Probing starts over.
            rt.pid = pid;
            rt.since = Some(Instant::now());
            rt.failures = 0;
            rt.healthy = None;
            rt.next_probe = None;
            let r = self.setup(def, &sb, spec, oci);
            if let Err(e) = r {
                // Leave pid unset so the next pass tries again.
                if let Some(rt) = self.rt.get_mut(&i.name) {
                    rt.pid = 0;
                }
                self.set_rotation(&i.name, false);
                return Err(Error::OperationFailed {
                    step: format!("set up {}", i.name),
                    message: e.to_string(),
                });
            }
        }
        let alive = self.alive(&sb, spec, oci);
        let healthy = match probe {
            None => alive,
            Some(p) => {
                let rt = self.rt.get_mut(&i.name).unwrap();
                let since = rt.since.unwrap_or_else(Instant::now);
                let in_start = since.elapsed() < p.start_period;
                if rt.next_probe.is_none_or(|t| Instant::now() >= t) {
                    let r = supervise::probe(&sb, p);
                    let rt = self.rt.get_mut(&i.name).unwrap();
                    rt.last_probe = r.output.clone();
                    if r.ok {
                        rt.failures = 0;
                        rt.healthy = Some(true);
                        rt.unhealthy_restarts = 0;
                    } else if !in_start {
                        rt.failures += 1;
                        if rt.failures >= p.retries {
                            rt.healthy = Some(false);
                        }
                    }
                    let wait = if rt.healthy.is_none() {
                        p.start_interval
                    } else {
                        p.interval
                    };
                    rt.next_probe = Some(Instant::now() + wait);
                }
                self.rt[&i.name].healthy == Some(true) && alive
            }
        };
        self.set_rotation(&i.name, healthy);
        let unhealthy = self.rt[&i.name].healthy == Some(false);
        if unhealthy && condition != RestartCondition::None {
            if self.restart_budget_spent(&i.name, policy.as_ref()) {
                self.message = Some(format!("{}: unhealthy, restart limit reached", i.name));
                return Ok(());
            }
            let n = self.rt[&i.name].unhealthy_restarts;
            if n >= RESTARTS_BEFORE_REPLACE {
                self.log(&format!(
                    "{} stayed unhealthy through {n} restarts; replacing it",
                    i.name
                ));
                let rt = self.rt.get_mut(&i.name).unwrap();
                rt.unhealthy_restarts = 0;
                drop(sb);
                self.retire(&i.name)?;
                return Ok(());
            }
            self.event(
                "warn",
                Some(&i.name),
                &format!(
                    "{} is unhealthy ({}); restarting its app",
                    i.name,
                    self.rt[&i.name]
                        .last_probe
                        .lines()
                        .last()
                        .unwrap_or("probe failed")
                ),
            );
            self.count_restart(&i.name);
            let rt = self.rt.get_mut(&i.name).unwrap();
            rt.unhealthy_restarts += 1;
            rt.failures = 0;
            rt.healthy = None;
            rt.since = Some(Instant::now());
            supervise::restart_app(&sb, &self.service, oci)?;
        }
        Ok(())
    }

    /// Secrets and the app's unit, after a boot or on creation.
    fn setup(&self, def: &StackDef, sb: &Sandbox, spec: &SandboxSpec, oci: bool) -> Result<()> {
        if !spec.secrets.is_empty() {
            supervise::push_secrets(sb, spec, &def.secret_values()?)?;
        }
        if spec.command.is_some() && !oci {
            let mut s = spec.clone();
            s.restart = Some(RestartMode::Always);
            supervise::install(sb, &self.service, &s, !spec.secrets.is_empty())?;
        }
        Ok(())
    }

    /// The app's process is up: its unit is active, or (OCI, or no command)
    /// the instance runs.
    fn alive(&self, sb: &Sandbox, spec: &SandboxSpec, oci: bool) -> bool {
        if spec.command.is_some() && !oci {
            return supervise::unit_state(sb, &self.service).is_ok_and(|s| s == "active");
        }
        sb.info()
            .is_ok_and(|i| i.status.eq_ignore_ascii_case("running"))
    }

    fn count_restart(&mut self, name: &str) {
        self.rt
            .entry(name.to_string())
            .or_default()
            .restarts
            .push_back(Instant::now());
    }

    fn restart_budget_spent(
        &mut self,
        name: &str,
        policy: Option<&crate::spec::RestartPolicy>,
    ) -> bool {
        let Some(p) = policy else { return false };
        let Some(max) = p.max_attempts else {
            return false;
        };
        let window = p
            .window
            .as_deref()
            .and_then(|w| crate::flex::parse_duration(w).ok());
        let rt = self.rt.entry(name.to_string()).or_default();
        if let Some(w) = window {
            while rt.restarts.front().is_some_and(|t| t.elapsed() > w) {
                rt.restarts.pop_front();
            }
        }
        rt.restarts.len() as u32 >= max
    }

    /// Replace (or fill) one slot with an instance of the current revision.
    #[allow(clippy::too_many_arguments)]
    fn replace(
        &mut self,
        def: &StackDef,
        spec: &SandboxSpec,
        rev: &str,
        slot: u32,
        old: Option<&str>,
        order: UpdateOrder,
        oci: bool,
        probe: Option<&HealthProbe>,
        monitor: Duration,
    ) -> Result<()> {
        if let (Some(o), UpdateOrder::StopFirst) = (old, order) {
            self.log(&format!("slot {slot}: replacing {o} (stop-first)"));
            self.slot_state(def, slot, Some("draining"), None, None);
            self.retire(o)?;
            self.slot_state(def, slot, Some("retired"), None, None);
        }
        let name = instance_name(&self.stack, &self.service, slot, &new_id())?;
        self.log(&format!("slot {slot}: creating {name} (rev {rev})"));
        self.slot_state(def, slot, None, Some(&name), Some("creating"));
        let mut s = instance_spec(def, &self.service, spec, slot, rev)?;
        s.name = Some(name.clone());
        let d = crate::sandbox::resolve(self.client(), &s, &def.file.volumes, &def.base_dir)?;
        let stack = self.q.clone();
        let mut report = |m: &str| eprintln!("isb serve: {stack}: {m}");
        let created =
            crate::sandbox::ensure(self.client(), &d, EnsureOptions::default(), &mut report);
        let result = created.and_then(|_| {
            let inst = Inst {
                name: name.clone(),
                slot,
                rev: rev.to_string(),
                status: "Running".into(),
            };
            self.insts.push(inst.clone());
            self.slot_state(def, slot, None, None, Some("probing"));
            self.wait_serving(def, &inst, spec, oci, probe, monitor)
        });
        if let Err(e) = result {
            self.event(
                "error",
                Some(&name),
                &format!("{name} did not come up: {e}"),
            );
            self.slot_state(def, slot, None, None, Some("failed"));
            let _ = self.retire(&name);
            return Err(e);
        }
        if let (Some(o), UpdateOrder::StartFirst) = (old, order) {
            self.log(&format!("slot {slot}: {name} is serving; retiring {o}"));
            self.slot_state(def, slot, Some("draining"), None, None);
            self.retire(o)?;
            self.slot_state(def, slot, Some("retired"), None, None);
        }
        self.slot_state(def, slot, None, None, Some("serving"));
        Ok(())
    }

    /// Wait for a new instance to pass its health (or, without a
    /// healthcheck, for its app to run), put it in rotation, then watch it
    /// for `monitor`.
    fn wait_serving(
        &mut self,
        def: &StackDef,
        i: &Inst,
        spec: &SandboxSpec,
        oci: bool,
        probe: Option<&HealthProbe>,
        monitor: Duration,
    ) -> Result<()> {
        let deadline = match probe {
            Some(p) => p.start_period + p.interval * p.retries + Duration::from_secs(30),
            None => Duration::from_secs(60),
        }
        .max(Duration::from_secs(60));
        let started = Instant::now();
        loop {
            self.maintain(def, i, spec, oci, probe)?;
            if self.rt.get(&i.name).is_some_and(|r| r.in_rotation) {
                break;
            }
            if self
                .rt
                .get(&i.name)
                .is_some_and(|r| r.healthy == Some(false))
            {
                return Err(Error::invalid(format!(
                    "unhealthy: {}",
                    self.rt[&i.name].last_probe
                )));
            }
            if started.elapsed() > deadline {
                let why = self
                    .rt
                    .get(&i.name)
                    .map(|r| r.last_probe.clone())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "its app is not running".into());
                return Err(Error::invalid(format!(
                    "not serving after {:?}: {why}",
                    started.elapsed()
                )));
            }
            // Probe on the start interval rather than waiting for the next one.
            if let Some(rt) = self.rt.get_mut(&i.name) {
                rt.next_probe = None;
            }
            std::thread::sleep(
                probe
                    .map(|p| p.start_interval)
                    .unwrap_or(Duration::from_secs(1))
                    .min(Duration::from_secs(2)),
            );
        }
        self.sync_routes();
        self.slot_state(def, i.slot, None, None, Some("monitoring"));
        let watch = Instant::now();
        while watch.elapsed() < monitor {
            std::thread::sleep(Duration::from_secs(1).min(monitor));
            if let Some(rt) = self.rt.get_mut(&i.name) {
                rt.next_probe = None;
            }
            self.maintain(def, i, spec, oci, probe)?;
            if !self.rt.get(&i.name).is_some_and(|r| r.in_rotation) {
                return Err(Error::invalid(format!(
                    "failed within the {monitor:?} monitor period"
                )));
            }
        }
        Ok(())
    }

    /// Take an instance out of rotation, let its connections drain, then
    /// delete it.
    fn retire(&mut self, name: &str) -> Result<()> {
        let ip = self.rt.get(name).and_then(|r| r.ip);
        self.set_rotation(name, false);
        self.sync_routes();
        if let Some(ip) = ip {
            for (k, p) in &self.routes {
                self.inner
                    .balancer
                    .wait_drained(k, SocketAddr::new(ip, p.target), DRAIN);
            }
        }
        if let Ok(sb) = Sandbox::get(self.client(), name) {
            let _ = sb.stop(false, Duration::from_secs(10));
        }
        self.rt.remove(name);
        self.insts.retain(|i| i.name != name);
        match Sandbox::remove(self.client(), name, true) {
            Err(e) if !e.is_not_found() => Err(e),
            _ => Ok(()),
        }
    }

    fn set_rotation(&mut self, name: &str, on: bool) {
        let rt = self.rt.entry(name.to_string()).or_default();
        if rt.in_rotation != on {
            rt.in_rotation = on;
            self.sync_routes();
        }
    }

    /// Bring the balancer's routes in line with the spec's published ports.
    fn set_routes(&mut self, spec: &SandboxSpec) {
        let want: BTreeMap<String, Published> = match published(spec) {
            Ok(ps) => ps
                .into_iter()
                .map(|p| (format!("{}/{}/{}", self.q, self.service, p.listen), p))
                .collect(),
            Err(e) => {
                self.message = Some(e.to_string());
                BTreeMap::new()
            }
        };
        let stale: Vec<String> = self
            .routes
            .keys()
            .filter(|k| !want.contains_key(*k))
            .cloned()
            .collect();
        for k in stale {
            self.inner.balancer.remove_route(&k);
            self.routes.remove(&k);
            self.route_errors.remove(&k);
        }
        for (k, p) in want {
            self.routes.entry(k).or_insert(p);
        }
        self.sync_routes();
    }

    fn sync_routes(&mut self) {
        let mut errors = BTreeMap::new();
        for (k, p) in &self.routes {
            let backends: Vec<SocketAddr> = self
                .rt
                .values()
                .filter(|r| r.in_rotation)
                .filter_map(|r| r.ip)
                .map(|ip| SocketAddr::new(ip, p.target))
                .collect();
            if let Err(e) = self.inner.balancer.set_route(k, p.listen, backends) {
                errors.insert(k.clone(), e.to_string());
            }
        }
        for (k, e) in &errors {
            if self.route_errors.get(k) != Some(e) {
                self.log(&format!("cannot publish {k}: {e}"));
            }
        }
        self.route_errors = errors;
        self.sync_dns();
    }

    /// Publish the in-rotation replicas' addresses as the service's name
    /// (see [`crate::discovery`]). Nothing to do in the default org, or in an
    /// org created without service names.
    fn sync_dns(&mut self) {
        let Some(dir) = crate::discovery::org_dir(&self.org) else {
            return;
        };
        let mut ips: Vec<IpAddr> = self
            .rt
            .values()
            .filter(|r| r.in_rotation)
            .filter_map(|r| r.ip)
            .collect();
        ips.sort();
        // A worker that has published nothing yet (a daemon restart) leaves
        // the last records alone until a replica is back in rotation, rather
        // than blanking the name while health is being re-established.
        let fresh = self.dns_last.is_none() && ips.is_empty();
        if fresh || self.dns_last.as_ref() == Some(&ips) || !dir.is_dir() {
            return;
        }
        match crate::discovery::publish(&dir, &self.org, &self.stack, &self.service, &ips) {
            Ok(()) => {
                self.dns_last = Some(ips);
                self.dns_error = None;
            }
            Err(e) => {
                let e = e.to_string();
                if self.dns_error.as_deref() != Some(&e) {
                    self.event(
                        "warn",
                        None,
                        &format!("cannot publish the service name: {e}"),
                    );
                    self.dns_error = Some(e);
                }
            }
        }
    }

    fn publish_status(&mut self, def: &StackDef) {
        // An address can change without a rotation change (a restart).
        self.sync_dns();
        let Ok(spec) = def.service(&self.service) else {
            return;
        };
        let rev = def.revision(&self.service).unwrap_or_default();
        let probe = matches!(spec.health_probe(), Ok(Some(_)));
        let snap = self.inner.snapshot.lock().unwrap().instances.clone();
        let instances: Vec<InstanceStatus> = self
            .insts
            .iter()
            .map(|i| {
                let rt = self.rt.get(&i.name);
                let m = snap.get(&format!("{}/{}", self.oclient.project_name(), i.name));
                let health = match (probe, rt.and_then(|r| r.healthy)) {
                    (false, _) => "none",
                    (true, Some(true)) => "healthy",
                    (true, Some(false)) => "unhealthy",
                    (true, None) => "starting",
                };
                InstanceStatus {
                    name: i.name.clone(),
                    slot: i.slot,
                    rev: i.rev.clone(),
                    status: m
                        .map(|m| m.status.clone())
                        .unwrap_or_else(|| i.status.clone()),
                    health: health.into(),
                    ip: rt.and_then(|r| r.ip).map(|ip| ip.to_string()),
                    in_rotation: rt.is_some_and(|r| r.in_rotation),
                    restarts: rt.map(|r| r.restarts.len() as u32).unwrap_or(0),
                    last_probe: rt.map(|r| r.last_probe.clone()).unwrap_or_default(),
                    cpu_pct: m.and_then(|m| m.cpu_pct),
                    cpu_history: m.map(|m| m.cpu_history.clone()).unwrap_or_default(),
                    mem_bytes: m.and_then(|m| m.mem_bytes),
                }
            })
            .collect();
        let mut instances = instances;
        instances.sort_by(|a, b| (a.slot, &a.name).cmp(&(b.slot, &b.name)));
        let routes = self.inner.balancer.routes();
        let now = Instant::now();
        let mut ports = Vec::new();
        for (k, p) in &self.routes {
            let r = routes.iter().find(|r| r.key == *k);
            let accepted = r.map(|r| r.accepted).unwrap_or(0);
            let e = self
                .rates
                .entry(k.clone())
                .or_insert_with(|| (accepted, now, VecDeque::new()));
            let dt = now.duration_since(e.1).as_secs_f32();
            // Only sample on a reconcile-sized step, so a burst of status
            // updates during a rollout does not flatten the curve.
            if dt >= 1.0 {
                let rate = accepted.saturating_sub(e.0) as f32 / dt;
                if e.2.len() == crate::metrics::HISTORY {
                    e.2.pop_front();
                }
                e.2.push_back(rate);
                e.0 = accepted;
                e.1 = now;
            }
            ports.push(PortStatus {
                listen: p.listen.to_string(),
                target: p.target,
                backends: r
                    .map(|r| r.backends.iter().map(|b| b.addr.to_string()).collect())
                    .unwrap_or_default(),
                error: self.route_errors.get(k).cloned(),
                accepted,
                active: r
                    .map(|r| {
                        r.backends
                            .iter()
                            .chain(r.draining.iter())
                            .map(|b| b.active)
                            .sum()
                    })
                    .unwrap_or(0),
                rate_history: e.2.iter().copied().collect(),
            });
        }
        self.rates.retain(|k, _| self.routes.contains_key(k));
        let running = instances
            .iter()
            .filter(|i| i.status.eq_ignore_ascii_case("running"))
            .count() as u32;
        let healthy = instances.iter().filter(|i| i.in_rotation).count() as u32;
        let st = ServiceStatus {
            service: self.service.clone(),
            image: spec.image.clone(),
            rev,
            replicas: spec.replicas(),
            running,
            healthy,
            state: self.state.clone(),
            message: self.message.clone(),
            instances,
            ports,
            rollout: self.rollout.clone(),
            checked_at: now_secs(),
        };
        self.inner.status.lock().unwrap().insert(self.key(), st);
    }
}

/// Names of the services that are not converged, for a deploy waiting on
/// its rollout.
pub fn unsettled(st: &StackStatus) -> BTreeSet<String> {
    st.services
        .iter()
        .filter(|s| s.state != "converged")
        .map(|s| s.service.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(y: &str) -> StackDef {
        StackDef {
            name: "app".into(),
            org: crate::org::OrgId::default_org(),
            file: serde_yaml_ng::from_str(y).unwrap(),
            base_dir: "/".into(),
            secrets: BTreeMap::new(),
            force: BTreeMap::new(),
            deployed_at: 0,
            deployed_by: String::new(),
            previous: None,
        }
    }

    #[test]
    fn published_ports() {
        let s: SandboxSpec = serde_yaml_ng::from_str(
            "image: x\nports: ['8080:80', '0.0.0.0:9000:9000', {listen: 'tcp:127.0.0.1:5000', connect: 'tcp:127.0.0.1:5000', bind: guest}]\n",
        )
        .unwrap();
        let p = published(&s).unwrap();
        assert_eq!(
            p,
            vec![
                Published {
                    listen: "127.0.0.1:8080".parse().unwrap(),
                    target: 80
                },
                Published {
                    listen: "0.0.0.0:9000".parse().unwrap(),
                    target: 9000
                },
            ]
        );
        let bad: SandboxSpec =
            serde_yaml_ng::from_str("image: x\nports: ['8000-8001:8000-8001']\n").unwrap();
        assert!(published(&bad).is_err());
        let udp: SandboxSpec = serde_yaml_ng::from_str("image: x\nports: ['53:53/udp']\n").unwrap();
        assert!(published(&udp).is_err());
    }

    #[test]
    fn instance_specs_are_labelled_and_unpublished() {
        let d = def(
            "services:\n  web: {image: x, ports: ['8080:80'], deploy: {labels: {tier: front}}}\n",
        );
        let s = instance_spec(&d, "web", d.service("web").unwrap(), 2, "abcd").unwrap();
        assert!(s.ports.is_empty());
        assert_eq!(s.labels["isb.stack"], "app");
        assert_eq!(s.labels["isb.slot"], "2");
        assert_eq!(s.labels["isb.rev"], "abcd");
        assert_eq!(s.labels["tier"], "front");
        assert_eq!(s.restart, Some(RestartMode::Always));
    }

    #[test]
    fn deploy_diff() {
        let a = def("services:\n  web: {image: x}\n  db: {image: y}\n");
        let b = def("services:\n  web: {image: x, deploy: {replicas: 3}}\n  api: {image: z}\n");
        let c: BTreeMap<String, String> = diff(Some(&a), &b)
            .unwrap()
            .into_iter()
            .map(|c| (c.service, c.change))
            .collect();
        assert_eq!(c["web"], "scale");
        assert_eq!(c["api"], "create");
        assert_eq!(c["db"], "remove");
        let c = diff(
            Some(&a),
            &def("services:\n  web: {image: x2}\n  db: {image: y}\n"),
        )
        .unwrap();
        assert_eq!(
            c.iter().find(|c| c.service == "web").unwrap().change,
            "update"
        );
        assert_eq!(
            c.iter().find(|c| c.service == "db").unwrap().change,
            "unchanged"
        );
    }
}
