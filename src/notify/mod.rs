//! Notifications: per-org channels (webhook, Slack, Discord, Telegram,
//! email) told about events of chosen kinds (`deploy.*`, `health.*`,
//! `backup.*`, `job.*`, `cert.*`; see [`crate::stack::controller::Event`]).
//!
//! A dispatcher thread follows the controller's event feed and queues a
//! delivery per matching channel. Each channel has its own bounded queue and
//! sender thread, so a slow or failing channel never holds up another: a
//! delivery is retried with exponential backoff, a channel is rate limited,
//! and the last deliveries per channel are kept with their outcome.
//!
//! The feed lives in memory and starts at 1 with each daemon run, so the
//! dispatcher starts from the beginning of the run: nothing from before a
//! restart is sent again (and deliveries still queued at a restart are lost).
//!
//! Channels and delivery logs are kept under `<state>/orgs/<org>/notify/`.
//! Destinations are held to [`net`]'s address policy.

pub mod net;
pub mod provider;
pub mod smtp;

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::org::OrgId;
use crate::secrets::Secrets;
use crate::stack::controller::{Controller, Event, now_ms};
use net::{Net, SendError};
pub use provider::{Message, Provider};

/// Deliveries waiting per channel; the oldest is dropped past this.
pub const QUEUE_MAX: usize = 100;
/// Deliveries kept in a channel's log.
pub const LOG_KEPT: usize = 50;
/// Attempts per delivery, the first included.
pub const ATTEMPTS: u32 = 6;
/// Messages a channel sends per minute at most.
pub const PER_MINUTE: usize = 20;
/// The first retry's wait; it doubles per attempt.
const BACKOFF: Duration = Duration::from_secs(5);
/// The longest wait between attempts (Retry-After included).
const BACKOFF_MAX: Duration = Duration::from_secs(300);
/// A channel's sender thread exits after this long with nothing to send.
const IDLE: Duration = Duration::from_secs(60);

/// Which events a channel hears about. An event matches a rule when its kind
/// matches one of `events` (globs: `deploy.*`, `*.failed`, `*`) and it
/// passes every non-empty filter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    #[serde(default = "all_events")]
    pub events: Vec<String>,
    /// App projects (an app's events only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projects: Vec<String>,
    /// Apps (a service of the same name in the app's stack).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub apps: Vec<String>,
    /// Stacks, by their name in the org.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stacks: Vec<String>,
}

fn all_events() -> Vec<String> {
    vec!["*".into()]
}

impl Default for Rule {
    fn default() -> Rule {
        Rule {
            events: all_events(),
            projects: Vec::new(),
            apps: Vec::new(),
            stacks: Vec::new(),
        }
    }
}

/// `*` matches any run of characters, everything else itself.
pub fn glob(pattern: &str, s: &str) -> bool {
    let (p, s) = (pattern.as_bytes(), s.as_bytes());
    let (mut pi, mut si) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while si < s.len() {
        if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            mark = si;
            pi += 1;
        } else if pi < p.len() && p[pi] == s[si] {
            pi += 1;
            si += 1;
        } else if let Some(st) = star {
            pi = st + 1;
            mark += 1;
            si = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == b'*')
}

/// What a rule is matched against.
#[derive(Debug, Clone, Default)]
pub struct Subject<'a> {
    pub kind: &'a str,
    /// The stack's name in its org.
    pub stack: &'a str,
    pub service: &'a str,
    /// The app's project, when the service is an app.
    pub project: Option<&'a str>,
}

impl Rule {
    pub fn matches(&self, s: &Subject) -> bool {
        self.events.iter().any(|p| glob(p, s.kind))
            && (self.stacks.is_empty() || self.stacks.iter().any(|x| x == s.stack))
            && (self.apps.is_empty() || self.apps.iter().any(|x| x == s.service))
            && (self.projects.is_empty()
                || s.project
                    .is_some_and(|p| self.projects.iter().any(|x| x == p)))
    }

    fn validate(&self) -> Result<()> {
        if self.events.is_empty() {
            return Err(Error::invalid("a rule needs at least one event pattern"));
        }
        for e in &self.events {
            if e.is_empty()
                || e.len() > 64
                || !e
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-*".contains(c))
            {
                return Err(Error::invalid(format!(
                    "event pattern {e:?}: [a-z0-9._-] and * globs, like deploy.* or *.failed"
                )));
            }
        }
        Ok(())
    }
}

/// A notification channel of an org.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Channel {
    pub name: String,
    pub provider: Provider,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Any rule matching is enough. Default: every event with a kind.
    #[serde(default = "default_rules")]
    pub rules: Vec<Rule>,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub updated_at: u64,
}

fn yes() -> bool {
    true
}

fn default_rules() -> Vec<Rule> {
    vec![Rule::default()]
}

impl Channel {
    pub fn matches(&self, s: &Subject) -> bool {
        self.enabled && self.rules.iter().any(|r| r.matches(s))
    }

    pub fn validate(&self) -> Result<()> {
        validate_name(&self.name)?;
        self.provider.validate().map_err(Error::Invalid)?;
        if self.rules.len() > 20 {
            return Err(Error::invalid("at most 20 rules per channel"));
        }
        for r in &self.rules {
            r.validate()?;
        }
        Ok(())
    }
}

/// A channel name: [a-z0-9-], starts with a letter, at most 40.
pub fn validate_name(n: &str) -> Result<()> {
    let ok = !n.is_empty()
        && n.len() <= 40
        && n.starts_with(|c: char| c.is_ascii_lowercase())
        && n.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "channel name {n:?}: [a-z0-9-], starting with a letter, at most 40 characters"
        )))
    }
}

/// One delivery to one channel, as its log shows it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Delivery {
    pub id: String,
    /// Unix milliseconds it was queued.
    pub at: u64,
    pub kind: String,
    pub seq: u64,
    pub summary: String,
    /// `queued`, `retrying`, `sent`, `failed`, `dropped` (queue full) or
    /// `skipped` (channel disabled or gone).
    pub status: String,
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<u64>,
    #[serde(default)]
    pub test: bool,
}

/// Server-wide settings (platform admins).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    /// Let channels reach loopback, private and other non-public addresses.
    #[serde(default)]
    pub allow_private_targets: bool,
}

/// The wait before attempt `attempt + 1` (attempt counts from 1): the base
/// doubling per attempt, at least what the server asked for, capped.
pub fn backoff(base: Duration, attempt: u32, retry_after: Option<Duration>) -> Duration {
    let exp = base.saturating_mul(1u32 << attempt.saturating_sub(1).min(16));
    exp.max(retry_after.unwrap_or_default()).min(BACKOFF_MAX)
}

/// Sliding one-minute window of sends.
#[derive(Debug, Default)]
pub struct RateLimit {
    sent: VecDeque<Instant>,
}

impl RateLimit {
    /// How long to wait before the next send may go, at `now`.
    pub fn wait(&mut self, now: Instant, per_minute: usize) -> Duration {
        while self
            .sent
            .front()
            .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(60))
        {
            self.sent.pop_front();
        }
        if self.sent.len() < per_minute {
            return Duration::ZERO;
        }
        (self.sent[0] + Duration::from_secs(60)).saturating_duration_since(now)
    }

    pub fn record(&mut self, now: Instant) {
        self.sent.push_back(now);
    }
}

/// The app project a service belongs to: `(org, stack, service)`.
pub type Resolve = Arc<dyn Fn(&OrgId, &str, &str) -> Option<String> + Send + Sync>;

/// The org of an event and its stack's own name. Events name their stack
/// `org/stack`, or just `stack` in the default org.
pub fn event_org(stack: &str) -> (OrgId, &str) {
    match stack.split_once('/') {
        Some((o, s)) => (OrgId::new(o).unwrap_or_else(|_| OrgId::default_org()), s),
        None => (OrgId::default_org(), stack),
    }
}

struct Job {
    org: OrgId,
    channel: String,
    msg: Message,
}

struct Queue {
    jobs: Mutex<(VecDeque<Job>, bool)>,
    wake: Condvar,
}

struct Inner {
    state: PathBuf,
    secrets: Arc<Secrets>,
    settings: Mutex<Settings>,
    /// Held across read-modify-write of a channels file.
    edit: Mutex<()>,
    queues: Mutex<BTreeMap<(OrgId, String), Arc<Queue>>>,
    logs: Mutex<BTreeMap<(OrgId, String), VecDeque<Delivery>>>,
    resolve: Resolve,
    next_id: AtomicU64,
    stop: AtomicBool,
    backoff: Duration,
    per_minute: usize,
    /// Trust these TLS roots instead of the public set (tests).
    tls: Option<Arc<rustls::ClientConfig>>,
}

/// The notification service of a daemon.
#[derive(Clone)]
pub struct Notifier {
    inner: Arc<Inner>,
}

fn settings_path(state: &Path) -> PathBuf {
    state.join("notify.json")
}

fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

impl Notifier {
    /// Open the notifier over a state directory. Nothing is sent until
    /// [`Notifier::start`].
    pub fn new(state: &Path, secrets: Arc<Secrets>, resolve: Resolve) -> Result<Notifier> {
        let settings = match std::fs::read(settings_path(state)) {
            Ok(b) => serde_json::from_slice(&b)
                .map_err(|e| Error::invalid(format!("{}: {e}", settings_path(state).display())))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Settings::default(),
            Err(e) => return Err(e.into()),
        };
        Ok(Notifier {
            inner: Arc::new(Inner {
                state: state.to_path_buf(),
                secrets,
                settings: Mutex::new(settings),
                edit: Mutex::new(()),
                queues: Mutex::new(BTreeMap::new()),
                logs: Mutex::new(BTreeMap::new()),
                resolve,
                next_id: AtomicU64::new(1),
                stop: AtomicBool::new(false),
                backoff: BACKOFF,
                per_minute: PER_MINUTE,
                tls: None,
            }),
        })
    }

    /// Follow the controller's events from the start of this run.
    pub fn start(&self, ctl: Controller) {
        let me = self.clone();
        let _ = std::thread::Builder::new()
            .name("isb-notify".into())
            .spawn(move || {
                let mut since = 0;
                while !me.inner.stop.load(Ordering::SeqCst) {
                    let (seq, evs) = ctl.wait_events(since, 1000, Duration::from_secs(5));
                    for e in &evs {
                        me.route(e);
                    }
                    // The feed returns everything after `since` (it keeps
                    // fewer than 1000), so `seq` is where to go on from.
                    since = since.max(seq);
                }
            });
    }

    pub fn shutdown(&self) {
        self.inner.stop.store(true, Ordering::SeqCst);
        for q in self.inner.queues.lock().unwrap().values() {
            q.wake.notify_all();
        }
    }

    pub fn settings(&self) -> Settings {
        self.inner.settings.lock().unwrap().clone()
    }

    pub fn set_settings(&self, s: Settings) -> Result<()> {
        write_atomic(
            &settings_path(&self.inner.state),
            &serde_json::to_vec_pretty(&s)?,
        )?;
        *self.inner.settings.lock().unwrap() = s;
        Ok(())
    }

    fn net(&self) -> Net {
        let allow = self.inner.settings.lock().unwrap().allow_private_targets;
        Net {
            allow_private: allow,
            tls: self.inner.tls.clone().unwrap_or_else(net::default_tls),
        }
    }

    fn dir(&self, org: &OrgId) -> PathBuf {
        org.dir(&self.inner.state).join("notify")
    }

    /// The org's channels.
    pub fn list(&self, org: &OrgId) -> Result<Vec<Channel>> {
        match std::fs::read(self.dir(org).join("channels.json")) {
            Ok(b) => Ok(serde_json::from_slice(&b)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e.into()),
        }
    }

    fn save(&self, org: &OrgId, chans: &[Channel]) -> Result<()> {
        write_atomic(
            &self.dir(org).join("channels.json"),
            &serde_json::to_vec_pretty(chans)?,
        )
    }

    pub fn get(&self, org: &OrgId, name: &str) -> Result<Channel> {
        self.list(org)?
            .into_iter()
            .find(|c| c.name == name)
            .ok_or_else(|| Error::NotFound(format!("notification channel {name}")))
    }

    /// Check a channel against the org's secrets: they must exist, and URL
    /// secrets must hold a URL fit for the provider. Values are never shown.
    fn check_secrets(&self, org: &OrgId, c: &Channel) -> Result<()> {
        for s in c.provider.secrets() {
            self.inner.secrets.inspect(org, s).map_err(|e| {
                if e.is_not_found() {
                    Error::invalid(format!(
                        "no secret {s} in org {org}: create it first (isb secret create {s})"
                    ))
                } else {
                    e
                }
            })?;
        }
        if let Provider::Webhook { url_secret, .. }
        | Provider::Slack { url_secret }
        | Provider::Discord { url_secret } = &c.provider
        {
            let v = self
                .secret_value(org, url_secret)
                .map_err(|e| Error::Invalid(e.message))?;
            provider::check_url_value(&c.provider, &v)
                .map_err(|e| Error::invalid(format!("secret {url_secret}: {e}")))?;
        }
        Ok(())
    }

    pub fn create(&self, org: &OrgId, mut c: Channel) -> Result<Channel> {
        c.validate()?;
        self.check_secrets(org, &c)?;
        let _g = self.inner.edit.lock().unwrap();
        let mut all = self.list(org)?;
        if all.iter().any(|x| x.name == c.name) {
            return Err(Error::invalid(format!(
                "notification channel {} exists",
                c.name
            )));
        }
        if all.len() >= 50 {
            return Err(Error::invalid("at most 50 notification channels per org"));
        }
        let now = now_ms() / 1000;
        c.created_at = now;
        c.updated_at = now;
        all.push(c.clone());
        self.save(org, &all)?;
        Ok(c)
    }

    /// Replace a channel's provider, rules or enabled flag (each optional).
    pub fn update(
        &self,
        org: &OrgId,
        name: &str,
        provider: Option<Provider>,
        rules: Option<Vec<Rule>>,
        enabled: Option<bool>,
    ) -> Result<Channel> {
        let _g = self.inner.edit.lock().unwrap();
        let mut all = self.list(org)?;
        let c = all
            .iter_mut()
            .find(|c| c.name == name)
            .ok_or_else(|| Error::NotFound(format!("notification channel {name}")))?;
        let mut n = c.clone();
        if let Some(p) = provider {
            n.provider = p;
        }
        if let Some(r) = rules {
            n.rules = r;
        }
        if let Some(e) = enabled {
            n.enabled = e;
        }
        n.validate()?;
        self.check_secrets(org, &n)?;
        n.updated_at = now_ms() / 1000;
        *c = n.clone();
        self.save(org, &all)?;
        Ok(n)
    }

    pub fn delete(&self, org: &OrgId, name: &str) -> Result<()> {
        let _g = self.inner.edit.lock().unwrap();
        let mut all = self.list(org)?;
        let before = all.len();
        all.retain(|c| c.name != name);
        if all.len() == before {
            return Err(Error::NotFound(format!("notification channel {name}")));
        }
        self.save(org, &all)?;
        let key = (org.clone(), name.to_string());
        self.inner.logs.lock().unwrap().remove(&key);
        let _ = std::fs::remove_file(self.log_path(org, name));
        if let Some(q) = self.inner.queues.lock().unwrap().get(&key) {
            q.jobs.lock().unwrap().0.clear();
        }
        Ok(())
    }

    fn log_path(&self, org: &OrgId, name: &str) -> PathBuf {
        self.dir(org)
            .join("deliveries")
            .join(format!("{name}.json"))
    }

    /// A channel's recent deliveries, newest first.
    pub fn deliveries(&self, org: &OrgId, name: &str) -> Result<Vec<Delivery>> {
        self.get(org, name)?;
        let mut logs = self.inner.logs.lock().unwrap();
        let log = self.log_of(&mut logs, org, name);
        Ok(log.iter().rev().cloned().collect())
    }

    fn log_of<'a>(
        &self,
        logs: &'a mut BTreeMap<(OrgId, String), VecDeque<Delivery>>,
        org: &OrgId,
        name: &str,
    ) -> &'a mut VecDeque<Delivery> {
        logs.entry((org.clone(), name.to_string()))
            .or_insert_with(|| {
                std::fs::read(self.log_path(org, name))
                    .ok()
                    .and_then(|b| serde_json::from_slice(&b).ok())
                    .unwrap_or_default()
            })
    }

    /// Insert or replace a delivery in its channel's log, and persist it.
    fn record(&self, org: &OrgId, name: &str, d: &Delivery) {
        let mut logs = self.inner.logs.lock().unwrap();
        let log = self.log_of(&mut logs, org, name);
        match log.iter_mut().find(|x| x.id == d.id) {
            Some(x) => *x = d.clone(),
            None => {
                log.push_back(d.clone());
                while log.len() > LOG_KEPT {
                    log.pop_front();
                }
            }
        }
        let data = serde_json::to_vec(&*log).unwrap_or_default();
        if let Err(e) = write_atomic(&self.log_path(org, name), &data) {
            eprintln!("isb serve: notify: cannot save the delivery log of {org}/{name}: {e}");
        }
    }

    fn new_id(&self) -> String {
        format!(
            "{}-{}",
            now_ms(),
            self.inner.next_id.fetch_add(1, Ordering::SeqCst)
        )
    }

    /// Queue a delivery to every channel of the event's org that wants it.
    pub fn route(&self, e: &Event) {
        let Some(kind) = e.kind.as_deref() else {
            return;
        };
        let (org, stack) = event_org(&e.stack);
        let Ok(chans) = self.list(&org) else { return };
        if chans.is_empty() {
            return;
        }
        let project = if e.service.is_empty() {
            None
        } else {
            (self.inner.resolve)(&org, stack, &e.service)
        };
        let subject = Subject {
            kind,
            stack,
            service: &e.service,
            project: project.as_deref(),
        };
        for c in chans.iter().filter(|c| c.matches(&subject)) {
            let msg = Message {
                id: self.new_id(),
                org: org.to_string(),
                kind: kind.to_string(),
                level: e.level.clone(),
                stack: stack.to_string(),
                service: e.service.clone(),
                project: project.clone(),
                instance: e.instance.clone(),
                message: e.message.clone(),
                at: e.at,
                seq: e.seq,
                test: false,
            };
            self.enqueue(Job {
                org: org.clone(),
                channel: c.name.clone(),
                msg,
            });
        }
    }

    fn delivery(msg: &Message, status: &str) -> Delivery {
        Delivery {
            id: msg.id.clone(),
            at: now_ms(),
            kind: msg.kind.clone(),
            seq: msg.seq,
            summary: msg.message.chars().take(200).collect(),
            status: status.into(),
            attempts: 0,
            http_status: None,
            error: None,
            finished_at: None,
            test: msg.test,
        }
    }

    fn enqueue(&self, job: Job) {
        let key = (job.org.clone(), job.channel.clone());
        let q = self
            .inner
            .queues
            .lock()
            .unwrap()
            .entry(key.clone())
            .or_insert_with(|| {
                Arc::new(Queue {
                    jobs: Mutex::new((VecDeque::new(), false)),
                    wake: Condvar::new(),
                })
            })
            .clone();
        self.record(&job.org, &job.channel, &Self::delivery(&job.msg, "queued"));
        let mut dropped = None;
        let spawn = {
            let mut g = q.jobs.lock().unwrap();
            if g.0.len() >= QUEUE_MAX {
                dropped = g.0.pop_front();
            }
            g.0.push_back(job);
            q.wake.notify_all();
            !std::mem::replace(&mut g.1, true)
        };
        if let Some(d) = dropped {
            let mut r = Self::delivery(&d.msg, "dropped");
            r.error = Some(format!("the channel's queue was full ({QUEUE_MAX})"));
            r.finished_at = Some(now_ms());
            self.record(&d.org, &d.channel, &r);
        }
        if spawn {
            let me = self.clone();
            let r = std::thread::Builder::new()
                .name(format!("isb-notify-{}", key.1))
                .spawn(move || me.sender(q));
            if let Err(e) = r {
                eprintln!("isb serve: notify: cannot start a sender: {e}");
            }
        }
    }

    /// A channel's sender: one delivery at a time, in order, until idle.
    fn sender(&self, q: Arc<Queue>) {
        let mut rate = RateLimit::default();
        loop {
            let job = {
                let mut g = q.jobs.lock().unwrap();
                let started = Instant::now();
                loop {
                    if self.inner.stop.load(Ordering::SeqCst) {
                        g.1 = false;
                        return;
                    }
                    if let Some(j) = g.0.pop_front() {
                        break j;
                    }
                    let left = IDLE.saturating_sub(started.elapsed());
                    if left.is_zero() {
                        g.1 = false;
                        return;
                    }
                    g = q.wake.wait_timeout(g, left).unwrap().0;
                }
            };
            let wait = rate.wait(Instant::now(), self.inner.per_minute);
            if !wait.is_zero() {
                self.sleep(wait);
            }
            rate.record(Instant::now());
            self.deliver(&job);
        }
    }

    /// Sleep in short steps so a shutdown is not held up.
    fn sleep(&self, d: Duration) {
        let end = Instant::now() + d;
        while !self.inner.stop.load(Ordering::SeqCst) {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            std::thread::sleep(left.min(Duration::from_millis(500)));
        }
    }

    /// Send one queued delivery, retrying what may succeed later.
    fn deliver(&self, job: &Job) {
        let mut d = Self::delivery(&job.msg, "queued");
        // The delivery was recorded at enqueue time: keep its id and time.
        if let Some(prev) = self
            .inner
            .logs
            .lock()
            .unwrap()
            .get(&(job.org.clone(), job.channel.clone()))
            .and_then(|l| l.iter().find(|x| x.id == job.msg.id).cloned())
        {
            d.at = prev.at;
        }
        for attempt in 1..=ATTEMPTS {
            // Read the channel each time: it may have been edited or removed.
            let ch = match self.get(&job.org, &job.channel) {
                Ok(c) if c.enabled => c,
                Ok(_) | Err(_) => {
                    d.status = "skipped".into();
                    d.error = Some("the channel was disabled or removed".into());
                    d.finished_at = Some(now_ms());
                    self.record(&job.org, &job.channel, &d);
                    return;
                }
            };
            d.attempts = attempt;
            let r = self.send(&job.org, &ch, &job.msg);
            match r {
                Ok(status) => {
                    d.status = "sent".into();
                    d.http_status = status;
                    d.error = None;
                    d.finished_at = Some(now_ms());
                    self.record(&job.org, &job.channel, &d);
                    return;
                }
                Err((status, e, retry_after)) => {
                    d.http_status = status;
                    d.error = Some(e.message.clone());
                    if !e.retryable || attempt == ATTEMPTS || self.inner.stop.load(Ordering::SeqCst)
                    {
                        d.status = "failed".into();
                        d.finished_at = Some(now_ms());
                        self.record(&job.org, &job.channel, &d);
                        eprintln!(
                            "isb serve: notify {}/{}: {} not delivered: {}",
                            job.org, job.channel, job.msg.kind, e.message
                        );
                        return;
                    }
                    d.status = "retrying".into();
                    self.record(&job.org, &job.channel, &d);
                    self.sleep(backoff(self.inner.backoff, attempt, retry_after));
                }
            }
        }
    }

    fn secret_value(&self, org: &OrgId, name: &str) -> std::result::Result<String, SendError> {
        let (v, _) = self.inner.secrets.get(org, name).map_err(|e| {
            if e.is_not_found() {
                SendError::permanent(format!("secret {name} is gone"))
            } else {
                SendError::transient(format!("secret {name}: {e}"))
            }
        })?;
        String::from_utf8(v)
            .map(|s| s.trim().to_string())
            .map_err(|_| SendError::permanent(format!("secret {name} is not UTF-8 text")))
    }

    /// One attempt. Ok: the HTTP status, if HTTP. Err: the HTTP status (if
    /// any), why, and how long the server asked us to wait.
    fn send(
        &self,
        org: &OrgId,
        ch: &Channel,
        msg: &Message,
    ) -> std::result::Result<Option<u16>, (Option<u16>, SendError, Option<Duration>)> {
        let net = self.net();
        let secret = |n: &str| self.secret_value(org, n);
        if let Provider::Email {
            host,
            port,
            tls,
            username,
            password_secret,
            from,
            to,
        } = &ch.provider
        {
            let password = match password_secret {
                Some(p) => Some(secret(p).map_err(|e| (None, e, None))?),
                None => None,
            };
            let body = format!(
                "{}\n\nlevel: {}\norg: {}\nstack: {}\n{}{}at: {}\n",
                msg.message,
                msg.level,
                msg.org,
                msg.stack,
                if msg.service.is_empty() {
                    String::new()
                } else {
                    format!("service: {}\n", msg.service)
                },
                msg.project
                    .as_ref()
                    .map(|p| format!("project: {p}\n"))
                    .unwrap_or_default(),
                smtp::rfc2822(msg.at / 1000),
            );
            let mid = format!("{}@isb", msg.id);
            return smtp::send(
                &net,
                &smtp::Mail {
                    host,
                    port: port.unwrap_or(tls.default_port()),
                    tls: *tls,
                    username: username.as_deref(),
                    password: password.as_deref(),
                    from,
                    to,
                    subject: &msg.title(),
                    body: &body,
                    date: now_ms() / 1000,
                    message_id: &mid,
                },
            )
            .map(|_| None)
            .map_err(|e| (None, e, None));
        }
        let req = provider::request(&ch.provider, msg, &secret).map_err(|e| (None, e, None))?;
        let r = net::post(&net, &req).map_err(|e| (None, e, None))?;
        if (200..300).contains(&r.status) {
            return Ok(Some(r.status));
        }
        let retryable = r.status == 429 || r.status == 408 || r.status >= 500;
        let what = if (300..400).contains(&r.status) {
            "a redirect (not followed)".to_string()
        } else {
            let b: String = r.body.chars().take(200).collect();
            format!(
                "HTTP {}{}",
                r.status,
                if b.is_empty() {
                    String::new()
                } else {
                    format!(": {b}")
                }
            )
        };
        Err((
            Some(r.status),
            SendError {
                message: format!("{} answered {what}", ch.provider.kind()),
                retryable,
            },
            r.retry_after,
        ))
    }

    /// Send a test message to a channel now, once, and log it.
    pub fn test(&self, org: &OrgId, name: &str, by: &str) -> Result<Delivery> {
        let ch = self.get(org, name)?;
        let msg = Message {
            id: self.new_id(),
            org: org.to_string(),
            kind: "test".into(),
            level: "info".into(),
            stack: "-".into(),
            service: String::new(),
            project: None,
            instance: None,
            message: format!("A test notification from isb for channel {name}, sent by {by}."),
            at: now_ms(),
            seq: 0,
            test: true,
        };
        let mut d = Self::delivery(&msg, "sent");
        d.attempts = 1;
        match self.send(org, &ch, &msg) {
            Ok(s) => d.http_status = s,
            Err((s, e, _)) => {
                d.status = "failed".into();
                d.http_status = s;
                d.error = Some(e.message);
            }
        }
        d.finished_at = Some(now_ms());
        self.record(org, name, &d);
        Ok(d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn globs() {
        for (p, s) in [
            ("*", "deploy.failed"),
            ("deploy.*", "deploy.failed"),
            ("*.failed", "backup.failed"),
            ("deploy.failed", "deploy.failed"),
            ("*.*", "a.b"),
            ("d*y.*d", "deploy.failed"),
        ] {
            assert!(glob(p, s), "{p} {s}");
        }
        for (p, s) in [
            ("deploy.*", "health.unhealthy"),
            ("*.failed", "deploy.succeeded"),
            ("deploy", "deploy.failed"),
            ("deploy.failed", "deploy.failed2"),
        ] {
            assert!(!glob(p, s), "{p} {s}");
        }
    }

    #[test]
    fn rules() {
        let s = Subject {
            kind: "deploy.failed",
            stack: "shop-production",
            service: "web",
            project: Some("shop"),
        };
        assert!(Rule::default().matches(&s));
        let r = |j: serde_json::Value| -> Rule { serde_json::from_value(j).unwrap() };
        assert!(r(serde_json::json!({"events": ["deploy.*"]})).matches(&s));
        assert!(!r(serde_json::json!({"events": ["health.*"]})).matches(&s));
        assert!(r(serde_json::json!({"events": ["*"], "projects": ["shop"]})).matches(&s));
        assert!(!r(serde_json::json!({"events": ["*"], "projects": ["blog"]})).matches(&s));
        assert!(r(serde_json::json!({"apps": ["web", "api"]})).matches(&s));
        assert!(!r(serde_json::json!({"apps": ["api"]})).matches(&s));
        assert!(r(serde_json::json!({"stacks": ["shop-production"]})).matches(&s));
        assert!(!r(serde_json::json!({"stacks": ["shop-staging"]})).matches(&s));
        // A project filter never matches a plain stack's events.
        let plain = Subject {
            project: None,
            ..s.clone()
        };
        assert!(!r(serde_json::json!({"projects": ["shop"]})).matches(&plain));
        // All filters must hold.
        assert!(!r(serde_json::json!({"events": ["deploy.*"], "apps": ["api"]})).matches(&s));
        assert!(serde_json::from_value::<Rule>(serde_json::json!({"evnts": ["x"]})).is_err());
        assert!(
            r(serde_json::json!({"events": ["Deploy"]}))
                .validate()
                .is_err()
        );
        assert!(r(serde_json::json!({"events": []})).validate().is_err());
        // A disabled channel hears nothing.
        let ch = Channel {
            name: "ops".into(),
            provider: Provider::Slack {
                url_secret: "S".into(),
            },
            enabled: false,
            rules: default_rules(),
            created_at: 0,
            updated_at: 0,
        };
        assert!(!ch.matches(&s));
    }

    #[test]
    fn event_orgs() {
        let (o, s) = event_org("acme/shop-production");
        assert_eq!((o.as_str(), s), ("acme", "shop-production"));
        let (o, s) = event_org("web");
        assert_eq!((o.as_str(), s), ("default", "web"));
    }

    #[test]
    fn backoff_and_rate() {
        let b = Duration::from_secs(5);
        assert_eq!(backoff(b, 1, None), Duration::from_secs(5));
        assert_eq!(backoff(b, 2, None), Duration::from_secs(10));
        assert_eq!(backoff(b, 5, None), Duration::from_secs(80));
        assert_eq!(backoff(b, 30, None), BACKOFF_MAX);
        assert_eq!(
            backoff(b, 1, Some(Duration::from_secs(42))),
            Duration::from_secs(42)
        );
        assert_eq!(backoff(b, 1, Some(Duration::from_secs(9999))), BACKOFF_MAX);
        let mut r = RateLimit::default();
        let t0 = Instant::now();
        for i in 0..3 {
            assert!(r.wait(t0 + Duration::from_secs(i), 3).is_zero());
            r.record(t0 + Duration::from_secs(i));
        }
        assert_eq!(
            r.wait(t0 + Duration::from_secs(10), 3),
            Duration::from_secs(50)
        );
        assert!(r.wait(t0 + Duration::from_secs(60), 3).is_zero());
    }

    /// A fake HTTP receiver answering each request with the next status.
    fn receiver(statuses: Vec<u16>) -> (u16, std::thread::JoinHandle<Vec<String>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            let mut got = Vec::new();
            for st in statuses {
                let (mut s, _) = l.accept().unwrap();
                let mut buf = Vec::new();
                let mut b = [0u8; 4096];
                loop {
                    let n = s.read(&mut b).unwrap();
                    buf.extend_from_slice(&b[..n]);
                    let t = String::from_utf8_lossy(&buf).to_string();
                    if let Some(i) = t.find("\r\n\r\n") {
                        let len: usize = t
                            .lines()
                            .find_map(|l| l.strip_prefix("Content-Length: "))
                            .and_then(|v| v.trim().parse().ok())
                            .unwrap_or(0);
                        if buf.len() >= i + 4 + len {
                            break;
                        }
                    }
                }
                got.push(String::from_utf8_lossy(&buf).to_string());
                write!(s, "HTTP/1.1 {st} X\r\nContent-Length: 0\r\n\r\n").unwrap();
            }
            got
        });
        (port, h)
    }

    fn notifier(dir: &Path) -> (Notifier, Arc<Secrets>, OrgId) {
        let keyring = Arc::new(crate::secrets::keys::Keyring::new(
            age::x25519::Identity::generate(),
            vec![],
        ));
        let secrets = Arc::new(Secrets::new(crate::secrets::local::LocalDriver::new(
            dir, keyring,
        )));
        let mut n = Notifier::new(
            dir,
            secrets.clone(),
            Arc::new(|_, _, _| Some("shop".into())),
        )
        .unwrap();
        let inner = Arc::get_mut(&mut n.inner).unwrap();
        inner.backoff = Duration::from_millis(20);
        (n, secrets, OrgId::new("acme").unwrap())
    }

    #[test]
    fn retries_then_delivers_with_a_signature() {
        let dir = tempfile::tempdir().unwrap();
        let (n, secrets, org) = notifier(dir.path());
        let (port, h) = receiver(vec![503, 500, 200]);
        secrets
            .create(
                &org,
                "HOOK",
                None,
                format!("http://127.0.0.1:{port}/in").as_bytes(),
                &Default::default(),
            )
            .unwrap();
        secrets
            .create(&org, "SIGN", None, b"k3y", &Default::default())
            .unwrap();
        let ch = Channel {
            name: "ops".into(),
            provider: Provider::Webhook {
                url_secret: "HOOK".into(),
                signing_secret: Some("SIGN".into()),
            },
            enabled: true,
            rules: vec![
                serde_json::from_value(
                    serde_json::json!({"events": ["deploy.*"], "projects": ["shop"]}),
                )
                .unwrap(),
            ],
            created_at: 0,
            updated_at: 0,
        };
        // Loopback is refused until a platform admin allows private targets.
        n.create(&org, ch.clone()).unwrap();
        let d = n.test(&org, "ops", "tester").unwrap();
        assert_eq!(d.status, "failed");
        assert!(d.error.unwrap().contains("private targets are off"));
        n.set_settings(Settings {
            allow_private_targets: true,
        })
        .unwrap();
        let ev = |kind: &str, seq| Event {
            seq,
            at: 1,
            level: "error".into(),
            stack: "acme/shop-production".into(),
            service: "web".into(),
            instance: None,
            message: "deployment 3 failed".into(),
            kind: Some(kind.into()),
        };
        // Not matching: no delivery.
        n.route(&ev("health.unhealthy", 1));
        n.route(&ev("deploy.failed", 2));
        let got = h.join().unwrap();
        assert_eq!(got.len(), 3);
        let req = &got[2];
        let body = &req[req.find("\r\n\r\n").unwrap() + 4..];
        let sig = req
            .lines()
            .find_map(|l| l.strip_prefix("X-Isb-Signature: "))
            .unwrap();
        assert_eq!(
            sig,
            format!("sha256={}", net::hmac_sha256_hex(b"k3y", body.as_bytes()))
        );
        let v: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(v["kind"], "deploy.failed");
        assert_eq!(v["project"], "shop");
        // The log shows the delivery, sent on the third attempt.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let log = n.deliveries(&org, "ops").unwrap();
            if let Some(d) = log
                .iter()
                .find(|d| d.kind == "deploy.failed" && d.status == "sent")
            {
                assert_eq!(d.attempts, 3);
                assert_eq!(d.http_status, Some(200));
                break;
            }
            assert!(Instant::now() < deadline, "{log:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
        // The log survives a restart.
        let (n2, _, _) = notifier(dir.path());
        assert!(n2.deliveries(&org, "ops").unwrap().len() >= 2);
        n.shutdown();
    }

    #[test]
    fn permanent_failures_are_not_retried() {
        let dir = tempfile::tempdir().unwrap();
        let (n, secrets, org) = notifier(dir.path());
        n.set_settings(Settings {
            allow_private_targets: true,
        })
        .unwrap();
        let (port, h) = receiver(vec![404]);
        secrets
            .create(
                &org,
                "HOOK",
                None,
                format!("http://127.0.0.1:{port}/").as_bytes(),
                &Default::default(),
            )
            .unwrap();
        n.create(
            &org,
            Channel {
                name: "x".into(),
                provider: Provider::Webhook {
                    url_secret: "HOOK".into(),
                    signing_secret: None,
                },
                enabled: true,
                rules: default_rules(),
                created_at: 0,
                updated_at: 0,
            },
        )
        .unwrap();
        let d = n.test(&org, "x", "t").unwrap();
        assert_eq!((d.status.as_str(), d.http_status), ("failed", Some(404)));
        h.join().unwrap();
    }

    #[test]
    fn channels_check_their_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let (n, secrets, org) = notifier(dir.path());
        let slack = |s: &str| Channel {
            name: "s".into(),
            provider: Provider::Slack {
                url_secret: s.into(),
            },
            enabled: true,
            rules: default_rules(),
            created_at: 0,
            updated_at: 0,
        };
        let e = n.create(&org, slack("NOPE")).unwrap_err();
        assert!(e.to_string().contains("no secret NOPE"), "{e}");
        secrets
            .create(
                &org,
                "BAD",
                None,
                b"https://example.com/services/TOKEN",
                &Default::default(),
            )
            .unwrap();
        let e = n.create(&org, slack("BAD")).unwrap_err().to_string();
        assert!(e.contains("hooks.slack.com") && !e.contains("TOKEN"), "{e}");
        secrets
            .create(
                &org,
                "GOOD",
                None,
                b"https://hooks.slack.com/services/T/B/x\n",
                &Default::default(),
            )
            .unwrap();
        n.create(&org, slack("GOOD")).unwrap();
        assert!(n.create(&org, slack("GOOD")).is_err(), "duplicate");
        let c = n.update(&org, "s", None, None, Some(false)).unwrap();
        assert!(!c.enabled);
        assert_eq!(n.list(&org).unwrap().len(), 1);
        // Another org sees none of it.
        assert!(n.list(&OrgId::new("beta").unwrap()).unwrap().is_empty());
        n.delete(&org, "s").unwrap();
        assert!(n.get(&org, "s").is_err());
    }
}
