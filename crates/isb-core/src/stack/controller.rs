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

use super::changes::diff;
use super::ports::{Published, published};
use super::secrets::{Cycle, LABEL_SECRETS, StaleSecret};
use super::{
    LABEL_REV, LABEL_SERVICE, LABEL_SLOT, LABEL_STACK, StackDef, Store, instance_name, new_id,
    now_secs, validate_stack_name,
};
use crate::balance::Balancer;
use crate::client::{Client, encode_segment};
use crate::error::{Error, Result};
use crate::org::OrgId;
use crate::plan::Desired;
use crate::sandbox::{EnsureOptions, Sandbox};
use crate::secrets::Secrets;
use crate::spec::{
    DependCondition, FailureAction, HealthProbe, OnChange, RestartCondition, RestartMode,
    SandboxSpec, UpdateConfig, UpdateOrder,
};
use crate::supervise;

mod dns;
mod health;
mod instances;
pub use dns::{DnsScope, DnsScopeFn};
use health::InstRt;
use instances::instance_state;
pub use instances::list_instances;

/// How long a replaced instance's connections may drain before it is stopped.
const DRAIN: Duration = Duration::from_secs(10);
/// How often an unhealthy app is restarted before its instance is replaced.
const RESTARTS_BEFORE_REPLACE: u32 = 3;
/// How long a service that was healthy must have no healthy replica before
/// `health.unhealthy` is raised (restarts and short blips stay quiet).
const HEALTH_DEBOUNCE: Duration = Duration::from_secs(20);

/// One replica, as `stack_status` reports it.
#[derive(Debug, Clone, Serialize, Default)]
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
    /// Root disk usage, where the storage driver reports it.
    pub disk_bytes: Option<u64>,
    /// Secrets (`on_change: none`, or a restart still to come) whose new
    /// version reached the replica's files but not its running app.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub stale_secrets: Vec<StaleSecret>,
}

/// A published port: TCP served by the balancer, UDP by a NAT proxy on the
/// replica (`listen` ends in `/udp`, `backends` is the replica).
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
    /// The service's domains as the ingress serves them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub domains: Vec<crate::ingress::DomainStatus>,
    /// While not converged: the last replica that failed to come up, without
    /// its output (`stack_logs` has that).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_failed_attempt: Option<serde_json::Value>,
}

/// Something that follows which replicas receive traffic: the ingress.
/// Called from worker threads, never with a controller lock held.
pub trait Observer: Send + Sync {
    /// The in-rotation replicas of a service changed. `stack` is qualified.
    fn rotation(&self, stack: &str, service: &str, ips: &[IpAddr]);
    /// Wait (at most `timeout`) until requests to `ip` have drained from the
    /// observer's proxies, before the replica is stopped.
    fn drain(&self, stack: &str, service: &str, ip: IpAddr, timeout: Duration);
    /// The deployed definitions changed (deploy, scale, rollback, removal).
    fn stacks_changed(&self, defs: Vec<Arc<StackDef>>);
    /// A service's domains for its status.
    fn domains(&self, stack: &str, service: &str) -> Vec<crate::ingress::DomainStatus>;
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
    /// What happened, for consumers that act on events (notifications):
    /// dotted, `<subject>.<outcome>`. In use: `deploy.succeeded`,
    /// `deploy.failed`, `health.unhealthy`, `health.recovered`,
    /// `backup.succeeded`, `backup.failed`, `restore.succeeded`,
    /// `restore.failed`, `job.succeeded`, `job.failed`, `cert.issued`,
    /// `cert.failed`, `secret.rotated` (a new secret version reached a
    /// service, saying what its `on_change` does; a workspace under stack
    /// `<org>/@workspaces`), `preview.created`, `preview.removed` (a preview's
    /// deploys are `deploy.*` under its own stack), `server.unreachable`,
    /// `server.recovered` (on a control plane, stack `<org>/@servers` for
    /// each org on the server and `system/@servers`, service = the server).
    /// Most events have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

/// Hears every event as it is emitted. It must not block (the history
/// queues and returns).
pub type EventSink = Arc<dyn Fn(&Event) + Send + Sync>;

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
    /// Set when something the service was failing on may have changed (an
    /// org's limits): the worker drops its retry backoff on its next pass.
    kick: AtomicBool,
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
    /// The output of each service's last replica that failed to come up.
    failures: super::failure::Failures,
    events: Mutex<(u64, VecDeque<Event>)>,
    snapshot: Mutex<Snapshot>,
    /// The org stores secret values are read from at delivery.
    secrets: Arc<Secrets>,
    /// Held across read-modify-save of a definition, so a version bump and
    /// a deploy never overwrite each other.
    edit: Mutex<()>,
    /// When driver-backed secrets are next checked for a new version.
    refresh: Mutex<super::secrets::RefreshSchedule>,
    observer: Option<Arc<dyn Observer>>,
    /// Where each event also goes: the persistent history.
    event_sink: Mutex<Option<EventSink>>,
    /// Where each sample also goes: the metrics history.
    metrics_sink: Mutex<Option<std::sync::mpsc::SyncSender<crate::metrics_history::Sample>>>,
    /// Which project environment a stack's services are also named in.
    dns_scope: Mutex<Option<DnsScopeFn>>,
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
        self.emit_kind(None, level, stack, service, instance, message);
    }

    fn emit_kind(
        &self,
        kind: Option<&str>,
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
            kind: kind.map(String::from),
        };
        if e.1.len() == EVENTS_KEPT {
            e.1.pop_front();
        }
        let sink = self.event_sink.lock().unwrap().clone();
        if let Some(s) = sink {
            s(&ev);
        }
        e.1.push_back(ev);
    }
}

/// How often the metrics sampler runs.
const SAMPLE_EVERY: Duration = Duration::from_secs(2);
/// The longest wait between looking for driver-backed secrets that are due.
const SECRET_TICK: Duration = Duration::from_secs(10);

/// The daemon's stack controller.
#[derive(Clone)]
pub struct Controller {
    inner: Arc<Inner>,
}

impl Controller {
    /// Load every stored stack and start reconciling it. Secret values are
    /// read from `secrets` whenever they are delivered.
    pub fn start(
        client: Client,
        store: Store,
        interval: Duration,
        secrets: Arc<Secrets>,
    ) -> Result<Controller> {
        Controller::start_with(client, store, interval, secrets, None)
    }

    /// [`Controller::start`], telling `observer` about rotation changes from
    /// the first one on.
    pub fn start_with(
        client: Client,
        store: Store,
        interval: Duration,
        secrets: Arc<Secrets>,
        observer: Option<Arc<dyn Observer>>,
    ) -> Result<Controller> {
        let c = Controller {
            inner: Arc::new(Inner {
                client,
                store,
                balancer: Balancer::new(),
                interval,
                workers: Mutex::new(BTreeMap::new()),
                stacks: Mutex::new(BTreeMap::new()),
                status: Mutex::new(BTreeMap::new()),
                failures: Default::default(),
                events: Mutex::new((0, VecDeque::new())),
                snapshot: Mutex::new(Snapshot::default()),
                secrets,
                edit: Mutex::new(()),
                refresh: Mutex::new(Default::default()),
                observer,
                event_sink: Mutex::new(None),
                metrics_sink: Mutex::new(None),
                dns_scope: Mutex::new(None),
            }),
        };
        // Driver-backed secrets are polled on their refresh intervals; the
        // tick only decides which are due.
        let weak = Arc::downgrade(&c.inner);
        let _ = std::thread::Builder::new()
            .name("isb-secrets".into())
            .spawn(move || {
                while let Some(inner) = weak.upgrade() {
                    let tick = inner.interval.clamp(Duration::from_secs(1), SECRET_TICK);
                    Controller { inner }.check_due_secrets();
                    std::thread::sleep(tick);
                }
            });
        // A weak handle, so the sampler ends with the controller.
        let weak = Arc::downgrade(&c.inner);
        let _ = std::thread::Builder::new()
            .name("isb-metrics".into())
            .spawn(move || {
                let mut sampler = crate::metrics::Sampler::new();
                let mut orgs = crate::metrics_history::OrgSet::default();
                while let Some(inner) = weak.upgrade() {
                    match sampler.sample(&inner.client) {
                        Ok((host, insts)) => {
                            if let Some(tx) = &*inner.metrics_sink.lock().unwrap() {
                                let s = crate::metrics_history::Sample {
                                    at: now_ms(),
                                    orgs: orgs.current(&inner.client, &insts),
                                    orgs_at: orgs.read_at(),
                                    instances: insts.clone(),
                                };
                                crate::metrics_history::offer(tx, s);
                            }
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
        dns::prune(&defs);
        for def in defs {
            eprintln!("isb serve: resuming stack {}", def.name);
            c.apply(Arc::new(def));
        }
        // A vault may have moved on while the daemon was down.
        let all: Vec<(String, String)> = c
            .definitions()
            .iter()
            .flat_map(|def| {
                let q = def.qualified();
                def.secrets.keys().map(move |k| (q.clone(), k.clone()))
            })
            .collect();
        c.poll(all);
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

    /// A client's resume cursor for [`Self::events`]: one past the newest
    /// event came from an earlier process (numbering restarts with the
    /// daemon), so the client starts over from what is kept instead of
    /// waiting for this feed to count back up to it.
    pub fn resume_from(&self, since: u64) -> u64 {
        if since > self.inner.events.lock().unwrap().0 {
            0
        } else {
            since
        }
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

    /// Hand every event to `sink` too (the persistent history), starting
    /// with the ones already kept, so none emitted before it was set is
    /// missed and none is handed over twice.
    pub fn set_event_sink(&self, sink: EventSink) {
        let e = self.inner.events.lock().unwrap();
        for ev in &e.1 {
            sink(ev);
        }
        *self.inner.event_sink.lock().unwrap() = Some(sink);
    }

    /// Send every metrics sample to `tx` too (the metrics history).
    pub fn set_metrics_sink(
        &self,
        tx: std::sync::mpsc::SyncSender<crate::metrics_history::Sample>,
    ) {
        *self.inner.metrics_sink.lock().unwrap() = Some(tx);
    }

    /// The latest metrics sample.
    pub fn snapshot(&self) -> Snapshot {
        self.inner.snapshot.lock().unwrap().clone()
    }

    /// Record an event from outside a worker (a deploy, a removal).
    /// Record an event of a known kind ([`Event::kind`]) about a stack, or
    /// one of its services when `service` is not empty.
    pub fn event(&self, kind: &str, level: &str, stack: &str, service: &str, message: String) {
        eprintln!("isb serve: {stack}: {message}");
        self.inner
            .emit_kind(Some(kind), level, stack, service, None, message);
    }

    /// Re-emit an event another daemon recorded (a control plane mirroring
    /// a server's feed), under this feed's numbering.
    pub fn relay(
        &self,
        kind: Option<&str>,
        level: &str,
        stack: &str,
        service: &str,
        instance: Option<&str>,
        message: String,
    ) {
        self.inner
            .emit_kind(kind, level, stack, service, instance, message);
    }

    pub fn note(&self, level: &str, stack: &str, message: String) {
        eprintln!("isb serve: {stack}: {message}");
        self.inner.emit(level, stack, "", None, message);
    }

    /// Record an event about one service of a stack (an app's deployment
    /// log, say). Not echoed to stderr: it can be chatty.
    pub fn note_service(&self, level: &str, stack: &str, service: &str, message: String) {
        self.inner.emit(level, stack, service, None, message);
    }

    /// Record an event about one service (`stack` qualified).
    pub fn service_event(&self, level: &str, stack: &str, service: &str, message: String) {
        eprintln!("isb serve: {stack}/{service}: {message}");
        self.inner.emit(level, stack, service, None, message);
    }

    fn notify_stacks(&self) {
        if let Some(o) = &self.inner.observer {
            o.stacks_changed(self.definitions());
        }
    }

    /// What deploying `def` would change, without deploying it.
    pub fn plan(&self, def: &StackDef) -> Result<Vec<DeployChange>> {
        self.validate(def)?;
        let mut def = def.clone();
        self.pin_images(&mut def)?;
        let def = &def;
        let old = self
            .inner
            .stacks
            .lock()
            .unwrap()
            .get(&def.qualified())
            .cloned();
        diff(old.as_deref(), def)
    }

    pub fn client(&self) -> &Client {
        &self.inner.client
    }

    /// Check a stack definition against this host without deploying it:
    /// every service must resolve (image source, paths, ports) and name its widest slot.
    pub fn validate(&self, def: &StackDef) -> Result<()> {
        validate_stack_name(&def.name)?;
        // In the org's project, which is what `registry:` images resolve in.
        let host = crate::sandbox::host_facts(&crate::org::client(&self.inner.client, &def.org))?;
        for (svc, spec) in &def.file.services {
            let mut s = instance_spec(def, svc, spec, 1, "0000")?;
            s.name = Some(instance_name(&def.name, svc, 1, "0000")?);
            instance_name(&def.name, svc, spec.replicas().max(1), "0000")?;
            crate::plan::resolve(&s, &def.file.volumes, &host, &def.base_dir)?;
            published(spec)?;
            crate::ingress::domain::validate(svc, &spec.domains)?;
        }
        super::ports::validate_udp(&self.inner.client, def, &self.definitions())
    }

    /// Deploy (or update) a stack. Returns what will change; the rollout
    /// itself happens in the background.
    pub fn deploy(&self, mut def: StackDef) -> Result<Vec<DeployChange>> {
        self.validate(&def)?;
        self.pin_images(&mut def)?;
        let _g = self.inner.edit.lock().unwrap();
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
                    // A new deployment starts from a clean slate: the
                    // failure the service reported before it is not the
                    // deployment's, and a waiter must not read it as such.
                    if let Some(st) = self.inner.status.lock().unwrap().get_mut(&key) {
                        if st.state == "failing" {
                            st.state = "updating".into();
                            st.message = None;
                        }
                    }
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
                        kick: AtomicBool::new(false),
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
        drop(workers);
        self.notify_stacks();
    }

    /// An org's limits changed: services of the org that are failing on a
    /// limit (a quota refusal from incus) retry now instead of after their
    /// backoff. Returns how many were woken.
    pub fn org_limits_changed(&self, org: &OrgId) -> usize {
        let stacks = self.inner.stacks.lock().unwrap();
        let workers = self.inner.workers.lock().unwrap();
        let status = self.inner.status.lock().unwrap();
        let mut n = 0;
        for (key, w) in workers.iter() {
            let ours = stacks.get(&key.0).is_some_and(|d| d.org == *org);
            let limited = status.get(key).is_some_and(|s| {
                s.state == "failing" && s.message.as_deref().is_some_and(limit_error)
            });
            if ours && limited {
                w.kick.store(true, Ordering::SeqCst);
                // Under the slot lock, the worker is either waiting (and
                // wakes) or yet to look at the flag.
                let _slot = w.slot.lock().unwrap();
                w.wake.notify_all();
                n += 1;
            }
        }
        n
    }

    /// Remove a stack: every instance and published port; with `volumes`,
    /// its named volumes too. Returns once the workers have cleaned up (or
    /// after `timeout`).
    pub fn remove(&self, name: &str, volumes: bool, timeout: Duration) -> Result<()> {
        {
            let _g = self.inner.edit.lock().unwrap();
            let Some(def) = self.inner.stacks.lock().unwrap().remove(name) else {
                return Err(Error::NotFound(format!("stack {name}")));
            };
            self.inner.store.remove(&def.org, &def.name)?;
        }
        self.notify_stacks();
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
        let _g = self.inner.edit.lock().unwrap();
        let cur = self.get_def(name)?;
        let prev = cur
            .previous
            .clone()
            .ok_or_else(|| Error::invalid(format!("stack {name} has no previous deployment")))?;
        let mut def = *prev;
        def.deployed_at = now_secs();
        // The store keeps only each secret's current value: that is what a
        // rollback delivers, under its current version.
        for b in def.secrets.values_mut() {
            if let Ok(v) = self.inner.secrets.version_in(&b.driver, &def.org, &b.name) {
                b.version = v;
            }
        }
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
        let _g = self.inner.edit.lock().unwrap();
        let cur = self.get_def(name)?;
        let mut def = (*cur).clone();
        let spec = def
            .file
            .services
            .get_mut(service)
            .ok_or_else(|| Error::NotFound(format!("service {service} in stack {name}")))?;
        spec.deploy.get_or_insert_with(Default::default).replicas = Some(replicas);
        super::ports::check_replicas(service, spec)?;
        // `name` is the controller key (`org/stack`); instances use the bare name.
        instance_name(&def.name, service, replicas.max(1), "0000")?;
        self.inner.store.save(&def)?;
        self.apply(Arc::new(def));
        Ok(())
    }

    /// Replace every instance of a service even though its spec is the same
    /// (`docker service update --force`): picks up a moved image tag or a
    /// changed bind-mounted file.
    pub fn redeploy(&self, name: &str, service: &str) -> Result<()> {
        let _g = self.inner.edit.lock().unwrap();
        let cur = self.get_def(name)?;
        cur.service(service)?;
        let mut def = (*cur).clone();
        *def.force.entry(service.to_string()).or_insert(0) += 1;
        // A moved tag is what a redeploy is usually for.
        self.pin_images(&mut def)?;
        self.inner.store.save(&def)?;
        self.apply(Arc::new(def));
        Ok(())
    }

    /// Resolve every `registry:` image given by tag to the digest the tag
    /// names now, in the stack's org (a digest in the file only has to
    /// exist there).
    fn pin_images(&self, def: &mut StackDef) -> Result<()> {
        def.images.clear();
        for (svc, spec) in &def.file.services {
            let Some(r) = spec.image.strip_prefix("registry:") else {
                continue;
            };
            let r = crate::registry::ImageRef::parse(r)?;
            let reg = crate::registry::Registry::shared(&self.inner.client)?;
            // A digest is checked too, so a typo (or another org's digest)
            // fails the deploy rather than the rollout.
            let d = reg.resolve(&def.org, &r)?;
            if r.digest.is_none() {
                def.images.insert(svc.clone(), d);
            }
        }
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
        drop(st);
        let mut services = services;
        if let Some(o) = &self.inner.observer {
            for s in &mut services {
                s.domains = o.domains(name, &s.service);
            }
        }
        for s in services.iter_mut().filter(|s| s.state != "converged") {
            s.last_failed_attempt = self
                .inner
                .failures
                .last(name, &s.service)
                .map(|f| f.summary());
        }
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
        let oci = crate::plan::ImageSource::parse(&def.service(service)?.image)?.is_oci();
        let oc = crate::org::client(&self.inner.client, &def.org);
        super::failure::replica_logs(&oc, &def.name, service, oci, slot, lines)
    }

    /// The last replica of a service that failed to come up, with its
    /// output, while the service is not converged (or `always`): after the
    /// instance is deleted, this is all that is left to read.
    pub fn last_failure(
        &self,
        name: &str,
        service: &str,
        always: bool,
    ) -> Option<super::failure::FailedAttempt> {
        let state = self.inner.status.lock().unwrap();
        let converged = state
            .get(&(name.to_string(), service.to_string()))
            .is_some_and(|s| s.state == "converged");
        drop(state);
        if converged && !always {
            return None;
        }
        self.inner.failures.last(name, service)
    }

    /// The last failed attempt of each of a stack's services, converged or not.
    pub fn failed_attempts(&self, name: &str) -> BTreeMap<String, super::failure::FailedAttempt> {
        self.inner.failures.of_stack(name)
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

/// Whether a failure message is an incus project-limit refusal (see
/// `org::limits`): one that raising the org's limits can fix.
fn limit_error(msg: &str) -> bool {
    msg.contains(" quota (") || msg.contains(" limit (")
}

/// The spec an instance of `service` is created from: labelled, with its
/// published TCP ports removed (the balancer serves them) and its UDP ports
/// as NAT proxies (see [`super::ports`]), and always long-running, as swarm
/// ignores `restart` in favour of `restart_policy`.
fn instance_spec(
    def: &StackDef,
    service: &str,
    spec: &SandboxSpec,
    slot: u32,
    rev: &str,
) -> Result<SandboxSpec> {
    let mut s = spec.clone();
    s.image = def.instance_image(service, &spec.image);
    s.restart = Some(RestartMode::Always);
    (s.ports, s.stack_udp) = super::ports::instance_ports(spec)?;
    s.domains.clear();
    if let Some(d) = &s.deploy {
        s.labels.extend(d.labels.clone());
    }
    s.labels.insert(LABEL_STACK.into(), def.name.clone());
    s.labels.insert(LABEL_SERVICE.into(), service.into());
    s.labels.insert(LABEL_SLOT.into(), slot.to_string());
    s.labels.insert(LABEL_REV.into(), rev.into());
    let live = live_versions(def, service);
    if !live.is_empty() {
        s.labels
            .insert(LABEL_SECRETS.into(), super::secrets::versions_label(&live));
    }
    Ok(s)
}

/// The versions bound now of the secrets `service` takes in place.
fn live_versions(def: &StackDef, service: &str) -> BTreeMap<String, u64> {
    def.live_secrets(service)
        .into_iter()
        .map(|(k, (_, v))| (k, v))
        .collect()
}

/// A stack's instance as listed.
#[derive(Debug, Clone)]
#[doc(hidden)]
pub struct Inst {
    pub name: String,
    pub(super) slot: u32,
    pub rev: String,
    status: String,
    /// [`LABEL_SECRETS`]: the `restart`/`none` secret versions its app last
    /// started with; `None` on an instance from before the label.
    secrets: Option<BTreeMap<String, u64>>,
}

impl Inst {
    #[doc(hidden)]
    pub fn is_running(&self) -> bool {
        self.running()
    }

    fn running(&self) -> bool {
        self.status.eq_ignore_ascii_case("running")
    }
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
    /// The addresses last published as the service's name, and the
    /// project environment it was also named in.
    dns_last: Option<(Vec<IpAddr>, Option<String>)>,
    dns_error: Option<String>,
    /// The addresses last told to the observer.
    observed: Option<Vec<IpAddr>>,
    /// The service had a healthy replica at some point in this run.
    ever_healthy: bool,
    /// Since when no replica has been healthy.
    health_down: Option<Instant>,
    /// `health.unhealthy` was raised and not yet answered by `recovered`.
    health_alarm: bool,
    /// The definition the last pass ran on.
    seen: Option<Arc<StackDef>>,
    /// The secret versions whose in-place restart failed, and why: not
    /// tried again on the other replicas until they move.
    restart_failed: Option<(BTreeMap<String, u64>, String)>,
}

fn spawn_worker(inner: Arc<Inner>, def: &StackDef, service: String, shared: Arc<WorkerShared>) {
    let (stack, q, org) = (def.name.clone(), def.qualified(), def.org.clone());
    let oclient = crate::org::client(&inner.client, &def.org);
    let name = format!("isb-{q}-{service}");
    let r = std::thread::Builder::new().name(name).spawn(move || {
        let mut w = Worker::new(inner, stack, q, oclient, service, shared, org);
        w.run();
    });
    if let Err(e) = r {
        eprintln!("isb serve: cannot start a worker thread: {e}");
    }
}

impl Worker {
    fn new(
        inner: Arc<Inner>,
        stack: String,
        q: String,
        oclient: Client,
        service: String,
        shared: Arc<WorkerShared>,
        org: OrgId,
    ) -> Worker {
        Worker {
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
            observed: None,
            ever_healthy: false,
            health_down: None,
            health_alarm: false,
            seen: None,
            restart_failed: None,
        }
    }

    /// Drop the retry backoff when the instructions changed (a new
    /// deployment) or were kicked (an org's limits changed), so the next
    /// pass makes a fresh attempt, and forget the failure the old
    /// instructions ended in.
    fn begin_pass(&mut self, def: &Arc<StackDef>) {
        let new_def = self.seen.as_ref().is_none_or(|d| !Arc::ptr_eq(d, def));
        let kicked = self.shared.kick.swap(false, Ordering::SeqCst);
        if new_def {
            self.seen = Some(def.clone());
            self.last_error = None;
            if self.state == "failing" {
                self.state = "updating".into();
                self.message = None;
            }
        }
        if new_def || kicked {
            self.create_backoff = None;
        }
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

    /// Raise `health.unhealthy` once a service that was healthy has had no
    /// healthy replica for [`HEALTH_DEBOUNCE`] (not while a rollout runs:
    /// it reports its own failure), and `health.recovered` when one is back.
    fn watch_health(&mut self, healthy: u32, replicas: u32) {
        if healthy > 0 {
            self.ever_healthy = true;
            self.health_down = None;
            if self.health_alarm {
                self.health_alarm = false;
                let msg = format!("{healthy} of {replicas} replicas healthy again");
                eprintln!("isb serve: {}/{}: {msg}", self.q, self.service);
                self.inner.emit_kind(
                    Some("health.recovered"),
                    "info",
                    &self.q,
                    &self.service,
                    None,
                    msg,
                );
            }
            return;
        }
        if replicas == 0 || !self.ever_healthy || self.rollout.is_some() {
            self.health_down = None;
            return;
        }
        let since = *self.health_down.get_or_insert_with(Instant::now);
        if !self.health_alarm && since.elapsed() >= HEALTH_DEBOUNCE {
            self.health_alarm = true;
            let msg = format!(
                "no healthy replica (of {replicas}) for {}s",
                since.elapsed().as_secs()
            );
            eprintln!("isb serve: {}/{}: {msg}", self.q, self.service);
            self.inner.emit_kind(
                Some("health.unhealthy"),
                "error",
                &self.q,
                &self.service,
                None,
                msg,
            );
        }
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
            self.begin_pass(&def);
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
        self.unpublish_dns();
        if let Some(o) = &self.inner.observer {
            o.rotation(&self.q, &self.service, &[]);
        }
        self.observed = Some(Vec::new());
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
    #[expect(
        clippy::too_many_lines,
        reason = "predates the lint ratchet; split it when next changed"
    )]
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
            // New versions of secrets the service takes in place.
            let uc = spec
                .deploy
                .as_ref()
                .and_then(|d| d.update_config.clone())
                .unwrap_or_default();
            self.cycle_in_place(def, &spec, &uc)?;
            let insts = self.insts.clone();
            let all_ok = insts
                .iter()
                .all(|i| i.rev == rev && self.rt.get(&i.name).is_some_and(|r| r.in_rotation));
            self.state = if all_ok { "converged" } else { "failing" }.into();
            if all_ok {
                self.message = self.restart_failed.as_ref().map(|(_, m)| m.clone());
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
    #[expect(clippy::too_many_arguments)]
    #[expect(
        clippy::excessive_nesting,
        reason = "predates the lint ratchet; split it when next changed"
    )]
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
                        let prev = self.create_backoff.map(|(_, w)| w);
                        let (wait, m) = super::failure::retry(prev, &spec.image, &msg, &e);
                        self.create_backoff = Some((Instant::now(), wait));
                        self.state = "failing".into();
                        self.message = Some(m);
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
    #[expect(
        clippy::too_many_lines,
        reason = "predates the lint ratchet; split it when next changed"
    )]
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
            let restarted = rt.pid != 0;
            rt.pid = pid;
            rt.started(Instant::now());
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
            // Seen restarting: its app now runs what setup delivered.
            if restarted {
                self.mark_started(def, &i.name)?;
            }
        }
        let alive = self.alive(&sb, spec, oci);
        let healthy = match probe {
            None => alive,
            Some(p) => {
                let rt = self.rt.get_mut(&i.name).unwrap();
                if rt.since.is_none() {
                    rt.since = Some(Instant::now());
                }
                if rt.next_probe.is_none_or(|t| Instant::now() >= t) {
                    let r = supervise::probe(&sb, p);
                    let rt = self.rt.get_mut(&i.name).unwrap();
                    rt.last_probe = r.output.clone();
                    rt.record_probe(r.ok, p, Instant::now());
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
            rt.started(Instant::now());
            supervise::restart_app(&sb, &self.service, oci)?;
            if !oci {
                // The unit restarts within the same instance; it reads the
                // files and variables delivered last.
                self.mark_started(def, &i.name)?;
            }
        }
        Ok(())
    }

    /// Secrets and the app's unit, after a boot or on creation.
    fn setup(&self, def: &StackDef, sb: &Sandbox, spec: &SandboxSpec, oci: bool) -> Result<()> {
        // Read from the store now, never from the definition.
        let keys = spec.secret_keys();
        let values = if keys.is_empty() {
            BTreeMap::new()
        } else {
            super::secrets::values(&self.inner.secrets, &def.org, &def.secrets, keys)?
        };
        // A new value for a secret the service takes with `on_change: none`
        // is delivered without restarting the app.
        let none = |k: &str| def.on_change(&self.service, k) == OnChange::None;
        // An OCI app started before its files arrived: restart it once so
        // it reads them (the next pass finds them in place).
        let pushed = supervise::push_secrets_detailed(sb, spec, &values)?;
        if oci && (pushed.missing || pushed.changed.iter().any(|k| !none(k))) {
            supervise::restart_app(sb, &self.service, oci)?;
        }
        if spec.command.is_some() && !oci {
            let mut s = spec.clone();
            s.restart = Some(RestartMode::Always);
            let env = supervise::secret_env(spec, &values)?;
            // Only a change all of whose variables are `none` secrets is
            // left for the next start.
            let env_restarts =
                spec.env.secrets.is_empty() || spec.env.secrets.values().any(|k| !none(k));
            supervise::install_with(
                sb,
                &self.service,
                &s,
                spec.has_secret_files(),
                &env,
                env_restarts,
            )?;
        }
        Ok(())
    }

    /// An OCI instance's secret variables, for its config (`environment.KEY`).
    fn oci_secret_env(
        &self,
        def: &StackDef,
        spec: &SandboxSpec,
    ) -> Result<BTreeMap<String, String>> {
        if spec.env.secrets.is_empty() {
            return Ok(BTreeMap::new());
        }
        let values = super::secrets::values(
            &self.inner.secrets,
            &def.org,
            &def.secrets,
            spec.env.secrets.values().map(String::as_str),
        )?;
        supervise::secret_env(spec, &values)
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
    #[expect(clippy::too_many_arguments)]
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
        // Before anything is stopped: a secret that cannot be read leaves
        // the old instance serving.
        let secret_env = if oci {
            self.oci_secret_env(def, spec)?
        } else {
            BTreeMap::new()
        };
        // An OCI app reads its files as it starts: write them into the new
        // instance before its first start, rather than restarting it after.
        let before_start = if oci && spec.has_secret_files() {
            let values = super::secrets::values(
                &self.inner.secrets,
                &def.org,
                &def.secrets,
                spec.secret_keys(),
            )?;
            let spec = spec.clone();
            Some(crate::plan::BeforeStart(Arc::new(move |c, n| {
                supervise::push_secret_files(c, n, &spec, &values).map(|_| ())
            })))
        } else {
            None
        };
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
        // `env.secrets` stays set, so the values are redacted in reports.
        s.env.vars.extend(secret_env);
        let mut d = crate::sandbox::resolve(self.client(), &s, &def.file.volumes, &def.base_dir)?;
        d.before_start = before_start;
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
                secrets: Some(live_versions(def, &self.service)),
            };
            self.insts.push(inst.clone());
            self.slot_state(def, slot, None, None, Some("probing"));
            self.wait_serving(def, &inst, spec, oci, probe, monitor)
        });
        if let Err(e) = result {
            // Read its output before it is deleted: it explains the failure.
            let (e, attempt) =
                super::failure::explain(self.client(), &name, &self.service, oci, e, now_ms());
            self.inner.failures.record(&self.q, &self.service, attempt);
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
        // Through the startup grace (a starting replica is not failed), then
        // `retries` counted failures, then a margin.
        let deadline = match probe {
            Some(p) => {
                p.startup_grace.max(p.start_period)
                    + p.interval * p.retries
                    + Duration::from_secs(30)
            }
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
        self.drain(name);
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

    /// Take an instance out of rotation and let its connections drain (up
    /// to [`DRAIN`]).
    fn drain(&mut self, name: &str) {
        let ip = self.rt.get(name).and_then(|r| r.ip);
        self.set_rotation(name, false);
        self.sync_routes();
        if let Some(ip) = ip {
            let started = Instant::now();
            if let Some(o) = &self.inner.observer {
                o.drain(&self.q, &self.service, ip, DRAIN);
            }
            let left = DRAIN.saturating_sub(started.elapsed());
            for (k, p) in &self.routes {
                self.inner
                    .balancer
                    .wait_drained(k, SocketAddr::new(ip, p.target), left);
            }
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
                .map(|p| (format!("{}/{}/{}", self.q, self.service, p.display()), p))
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
        // UDP ports are proxy devices on the replica (see super::ports).
        for (k, p) in self.routes.iter().filter(|(_, p)| !p.udp) {
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
        self.sync_observer();
        self.sync_dns();
    }

    /// Tell the observer (the ingress) when the in-rotation set changed.
    fn sync_observer(&mut self) {
        let Some(o) = self.inner.observer.clone() else {
            return;
        };
        let mut ips: Vec<IpAddr> = self
            .rt
            .values()
            .filter(|r| r.in_rotation)
            .filter_map(|r| r.ip)
            .collect();
        ips.sort();
        if self.observed.as_ref() != Some(&ips) {
            o.rotation(&self.q, &self.service, &ips);
            self.observed = Some(ips);
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "predates the lint ratchet; split it when next changed"
    )]
    fn publish_status(&mut self, def: &StackDef) {
        // An address can change without a rotation change (a restart).
        self.sync_observer();
        self.sync_dns();
        let Ok(spec) = def.service(&self.service) else {
            return;
        };
        let rev = def.revision(&self.service).unwrap_or_default();
        let probe = matches!(spec.health_probe(), Ok(Some(_)));
        let live = live_versions(def, &self.service);
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
                    disk_bytes: m.and_then(|m| m.disk_bytes),
                    stale_secrets: i
                        .secrets
                        .as_ref()
                        .map(|have| super::secrets::stale(have, &live))
                        .unwrap_or_default(),
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
                listen: p.display(),
                target: p.target,
                backends: match r {
                    _ if p.udp => {
                        let ips = self.rt.values().filter_map(|r| r.ip);
                        ips.map(|ip| SocketAddr::new(ip, p.target).to_string())
                            .collect()
                    }
                    Some(r) => r.backends.iter().map(|b| b.addr.to_string()).collect(),
                    None => Vec::new(),
                },
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
        self.watch_health(healthy, spec.replicas());
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
            domains: Vec::new(),
            last_failed_attempt: None,
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

#[path = "rotation.rs"]
mod rotation;

#[cfg(test)]
#[path = "controller_tests.rs"]
mod tests;
