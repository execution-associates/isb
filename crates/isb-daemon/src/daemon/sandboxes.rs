//! Sandboxes in the daemon (docs/concepts/sandboxes.md#sandboxes-are-short-lived):
//! the activity isb sees on instances (exec, terminals, CPU), the org's
//! sandbox defaults and the `sandbox_settings` tool, and the reaper that
//! deletes sandboxes past their expiry or idle timeout, only those isb gave
//! deadlines, never a replica, and records each in the history.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{Daemon, args, obj};
use crate::client::{Client, encode_segment};
use crate::error::Result;
use crate::org::OrgId;
use crate::sandbox::Sandbox;
use crate::sandbox_life::{self as life, Settings};
use crate::server::{Caller, Registry, Tool};

/// How often the reaper runs.
const REAP_EVERY: Duration = Duration::from_secs(60);
/// A sandbox using this much of a core is active.
const ACTIVE_CPU_PCT: f32 = 2.0;
/// How long an isbx sandbox stays running after its last run, unless its
/// `isbx.idle` label says otherwise.
const ISBX_IDLE: Duration = Duration::from_secs(30 * 60);

/// A sandbox's root disk size when it gives none in an org with a disk
/// quota.
pub const SANDBOX_ROOT_SIZE: &str = crate::org::DEFAULT_ROOT_SIZE;

const OUTPUT_CAP: usize = 256 * 1024;

/// The last [`OUTPUT_CAP`] bytes of an exec's output, and whether it was cut.
pub(super) fn cap(b: &[u8]) -> (String, bool) {
    if b.len() <= OUTPUT_CAP {
        return (String::from_utf8_lossy(b).into_owned(), false);
    }
    (
        String::from_utf8_lossy(&b[b.len() - OUTPUT_CAP..]).into_owned(),
        true,
    )
}

fn now() -> u64 {
    crate::stack::now_secs()
}

fn key(project: &str, instance: &str) -> String {
    format!("{project}/{instance}")
}

/// Live sessions on an instance (web terminals), counted while their guard
/// lives; the reaper leaves an instance with one alone.
pub struct SessionGuard {
    key: String,
    sessions: Arc<Mutex<HashMap<String, usize>>>,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        let mut m = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = m.get_mut(&self.key) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                m.remove(&self.key);
            }
        }
    }
}

/// The daemon's sandboxes: activity, defaults and the reaper.
pub struct Sandboxes {
    state_dir: PathBuf,
    client: Client,
    recorder: Arc<crate::history::Recorder>,
    sessions: Arc<Mutex<HashMap<String, usize>>>,
    /// The latest activity isb saw per `<project>/<instance>`: exec, a
    /// terminal, CPU use.
    activity: Mutex<HashMap<String, u64>>,
    started: u64,
}

impl Sandboxes {
    pub fn new(
        state_dir: &Path,
        client: Client,
        recorder: Arc<crate::history::Recorder>,
    ) -> Arc<Sandboxes> {
        Arc::new(Sandboxes {
            state_dir: state_dir.to_path_buf(),
            client,
            recorder,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            activity: Mutex::new(HashMap::new()),
            started: now(),
        })
    }

    /// Count a session on an instance while the guard lives.
    pub fn session(&self, project: &str, instance: &str) -> SessionGuard {
        let k = key(project, instance);
        *self
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(k.clone())
            .or_insert(0) += 1;
        self.mark_active(project, instance);
        SessionGuard {
            key: k,
            sessions: self.sessions.clone(),
        }
    }

    fn terminals(&self, project: &str, instance: &str) -> usize {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key(project, instance))
            .copied()
            .unwrap_or(0)
    }

    /// isb saw the instance used (exec, a terminal).
    pub fn mark_active(&self, project: &str, instance: &str) {
        self.activity
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key(project, instance), now());
    }

    /// The latest activity isb saw on an instance, if any.
    pub fn last_seen(&self, project: &str, instance: &str) -> Option<u64> {
        self.activity
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key(project, instance))
            .copied()
    }

    fn last_active(&self, project: &str, instance: &str) -> u64 {
        // A daemon that just started has seen nothing yet: it counts from
        // its own start, never from before.
        self.last_seen(project, instance)
            .unwrap_or(0)
            .max(self.started)
    }

    /// The org's sandbox defaults.
    pub fn settings(&self, org: &OrgId) -> Result<Settings> {
        life::settings(&self.state_dir, org)
    }

    /// The reaper, on a thread of its own.
    pub fn start(self: &Arc<Self>, ctl: crate::stack::Controller) {
        let me = self.clone();
        let _ = std::thread::Builder::new()
            .name("isb-sandboxes".into())
            .spawn(move || {
                let mut warned = HashSet::new();
                loop {
                    std::thread::sleep(REAP_EVERY);
                    me.warn_unmanaged(&ctl, &mut warned);
                    me.reap(&ctl);
                    me.stop_idle_isbx(&ctl);
                }
            });
    }

    /// Say once per instance that an isb 2 workspace is no longer managed:
    /// isb leaves it running, and never reaps it (it has no deadlines).
    fn warn_unmanaged(&self, ctl: &crate::stack::Controller, warned: &mut HashSet<String>) {
        for i in ctl.snapshot().instances.values() {
            if i.labels.contains_key("isb.workspace") && warned.insert(key(&i.project, &i.name)) {
                eprintln!(
                    "isb serve: {}/{} was an isb workspace; isb no longer manages it. Deploy a stack in its place, then remove it with: incus delete --force --project {} {}",
                    i.project, i.name, i.project, i.name
                );
            }
        }
    }

    fn record(&self, org: &OrgId, object: &str, msg: String, d: Value) {
        self.recorder.record(crate::history::NewRecord {
            source: "controller".into(),
            org: Some(org.as_str().to_string()),
            project: Some(org.incus_project()),
            kind: "sandbox.reaped".into(),
            object_type: Some("instance".into()),
            object: Some(object.to_string()),
            objects: vec![object.to_string()],
            actor: Some("isb".into()),
            level: Some("info".into()),
            message: Some(msg),
            details: d,
            ..Default::default()
        });
    }

    /// Stop isbx's sandboxes (`isb x`: outside every org, kept between
    /// runs) that no run has used for their `isbx.idle` (default 30m), so
    /// they hold no memory until the next run starts one again, in about a
    /// second. Never deletes; leaves alone one with a run going, and one
    /// isbx never marked as used here (another user's, or one made by hand).
    fn stop_idle_isbx(&self, ctl: &crate::stack::Controller) {
        for i in ctl.snapshot().instances.values() {
            if !i.labels.contains_key(crate::x::LABEL_KIND)
                || !i.status.eq_ignore_ascii_case("running")
                || OrgId::from_incus_project(&i.project).is_some()
            {
                continue;
            }
            let idle = i
                .labels
                .get("isbx.idle")
                .and_then(|d| crate::flex::parse_duration(d).ok())
                .unwrap_or(ISBX_IDLE);
            let quiet = crate::x::last_used(&i.name).and_then(|t| t.elapsed().ok());
            if quiet.is_none_or(|q| q < idle) || crate::x::live::busy(&i.name) {
                continue;
            }
            let c = self.client.clone().project(&i.project);
            match Sandbox::get(&c, &i.name).and_then(|sb| sb.stop(false, Duration::from_secs(30))) {
                Ok(()) => eprintln!(
                    "isb serve: stopped isbx sandbox {} (unused for {}m)",
                    i.name,
                    quiet.unwrap_or_default().as_secs() / 60
                ),
                Err(e) if e.is_not_found() => {}
                Err(e) => eprintln!("isb serve: could not stop idle {}: {e}", i.name),
            }
        }
    }

    /// Delete sandboxes past their expiry or idle timeout. Idempotent: an
    /// instance already gone is not an error.
    fn reap(&self, ctl: &crate::stack::Controller) {
        let snap = ctl.snapshot();
        let t = now();
        for i in snap.instances.values() {
            if i.cpu_pct.is_some_and(|c| c >= ACTIVE_CPU_PCT) {
                self.mark_active(&i.project, &i.name);
            }
        }
        for i in snap.instances.values() {
            if self.terminals(&i.project, &i.name) > 0 {
                continue;
            }
            let Some(reason) =
                life::reap_reason(&i.labels, t, self.last_active(&i.project, &i.name))
            else {
                continue;
            };
            let Some(org) = OrgId::from_incus_project(&i.project) else {
                continue;
            };
            let oc = crate::org::client(&self.client, &org);
            // Read it again: the sample may be stale, and only an instance
            // that still has the deadline it was sampled with is taken.
            let Ok(sb) = Sandbox::get(&oc, &i.name) else {
                continue;
            };
            let Ok(info) = sb.info() else { continue };
            let labels: BTreeMap<String, String> = info
                .config
                .iter()
                .filter_map(|(k, v)| k.strip_prefix("user.").map(|k| (k.to_string(), v.clone())))
                .collect();
            if life::reap_reason(&labels, t, self.last_active(&i.project, &i.name)).is_none() {
                continue;
            }
            match Sandbox::remove(&oc, &i.name, true) {
                Ok(()) => {
                    eprintln!("isb serve: reaped sandbox {org}/{} ({reason})", i.name);
                    self.record(
                        &org,
                        &i.name,
                        format!("sandbox {} deleted: {reason}", i.name),
                        json!({
                            "reason": reason,
                            "owner": labels.get("isb.owner"),
                            "expires_at": labels.get("isb.expires_at"),
                            "idle_timeout": labels.get("isb.idle_timeout"),
                        }),
                    );
                }
                Err(e) if e.is_not_found() => {}
                Err(e) => eprintln!("isb serve: could not reap {org}/{}: {e}", i.name),
            }
            self.activity
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&key(&i.project, &i.name));
        }
    }
}

/// Whether the org's project has a disk quota (`limits.disk`).
pub fn project_has_disk_limit(client: &Client, org: &OrgId) -> bool {
    client
        .get_opt(&format!(
            "/1.0/projects/{}",
            encode_segment(&org.incus_project())
        ))
        .ok()
        .flatten()
        .is_some_and(|p| p["config"]["limits.disk"].as_str().is_some())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsArgs {
    #[serde(default)]
    #[allow(dead_code)]
    org: Option<String>,
    #[serde(default)]
    sandbox_expiry: Option<String>,
    #[serde(default)]
    sandbox_idle: Option<String>,
}

fn sandbox_settings(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    let org = super::arg_org(&a)?;
    let a: SettingsArgs = args(a)?;
    let mut s = d.sandboxes.settings(&org)?;
    if a.sandbox_expiry.is_some() || a.sandbox_idle.is_some() {
        super::volumes::require_admin(c, &org, "changing the sandbox defaults")?;
        if let Some(e) = a.sandbox_expiry {
            s.sandbox_expiry = e.trim().to_string();
        }
        if let Some(i) = a.sandbox_idle {
            s.sandbox_idle = i.trim().to_string();
        }
        life::put_settings(&d.state_dir, &org, &s)?;
    }
    Ok(json!({"org": org.as_str(), "settings": s}))
}

pub(super) fn register(r: &mut Registry, d: Arc<Daemon>) -> Result<()> {
    r.register(
        Tool::new(
            "sandbox_settings",
            "The org's defaults for new sandboxes: sandbox_expiry (24h, at most 30d), how long one lives, and sandbox_idle (2h, or none), how long it may sit idle before the reaper deletes it. sandbox_create's expires and idle_timeout override them per sandbox. Without changes it reads them; org admins and above change them.",
            obj(
                json!({
                    "sandbox_expiry": {"type": "string", "description": "e.g. 24h, 7d; at most 30d."},
                    "sandbox_idle": {"type": "string", "description": "e.g. 2h, or none."}
                }),
                &[],
            ),
            move |a, c| {
                crate::org::check_exists(&d.client, &super::arg_org(&a)?)?;
                sandbox_settings(&d, a, c)
            },
        )
        .title("Sandbox defaults")
        .annotations(json!({"destructiveHint": false, "openWorldHint": false})),
    )?;
    Ok(())
}
