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
}

/// A published port served by the balancer.
#[derive(Debug, Clone, Serialize)]
pub struct PortStatus {
    pub listen: String,
    pub target: u16,
    pub backends: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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
    /// Unix seconds of the last completed reconcile pass.
    pub checked_at: u64,
}

/// A stack, as `stack_status` and `stack_list` report it.
#[derive(Debug, Clone, Serialize)]
pub struct StackStatus {
    pub name: String,
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
}

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
            }),
        };
        for def in c.inner.store.load_all()? {
            eprintln!("isb serve: resuming stack {}", def.name);
            c.apply(Arc::new(def));
        }
        Ok(c)
    }

    pub fn balancer(&self) -> &Balancer {
        &self.inner.balancer
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
        let old = self.inner.stacks.lock().unwrap().get(&def.name).cloned();
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
        let name = def.name.clone();
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
                    spawn_worker(self.inner.clone(), name.clone(), svc.clone(), shared);
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
        if self.inner.stacks.lock().unwrap().remove(name).is_none() {
            return Err(Error::NotFound(format!("stack {name}")));
        }
        self.inner.store.remove(name)?;
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
            name: name.to_string(),
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
        for i in list_instances(&self.inner.client, name, Some(service))? {
            if slot.is_some_and(|s| s != i.slot) {
                continue;
            }
            let sb = Sandbox::get(&self.inner.client, &i.name)?;
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
    stack: String,
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
}

fn spawn_worker(inner: Arc<Inner>, stack: String, service: String, shared: Arc<WorkerShared>) {
    let name = format!("isb-{stack}-{service}");
    let r = std::thread::Builder::new().name(name).spawn(move || {
        let mut w = Worker {
            inner,
            stack,
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
        };
        w.run();
    });
    if let Err(e) = r {
        eprintln!("isb serve: cannot start a worker thread: {e}");
    }
}

impl Worker {
    fn log(&self, msg: &str) {
        eprintln!("isb serve: {}/{}: {msg}", self.stack, self.service);
    }

    fn client(&self) -> &Client {
        &self.inner.client
    }

    fn key(&self) -> (String, String) {
        (self.stack.clone(), self.service.clone())
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
                return;
            }
            if let Err(e) = self.pass(&def) {
                self.state = "failing".into();
                self.message = Some(e.to_string());
                self.log(&format!("{e}"));
                self.publish_status(&def, &[]);
            }
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
        self.inner.status.lock().unwrap().remove(&self.key());
        self.inner.workers.lock().unwrap().remove(&self.key());
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

        if let Some(msg) = self.waiting_for(&spec) {
            self.state = "waiting".into();
            self.message = Some(msg);
            self.publish_status(def, &[]);
            return Ok(());
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
        self.publish_status(def, &insts);

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
            self.publish_status(def, &insts);
            return Ok(());
        }
        if self.paused.as_ref().is_some_and(|(r, _)| *r == rev) {
            self.state = "paused".into();
            self.message = self.paused.as_ref().map(|(_, m)| m.clone());
            self.publish_status(def, &insts);
            return Ok(());
        }
        if let Some((at, wait)) = self.create_backoff {
            if at.elapsed() < wait {
                return Ok(());
            }
        }
        self.state = "updating".into();
        self.message = None;
        self.publish_status(def, &insts);

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

        for (n, batch) in pending.chunks(parallel).enumerate() {
            if n > 0 && !delay.is_zero() {
                std::thread::sleep(delay);
            }
            if self.superseded(def) {
                return Ok(());
            }
            for (slot, old) in batch {
                let r = self.replace(
                    def,
                    &spec,
                    &rev,
                    *slot,
                    old.as_deref(),
                    order,
                    oci,
                    probe.as_ref(),
                    monitor,
                );
                if let Err(e) = r {
                    let msg = format!("slot {slot}: {e}");
                    self.log(&msg);
                    if old.is_none() {
                        // Nothing to protect: keep trying, slower each time.
                        let wait = self
                            .create_backoff
                            .map(|(_, w)| (w * 2).min(Duration::from_secs(300)))
                            .unwrap_or(Duration::from_secs(10));
                        self.create_backoff = Some((Instant::now(), wait));
                        self.state = "failing".into();
                        self.message = Some(format!("{msg}; retrying in {wait:?}"));
                        self.publish_status(def, &[]);
                        return Ok(());
                    }
                    match uc.failure_action.unwrap_or_default() {
                        FailureAction::Continue => continue,
                        FailureAction::Pause => {
                            self.paused = Some((rev.clone(), format!("rollout paused: {msg}")));
                            return Ok(());
                        }
                        FailureAction::Rollback => {
                            self.paused = Some((rev.clone(), format!("rolled back: {msg}")));
                            let ctl = Controller {
                                inner: self.inner.clone(),
                            };
                            if let Err(e) = ctl.rollback(&self.stack) {
                                self.log(&format!("rollback failed: {e}"));
                            }
                            return Ok(());
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Unmet `depends_on`, as a message.
    fn waiting_for(&self, spec: &SandboxSpec) -> Option<String> {
        let st = self.inner.status.lock().unwrap();
        for (dep, d) in &spec.depends_on {
            let s = st.get(&(self.stack.clone(), dep.clone()));
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
            self.log(&format!("{} is {}; starting it", i.name, i.status));
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
            self.log(&format!(
                "{} is unhealthy ({}); restarting its app",
                i.name,
                self.rt[&i.name]
                    .last_probe
                    .lines()
                    .last()
                    .unwrap_or("probe failed")
            ));
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
            self.retire(o)?;
        }
        let name = instance_name(&self.stack, &self.service, slot, &new_id())?;
        self.log(&format!("slot {slot}: creating {name} (rev {rev})"));
        let mut s = instance_spec(def, &self.service, spec, slot, rev)?;
        s.name = Some(name.clone());
        let d = crate::sandbox::resolve(self.client(), &s, &def.file.volumes, &def.base_dir)?;
        let stack = self.stack.clone();
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
            self.wait_serving(def, &inst, spec, oci, probe, monitor)
        });
        if let Err(e) = result {
            self.log(&format!("{name} did not come up: {e}"));
            let _ = self.retire(&name);
            return Err(e);
        }
        if let (Some(o), UpdateOrder::StartFirst) = (old, order) {
            self.log(&format!("slot {slot}: {name} is serving; retiring {o}"));
            self.retire(o)?;
        }
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
                .map(|p| (format!("{}/{}/{}", self.stack, self.service, p.listen), p))
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
    }

    fn publish_status(&self, def: &StackDef, insts: &[Inst]) {
        let Ok(spec) = def.service(&self.service) else {
            return;
        };
        let rev = def.revision(&self.service).unwrap_or_default();
        let probe = matches!(spec.health_probe(), Ok(Some(_)));
        let instances: Vec<InstanceStatus> = insts
            .iter()
            .map(|i| {
                let rt = self.rt.get(&i.name);
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
                    status: i.status.clone(),
                    health: health.into(),
                    ip: rt.and_then(|r| r.ip).map(|ip| ip.to_string()),
                    in_rotation: rt.is_some_and(|r| r.in_rotation),
                    restarts: rt.map(|r| r.restarts.len() as u32).unwrap_or(0),
                    last_probe: rt.map(|r| r.last_probe.clone()).unwrap_or_default(),
                }
            })
            .collect();
        let routes = self.inner.balancer.routes();
        let ports = self
            .routes
            .iter()
            .map(|(k, p)| PortStatus {
                listen: p.listen.to_string(),
                target: p.target,
                backends: routes
                    .iter()
                    .find(|r| r.key == *k)
                    .map(|r| r.backends.iter().map(|b| b.addr.to_string()).collect())
                    .unwrap_or_default(),
                error: self.route_errors.get(k).cloned(),
            })
            .collect();
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
