//! The monitoring service of a daemon: definitions, the scheduler, the
//! workers that run checks, and the events they raise.
//!
//! One scheduler thread wakes every second and hands due checks to a fixed
//! pool of workers through a bounded queue ([`WORKERS`], [`QUEUE`]), so a
//! hundred slow targets never start a hundred threads, and a check never
//! runs twice at once. A monitor's first check comes at a phase spread by
//! its name, later ones every interval plus a little jitter, so monitors
//! created together do not fire together.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::state::{Notify, State, Status, Thresholds};
use super::store::{Check, Db};
use super::target::{self, Ctx, Outcome};
use super::{AUTO_PREFIX, Kind, MAX_PER_ORG, Monitor, Settings, dir, human, read_json, write_json};
use crate::app::Apps;
use crate::error::{Error, Result};
use crate::org::OrgId;
use crate::secrets::Secrets;
use crate::stack::controller::now_ms;

/// Checks running at once, at most.
pub const WORKERS: usize = 8;
/// Due checks waiting for a worker, at most; more wait for the next tick.
pub const QUEUE: usize = 256;
/// How often apps are looked at for their own monitors.
const AUTO_SYNC_MS: u64 = 60_000;
/// How often the history is rolled up and pruned.
const ROLLUP_MS: u64 = 300_000;
/// Event details kept for the notifier to pick up.
const DETAILS_KEPT: usize = 256;

/// Is the address policy relaxed (the platform's private-targets setting)?
pub type AllowPrivate = Arc<dyn Fn() -> bool + Send + Sync>;

/// A monitor's state as kept: the state machine and its last check.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Stored {
    pub state: State,
    pub last: Option<Outcome>,
}

#[derive(Debug, Clone, Copy, Default)]
struct Slot {
    next: u64,
    running: bool,
}

struct Job {
    org: OrgId,
    monitor: Monitor,
}

type DetailKey = (OrgId, String, String);

struct Inner {
    state: PathBuf,
    apps: Apps,
    secrets: Arc<Secrets>,
    allow_private: AllowPrivate,
    public_url: Option<String>,
    /// Held across read-modify-write of an org's files.
    edit: Mutex<()>,
    dbs: Mutex<BTreeMap<OrgId, Arc<Mutex<Db>>>>,
    slots: Mutex<BTreeMap<(OrgId, String), Slot>>,
    details: Mutex<VecDeque<(DetailKey, Value)>>,
    stop: Arc<AtomicBool>,
    tls: Arc<rustls::ClientConfig>,
}

/// The monitoring service.
#[derive(Clone)]
pub struct Monitors {
    inner: Arc<Inner>,
}

fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// A little randomness for the schedule, without a crate.
fn jitter(max_ms: u64) -> i64 {
    if max_ms == 0 {
        return 0;
    }
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let r = fnv(&n.to_string()) % (2 * max_ms + 1);
    r as i64 - max_ms as i64
}

impl Monitors {
    pub fn new(
        state: &Path,
        apps: Apps,
        secrets: Arc<Secrets>,
        allow_private: AllowPrivate,
        public_url: Option<String>,
    ) -> Monitors {
        Monitors {
            inner: Arc::new(Inner {
                state: state.to_path_buf(),
                apps,
                secrets,
                allow_private,
                public_url: public_url.map(|u| u.trim_end_matches('/').to_string()),
                edit: Mutex::new(()),
                dbs: Mutex::new(BTreeMap::new()),
                slots: Mutex::new(BTreeMap::new()),
                details: Mutex::new(VecDeque::new()),
                stop: Arc::new(AtomicBool::new(false)),
                tls: crate::net::default_tls(),
            }),
        }
    }

    /// Trust these TLS roots instead of the public set (tests).
    pub fn with_tls(mut self, tls: Arc<rustls::ClientConfig>) -> Monitors {
        if let Some(i) = Arc::get_mut(&mut self.inner) {
            i.tls = tls;
        }
        self
    }

    fn path(&self, org: &OrgId, file: &str) -> PathBuf {
        dir(&self.inner.state, org).join(file)
    }

    pub(crate) fn db(&self, org: &OrgId) -> Result<Arc<Mutex<Db>>> {
        let mut dbs = self.inner.dbs.lock().unwrap();
        if let Some(d) = dbs.get(org) {
            return Ok(d.clone());
        }
        let d = Arc::new(Mutex::new(Db::open(&self.path(org, "monitors.db"))?));
        dbs.insert(org.clone(), d.clone());
        Ok(d)
    }

    /// The orgs with state on this daemon.
    pub fn orgs(&self) -> Vec<OrgId> {
        let mut out = vec![OrgId::default_org()];
        if let Ok(rd) = std::fs::read_dir(self.inner.state.join("orgs")) {
            for e in rd.flatten() {
                if let Some(o) = e.file_name().to_str().and_then(|n| OrgId::new(n).ok()) {
                    if !out.contains(&o) {
                        out.push(o);
                    }
                }
            }
        }
        out
    }

    // ---- definitions ----

    pub fn list(&self, org: &OrgId) -> Result<Vec<Monitor>> {
        read_json(&self.path(org, "monitors.json"))
    }

    fn save(&self, org: &OrgId, all: &[Monitor]) -> Result<()> {
        write_json(&self.path(org, "monitors.json"), &all)
    }

    pub fn get(&self, org: &OrgId, name: &str) -> Result<Monitor> {
        self.list(org)?
            .into_iter()
            .find(|m| m.name == name)
            .ok_or_else(|| Error::NotFound(format!("monitor {name}")))
    }

    pub fn settings(&self, org: &OrgId) -> Result<Settings> {
        read_json(&self.path(org, "settings.json"))
    }

    pub fn set_settings(&self, org: &OrgId, s: &Settings) -> Result<()> {
        for a in &s.exclude_apps {
            crate::app::validate_app_name(a)?;
        }
        let _g = self.inner.edit.lock().unwrap();
        write_json(&self.path(org, "settings.json"), s)
    }

    /// What a monitor needs to exist: its app and its secrets.
    fn check_refs(&self, org: &OrgId, m: &Monitor) -> Result<()> {
        if m.kind == Kind::App {
            let a = m.app.as_deref().unwrap_or_default();
            self.inner.apps.get(org, a)?;
        }
        for s in m.secrets() {
            self.inner.secrets.inspect(org, s).map_err(|e| {
                if e.is_not_found() {
                    Error::invalid(format!("no secret {s} in org {org}: create it first"))
                } else {
                    e
                }
            })?;
        }
        Ok(())
    }

    pub fn create(&self, org: &OrgId, mut m: Monitor) -> Result<Monitor> {
        m.validate()?;
        self.check_refs(org, &m)?;
        let _g = self.inner.edit.lock().unwrap();
        let mut all = self.list(org)?;
        if all.iter().any(|x| x.name == m.name) {
            return Err(Error::invalid(format!("monitor {} exists", m.name)));
        }
        if all.len() >= MAX_PER_ORG {
            return Err(Error::invalid(format!(
                "at most {MAX_PER_ORG} monitors per org"
            )));
        }
        let now = now_ms() / 1000;
        (m.created_at, m.updated_at) = (now, now);
        all.push(m.clone());
        self.save(org, &all)?;
        self.due_soon(org, &m.name);
        Ok(m)
    }

    /// Change fields of a monitor: `patch` holds the fields to set (null
    /// puts a field back to its default). The name stays.
    pub fn update(
        &self,
        org: &OrgId,
        name: &str,
        patch: serde_json::Map<String, Value>,
    ) -> Result<Monitor> {
        if patch.get("name").is_some_and(|n| n.as_str() != Some(name)) {
            return Err(Error::invalid(
                "a monitor's name cannot change; create another",
            ));
        }
        let _g = self.inner.edit.lock().unwrap();
        let mut all = self.list(org)?;
        let cur = all
            .iter_mut()
            .find(|m| m.name == name)
            .ok_or_else(|| Error::NotFound(format!("monitor {name}")))?;
        let mut v = serde_json::to_value(&*cur)?;
        let o = v.as_object_mut().expect("a monitor is an object");
        for (k, x) in patch {
            if matches!(k.as_str(), "created_at" | "updated_at" | "auto") {
                continue;
            }
            if x.is_null() {
                o.remove(&k);
            } else {
                o.insert(k, x);
            }
        }
        let mut n: Monitor =
            serde_json::from_value(v).map_err(|e| Error::invalid(format!("bad monitor: {e}")))?;
        n.validate()?;
        self.check_refs(org, &n)?;
        n.updated_at = now_ms() / 1000;
        *cur = n.clone();
        self.save(org, &all)?;
        drop(_g);
        self.edit_state(org, name, State::reset_counts)?;
        self.due_soon(org, name);
        Ok(n)
    }

    /// Remove a monitor and its history. An app's own monitor stays away:
    /// the app joins the org's exclusions.
    pub fn delete(&self, org: &OrgId, name: &str) -> Result<Monitor> {
        let _g = self.inner.edit.lock().unwrap();
        let mut all = self.list(org)?;
        let i = all
            .iter()
            .position(|m| m.name == name)
            .ok_or_else(|| Error::NotFound(format!("monitor {name}")))?;
        let m = all.remove(i);
        self.save(org, &all)?;
        if m.auto {
            let mut s = self.settings(org)?;
            let app = m.app.clone().unwrap_or_default();
            if !s.exclude_apps.contains(&app) {
                s.exclude_apps.push(app);
                write_json(&self.path(org, "settings.json"), &s)?;
            }
        }
        drop(_g);
        self.db(org)?.lock().unwrap().forget(name)?;
        self.inner
            .slots
            .lock()
            .unwrap()
            .remove(&(org.clone(), name.to_string()));
        Ok(m)
    }

    pub fn set_paused(&self, org: &OrgId, name: &str, paused: bool) -> Result<Monitor> {
        let mut p = serde_json::Map::new();
        p.insert("paused".into(), json!(paused));
        self.update(org, name, p)
    }

    fn edit_state(&self, org: &OrgId, name: &str, f: impl FnOnce(&mut State)) -> Result<()> {
        let db = self.db(org)?;
        let db = db.lock().unwrap();
        let mut s: Stored = db.load_state(name)?.unwrap_or_default();
        f(&mut s.state);
        db.save_state(name, &s)
    }

    pub(crate) fn stored(&self, org: &OrgId, name: &str) -> Result<Stored> {
        Ok(self
            .db(org)?
            .lock()
            .unwrap()
            .load_state(name)?
            .unwrap_or_default())
    }

    fn due_soon(&self, org: &OrgId, name: &str) {
        let mut slots = self.inner.slots.lock().unwrap();
        let s = slots.entry((org.clone(), name.to_string())).or_default();
        s.next = now_ms() + 1000;
    }

    // ---- apps' own monitors ----

    /// Give every app with a served domain its own monitor, and remove
    /// those whose app is gone, has no domains, or opted out.
    pub fn sync_auto(&self, org: &OrgId) -> Result<()> {
        let settings = self.settings(org)?;
        let apps = self.inner.apps.list(org)?;
        let ctl = self.inner.apps.controller();
        let served = |a: &crate::app::App| -> bool {
            let Ok(stack) = a.spec.stack() else {
                return false;
            };
            ctl.status(&crate::stack::qualified(org, &stack))
                .ok()
                .and_then(|s| s.services.into_iter().find(|x| x.service == a.spec.name))
                .is_some_and(|s| s.domains.iter().any(|d| d.url.is_some()))
        };
        let wanted = |a: &crate::app::App| {
            settings.auto_monitors
                && !settings.exclude_apps.contains(&a.spec.name)
                && !a.spec.domains.is_empty()
        };
        let _g = self.inner.edit.lock().unwrap();
        let mut all = self.list(org)?;
        let before = all.len();
        let mut gone = Vec::new();
        all.retain(|m| {
            let keep = !m.auto
                || apps
                    .iter()
                    .any(|a| Some(&a.spec.name) == m.app.as_ref() && wanted(a));
            if !keep {
                gone.push(m.name.clone());
            }
            keep
        });
        let mut added = Vec::new();
        for a in apps.iter().filter(|a| wanted(a) && served(a)) {
            let name = format!("{AUTO_PREFIX}{}", a.spec.name);
            if all.len() >= MAX_PER_ORG || all.iter().any(|m| m.name == name) {
                continue;
            }
            let mut m = Monitor::new(&name, Kind::App);
            m.app = Some(a.spec.name.clone());
            m.auto = true;
            let now = now_ms() / 1000;
            (m.created_at, m.updated_at) = (now, now);
            added.push(name);
            all.push(m);
        }
        if all.len() != before || !gone.is_empty() {
            self.save(org, &all)?;
        }
        drop(_g);
        for n in &gone {
            self.db(org)?.lock().unwrap().forget(n)?;
        }
        for n in &added {
            eprintln!("isb serve: monitor: {org}/{n}: watching the app's domain");
            self.due_soon(org, n);
        }
        Ok(())
    }

    // ---- running ----

    /// Start the scheduler and the workers.
    pub fn start(&self) {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Job>(QUEUE);
        let rx = Arc::new(Mutex::new(rx));
        for i in 0..WORKERS {
            let (me, rx) = (self.clone(), rx.clone());
            let _ = std::thread::Builder::new()
                .name(format!("isb-monitor-{i}"))
                .spawn(move || me.worker(&rx));
        }
        let me = self.clone();
        let _ = std::thread::Builder::new()
            .name("isb-monitor".into())
            .spawn(move || me.scheduler(&tx));
    }

    pub fn shutdown(&self) {
        self.inner.stop.store(true, Ordering::SeqCst);
    }

    /// Set by [`Monitors::shutdown`]: what the heartbeat stops on too.
    pub fn stopper(&self) -> Arc<AtomicBool> {
        self.inner.stop.clone()
    }

    fn scheduler(&self, tx: &SyncSender<Job>) {
        let (mut synced, mut rolled) = (0u64, now_ms());
        while !self.inner.stop.load(Ordering::SeqCst) {
            let now = now_ms();
            if now.saturating_sub(synced) >= AUTO_SYNC_MS {
                for o in self.orgs() {
                    if let Err(e) = self.sync_auto(&o) {
                        eprintln!("isb serve: monitor: {o}: apps' own monitors: {e}");
                    }
                }
                synced = now;
            }
            if now.saturating_sub(rolled) >= ROLLUP_MS {
                self.rollup_all(now);
                rolled = now;
            }
            self.tick(now, tx);
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    fn rollup_all(&self, now: u64) {
        for o in self.orgs() {
            if !self.path(&o, "monitors.db").exists() {
                continue;
            }
            if let Err(e) = self.db(&o).and_then(|d| d.lock().unwrap().rollup(now)) {
                eprintln!("isb serve: monitor: {o}: history rollup: {e}");
            }
        }
    }

    /// Queue every check that is due.
    fn tick(&self, now: u64, tx: &SyncSender<Job>) {
        let mut seen = Vec::new();
        for org in self.orgs() {
            for m in self.list(&org).unwrap_or_default() {
                let key = (org.clone(), m.name.clone());
                seen.push(key.clone());
                if m.paused {
                    continue;
                }
                let mut slots = self.inner.slots.lock().unwrap();
                let s = slots.entry(key.clone()).or_insert_with(|| Slot {
                    // The first check: a phase spread by name, within a minute.
                    next: now + fnv(&format!("{}/{}", org, m.name)) % (m.interval.min(60) * 1000),
                    running: false,
                });
                if s.running || now < s.next {
                    continue;
                }
                match tx.try_send(Job {
                    org: org.clone(),
                    monitor: m,
                }) {
                    Ok(()) => s.running = true,
                    Err(TrySendError::Full(_)) => return,
                    Err(TrySendError::Disconnected(_)) => return,
                }
            }
        }
        self.inner
            .slots
            .lock()
            .unwrap()
            .retain(|k, _| seen.contains(k));
    }

    fn worker(&self, rx: &Mutex<Receiver<Job>>) {
        loop {
            let job = match rx.lock().unwrap().recv_timeout(Duration::from_secs(1)) {
                Ok(j) => j,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if self.inner.stop.load(Ordering::SeqCst) {
                        return;
                    }
                    continue;
                }
                Err(_) => return,
            };
            let started = now_ms();
            let o = self.check(&job.org, &job.monitor);
            if let Err(e) = self.record(&job.org, &job.monitor, o) {
                eprintln!("isb serve: monitor: {}/{}: {e}", job.org, job.monitor.name);
            }
            let iv = job.monitor.interval * 1000;
            let mut slots = self.inner.slots.lock().unwrap();
            if let Some(s) = slots.get_mut(&(job.org, job.monitor.name)) {
                s.running = false;
                s.next = (started + iv).saturating_add_signed(jitter((iv / 20).min(2000)));
            }
        }
    }

    /// Run one check now.
    pub fn check(&self, org: &OrgId, m: &Monitor) -> Outcome {
        let ctx = Ctx {
            apps: &self.inner.apps,
            ctl: self.inner.apps.controller(),
            secrets: &self.inner.secrets,
            allow_private: (self.inner.allow_private)(),
            tls: self.inner.tls.clone(),
        };
        target::run(&ctx, org, m, now_ms())
    }

    /// Keep a check's result, step the state machine, raise events.
    pub fn record(&self, org: &OrgId, m: &Monitor, o: Outcome) -> Result<()> {
        // A monitor removed or paused while its check ran: drop the result.
        match self.get(org, &m.name) {
            Ok(cur) if !cur.paused => {}
            _ => return Ok(()),
        }
        let db = self.db(org)?;
        let db = db.lock().unwrap();
        db.insert(
            &m.name,
            &Check {
                at: o.at,
                ok: o.ok,
                latency_ms: o.latency_ms,
                status: o.status,
                error: o.error.clone(),
            },
        )?;
        let mut s: Stored = db.load_state(&m.name)?.unwrap_or_default();
        let t = Thresholds {
            failures: m.failure_threshold,
            recoveries: m.recovery_threshold,
        };
        let step = s.state.observe(o.ok, o.at, t);
        match step.changed {
            Some(Status::Down) => {
                let since = s.state.failing_since.unwrap_or(o.at);
                db.open_incident(&m.name, since, o.error.as_deref())?;
            }
            Some(Status::Up) => db.close_incident(&m.name, o.at)?,
            _ => {}
        }
        let cert = self.cert_due(m, &o, &mut s.state);
        s.last = Some(o.clone());
        db.save_state(&m.name, &s)?;
        drop(db);
        match step.notify {
            Notify::Down { since, flapping } => {
                self.emit_down(org, m, &o, since, flapping, s.state.fails)
            }
            Notify::Up {
                down_since,
                downtime_ms,
            } => self.emit_up(org, m, &o, down_since, downtime_ms),
            Notify::None => {}
        }
        if let Some(days) = cert {
            self.emit_cert(org, m, &o, days);
        }
        Ok(())
    }

    /// Days left on a certificate that is due a warning, once per
    /// certificate.
    fn cert_due(&self, m: &Monitor, o: &Outcome, s: &mut State) -> Option<i64> {
        let exp = o.cert_expires?;
        if m.cert_expiry_days == 0 || s.cert_warned == Some(exp) {
            return None;
        }
        let days = (exp as i64 - (o.at / 1000) as i64).div_euclid(86_400);
        (days <= i64::from(m.cert_expiry_days)).then(|| {
            s.cert_warned = Some(exp);
            days
        })
    }

    // ---- events ----

    /// Where a monitor's events go: an app's service (so channel rules on
    /// apps and projects match), else `<org>/@monitors`, service = the
    /// monitor.
    fn subject(&self, org: &OrgId, m: &Monitor) -> (String, String) {
        if let Some(a) = m
            .app
            .as_deref()
            .and_then(|a| self.inner.apps.get(org, a).ok())
        {
            if let Ok(st) = a.spec.stack() {
                return (crate::stack::qualified(org, &st), a.spec.name);
            }
        }
        (crate::stack::qualified(org, "@monitors"), m.name.clone())
    }

    /// The web UI's page of a monitor, when isb knows its public URL.
    pub fn link(&self, org: &OrgId, name: &str) -> Option<String> {
        let base = self.inner.public_url.as_deref()?;
        Some(format!("{base}/orgs/{org}/uptime/{name}"))
    }

    fn base_details(&self, org: &OrgId, m: &Monitor, o: &Outcome) -> Value {
        json!({
            "monitor": m.name,
            "type": m.kind,
            "app": m.app,
            "url": o.url.clone().unwrap_or_else(|| m.target()),
            "status": o.status,
            "latency_ms": o.latency_ms,
            "error": o.error,
            "via": o.via,
            "note": o.note,
            "checked_at": o.at,
            "link": self.link(org, &m.name),
        })
    }

    fn emit(
        &self,
        org: &OrgId,
        m: &Monitor,
        kind: &str,
        level: &str,
        message: String,
        details: Value,
    ) {
        let (stack, service) = self.subject(org, m);
        {
            let mut d = self.inner.details.lock().unwrap();
            if d.len() >= DETAILS_KEPT {
                d.pop_front();
            }
            d.push_back(((org.clone(), kind.to_string(), message.clone()), details));
        }
        self.inner
            .apps
            .controller()
            .event(kind, level, &stack, &service, message);
    }

    fn emit_down(
        &self,
        org: &OrgId,
        m: &Monitor,
        o: &Outcome,
        since: u64,
        flapping: bool,
        fails: u32,
    ) {
        let mut d = self.base_details(org, m, o);
        d["down_since"] = json!(since);
        d["failures"] = json!(fails);
        d["flapping"] = json!(flapping);
        self.emit(
            org,
            m,
            "monitor.down",
            "error",
            down_message(m, o, fails, flapping),
            d,
        );
    }

    fn emit_up(&self, org: &OrgId, m: &Monitor, o: &Outcome, down_since: u64, downtime_ms: u64) {
        let mut d = self.base_details(org, m, o);
        d["down_since"] = json!(down_since);
        d["downtime_ms"] = json!(downtime_ms);
        d["downtime"] = json!(human(downtime_ms));
        self.emit(
            org,
            m,
            "monitor.up",
            "info",
            up_message(m, o, downtime_ms),
            d,
        );
    }

    fn emit_cert(&self, org: &OrgId, m: &Monitor, o: &Outcome, days: i64) {
        let mut d = self.base_details(org, m, o);
        d["cert_expires_at"] = json!(o.cert_expires);
        d["cert_days_left"] = json!(days);
        self.emit(
            org,
            m,
            "monitor.cert_expiring",
            "warn",
            cert_message(m, o, days),
            d,
        );
    }

    /// The details of an event this service raised, for the notifier.
    pub fn details(&self, org: &OrgId, kind: &str, message: &str) -> Option<Value> {
        let d = self.inner.details.lock().unwrap();
        d.iter()
            .rev()
            .find(|((o, k, msg), _)| o == org && k == kind && msg == message)
            .map(|(_, v)| v.clone())
    }
}

fn what(m: &Monitor, o: &Outcome) -> String {
    o.url.clone().unwrap_or_else(|| m.target())
}

pub fn down_message(m: &Monitor, o: &Outcome, fails: u32, flapping: bool) -> String {
    let why = o.error.as_deref().unwrap_or("failed");
    let mut s = format!(
        "Monitor {} is DOWN: {}: {why} ({fails} failed check{} in a row)",
        m.name,
        what(m, o),
        if fails == 1 { "" } else { "s" }
    );
    if flapping {
        s.push_str("; it is flapping, so further changes are held until it is stable for 30 min");
    }
    s
}

pub fn up_message(m: &Monitor, o: &Outcome, downtime_ms: u64) -> String {
    let answer = match (o.status, o.latency_ms) {
        (Some(st), Some(l)) => format!("answered HTTP {st} in {l} ms"),
        (None, Some(l)) => format!("answered in {l} ms"),
        _ => "answered".into(),
    };
    format!(
        "Monitor {} is UP again after {}: {} {answer}",
        m.name,
        human(downtime_ms),
        what(m, o)
    )
}

pub fn cert_message(m: &Monitor, o: &Outcome, days: i64) -> String {
    let on = o
        .cert_expires
        .map(|e| {
            let d = (e / 86_400) as i64;
            let (y, mo, da) = civil(d);
            format!(" ({y:04}-{mo:02}-{da:02})")
        })
        .unwrap_or_default();
    let when = if days < 0 {
        "has expired".to_string()
    } else {
        format!("expires in {days} day{}", if days == 1 { "" } else { "s" })
    };
    format!(
        "Monitor {}: the TLS certificate of {} {when}{on}",
        m.name,
        what(m, o)
    )
}

/// The date of a day number since 1970-01-01.
fn civil(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
