//! What the daemon keeps beside a compose stack's definition: its
//! environment (`.env` text that `${VAR}` resolves against), its managed
//! domains (domain records per service, merged in at deploy), and a record
//! of each deploy ([`Deployment`]), kept per stack under
//! `stack-meta/<stack>/` in the stack's org directory.
//!
//! A record is written when a deploy is handed to the controller
//! (`deploying`) and finished by a watcher thread that follows the rollout:
//! `done` when every service converged, `failed` when one fails or pauses
//! or the rollout outlasts its timeout, `superseded` when a newer deploy
//! came first. It keeps the compose source it deployed, the environment
//! and managed domains of the moment (secret references, never values),
//! and the stack's events while it ran.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::Controller;
use super::now_secs;
use crate::error::{Error, Result};
use crate::org::OrgId;
use crate::spec::DomainSpec;

/// Deployment records kept per stack, as for an app.
pub const KEEP_DEPLOYMENTS: usize = 30;

/// Events kept per record.
const EVENTS_KEPT: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Queued,
    Deploying,
    Done,
    Failed,
    /// A newer deploy came before this one's rollout settled.
    Superseded,
}

impl Status {
    pub fn finished(self) -> bool {
        !matches!(self, Status::Queued | Status::Deploying)
    }
}

/// One line of a deployment's log: an event of its stack.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogLine {
    /// Unix milliseconds.
    pub at: u64,
    pub level: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub service: String,
    pub message: String,
}

/// One deploy of a compose stack.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Deployment {
    pub id: u64,
    pub stack: String,
    /// `manual` (the local CLI) or `api` (a tool call over MCP or REST).
    pub trigger: String,
    /// What deployed: `deploy` (a compose file), `rollback`, `env` (an
    /// environment change) or `domains` (a managed domains change).
    pub action: String,
    /// The caller.
    pub actor: String,
    pub status: Status,
    /// A rollback to a kept deployment: its id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback_of: Option<u64>,
    /// The services it created, changed, scaled or removed.
    #[serde(default)]
    pub services: Vec<String>,
    /// `file:`/`environment:` secrets it deployed with the value an
    /// earlier deploy stored, because it was given none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reused_secrets: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Unix seconds, as are `started_at` and `finished_at`.
    pub created_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<u64>,
    /// The compose source it deployed, as stack_export gives it.
    #[serde(default)]
    pub source: String,
    /// Where the source's relative paths resolved.
    #[serde(default)]
    pub base_dir: PathBuf,
    /// The stack's environment at the time (`.env` text).
    #[serde(default)]
    pub env: String,
    /// The managed domains at the time, per service.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub domains: BTreeMap<String, Vec<DomainSpec>>,
    /// The stack's events while it ran.
    #[serde(default)]
    pub events: Vec<LogLine>,
    /// The event feed's sequence number it has read up to.
    #[serde(default)]
    pub events_seq: u64,
}

impl Deployment {
    /// The record for listings: without the source, environment, domains
    /// and events.
    pub fn summary(&self) -> Value {
        let mut v = serde_json::to_value(self).unwrap_or_default();
        if let Some(o) = v.as_object_mut() {
            for k in [
                "source",
                "base_dir",
                "env",
                "domains",
                "events",
                "events_seq",
            ] {
                o.remove(k);
            }
        }
        v
    }

    /// The events as text, one line each.
    pub fn log(&self) -> String {
        let mut out = String::new();
        for l in &self.events {
            let secs = (l.at / 1000) % 86_400;
            out.push_str(&format!(
                "{:02}:{:02}:{:02} {}{}{}\n",
                secs / 3600,
                (secs / 60) % 60,
                secs % 60,
                if l.level == "info" || l.level == "log" {
                    String::new()
                } else {
                    format!("[{}] ", l.level)
                },
                if l.service.is_empty() {
                    String::new()
                } else {
                    format!("{}: ", l.service)
                },
                l.message
            ));
        }
        out
    }
}

/// The per-stack settings and records, for every org.
#[derive(Clone)]
pub struct StackMeta {
    dir: PathBuf,
    /// Held across read-modify-write of a record.
    edit: Arc<Mutex<()>>,
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn read_opt(path: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

impl StackMeta {
    /// Under the daemon's state directory.
    pub fn new(state: &Path) -> StackMeta {
        StackMeta {
            dir: state.to_path_buf(),
            edit: Arc::new(Mutex::new(())),
        }
    }

    fn stack_dir(&self, org: &OrgId, name: &str) -> PathBuf {
        let base = if org.is_default() {
            self.dir.clone()
        } else {
            org.dir(&self.dir)
        };
        base.join("stack-meta").join(name)
    }

    fn deployments_dir(&self, org: &OrgId, name: &str) -> PathBuf {
        self.stack_dir(org, name).join("deployments")
    }

    /// Whether anything is kept for the stack.
    pub fn exists(&self, org: &OrgId, name: &str) -> bool {
        self.stack_dir(org, name).is_dir()
    }

    /// Forget everything kept for a removed stack.
    pub fn remove(&self, org: &OrgId, name: &str) -> Result<()> {
        match std::fs::remove_dir_all(self.stack_dir(org, name)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    // --- environment and domains -------------------------------------------

    /// The stack's environment as stored (`.env` text; empty: none).
    pub fn env(&self, org: &OrgId, name: &str) -> Result<String> {
        let b = read_opt(&self.stack_dir(org, name).join("env"))?;
        Ok(b.map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default())
    }

    pub fn set_env(&self, org: &OrgId, name: &str, text: &str) -> Result<()> {
        write_private(&self.stack_dir(org, name).join("env"), text.as_bytes())
    }

    /// The stack's managed domains per service.
    pub fn domains(&self, org: &OrgId, name: &str) -> Result<BTreeMap<String, Vec<DomainSpec>>> {
        match read_opt(&self.stack_dir(org, name).join("domains.json"))? {
            Some(b) => Ok(serde_json::from_slice(&b)?),
            None => Ok(BTreeMap::new()),
        }
    }

    pub fn set_domains(
        &self,
        org: &OrgId,
        name: &str,
        domains: &BTreeMap<String, Vec<DomainSpec>>,
    ) -> Result<()> {
        let kept: BTreeMap<&String, &Vec<DomainSpec>> =
            domains.iter().filter(|(_, d)| !d.is_empty()).collect();
        write_private(
            &self.stack_dir(org, name).join("domains.json"),
            serde_json::to_string_pretty(&kept)?.as_bytes(),
        )
    }

    // --- deployments --------------------------------------------------------

    /// The stack's deployments, newest first.
    pub fn deployments(&self, org: &OrgId, name: &str) -> Result<Vec<Deployment>> {
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir(self.deployments_dir(org, name)) else {
            return Ok(out);
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "json") {
                if let Ok(d) = serde_json::from_slice::<Deployment>(&std::fs::read(&p)?) {
                    out.push(d);
                }
            }
        }
        out.sort_by_key(|a| std::cmp::Reverse(a.id));
        Ok(out)
    }

    pub fn deployment(&self, org: &OrgId, name: &str, id: u64) -> Result<Deployment> {
        let p = self.deployments_dir(org, name).join(format!("{id}.json"));
        match read_opt(&p)? {
            Some(b) => Ok(serde_json::from_slice(&b)?),
            None => Err(Error::NotFound(format!(
                "deployment {id} of stack {name} (the last {KEEP_DEPLOYMENTS} are kept)"
            ))),
        }
    }

    fn save(&self, org: &OrgId, d: &Deployment) -> Result<()> {
        write_private(
            &self
                .deployments_dir(org, &d.stack)
                .join(format!("{}.json", d.id)),
            serde_json::to_string_pretty(d)?.as_bytes(),
        )
    }

    /// Record a new deployment, `deploying` from now: numbered after the
    /// newest, which is superseded if it has not settled. Old records past
    /// [`KEEP_DEPLOYMENTS`] are pruned.
    pub fn start(&self, org: &OrgId, mut d: Deployment) -> Result<Deployment> {
        let _g = self.edit.lock().unwrap();
        let all = self.deployments(org, &d.stack)?;
        d.id = all.first().map_or(1, |x| x.id + 1);
        d.status = Status::Deploying;
        d.created_at = now_secs();
        d.started_at = Some(d.created_at);
        for mut old in all.iter().filter(|x| !x.status.finished()).cloned() {
            old.status = Status::Superseded;
            old.finished_at = Some(d.created_at);
            old.error = Some(format!("superseded by deployment {}", d.id));
            self.save(org, &old)?;
        }
        self.save(org, &d)?;
        for old in all.iter().skip(KEEP_DEPLOYMENTS.saturating_sub(1)) {
            let _ = std::fs::remove_file(
                self.deployments_dir(org, &d.stack)
                    .join(format!("{}.json", old.id)),
            );
        }
        Ok(d)
    }

    /// Change a record under the lock; a finished one is left alone.
    /// Returns it as saved.
    fn update(
        &self,
        org: &OrgId,
        name: &str,
        id: u64,
        f: impl FnOnce(&mut Deployment),
    ) -> Result<Deployment> {
        let _g = self.edit.lock().unwrap();
        let mut d = self.deployment(org, name, id)?;
        if !d.status.finished() {
            f(&mut d);
            if d.status.finished() && d.finished_at.is_none() {
                d.finished_at = Some(now_secs());
            }
            self.save(org, &d)?;
        }
        Ok(d)
    }

    /// Mark a deployment failed (the controller refused it).
    pub fn fail(&self, org: &OrgId, name: &str, id: u64, error: &str) -> Result<Deployment> {
        self.update(org, name, id, |d| {
            d.status = Status::Failed;
            d.error = Some(error.to_string());
        })
    }

    /// Deployments a stopped daemon left unfinished are marked failed.
    pub fn recover(&self) {
        let mut roots = vec![self.dir.join("stack-meta")];
        if let Ok(rd) = std::fs::read_dir(self.dir.join("orgs")) {
            roots.extend(rd.flatten().map(|e| e.path().join("stack-meta")));
        }
        for root in roots {
            let Ok(stacks) = std::fs::read_dir(&root) else {
                continue;
            };
            for s in stacks.flatten() {
                let Ok(rd) = std::fs::read_dir(s.path().join("deployments")) else {
                    continue;
                };
                for e in rd.flatten() {
                    let p = e.path();
                    let Ok(mut d) = std::fs::read(&p)
                        .map_err(Error::from)
                        .and_then(|b| Ok(serde_json::from_slice::<Deployment>(&b)?))
                    else {
                        continue;
                    };
                    if d.status.finished() {
                        continue;
                    }
                    d.status = Status::Failed;
                    d.finished_at = Some(now_secs());
                    d.error = Some("isb serve stopped before the rollout settled".into());
                    if let Ok(t) = serde_json::to_string_pretty(&d) {
                        let _ = write_private(&p, t.as_bytes());
                    }
                }
            }
        }
    }

    /// Follow deployment `id` of `org`/`name` in a thread until its rollout
    /// settles, collecting the stack's events into it.
    pub fn watch(&self, ctl: Controller, org: OrgId, name: String, id: u64, timeout: Duration) {
        let meta = self.clone();
        std::thread::spawn(move || meta.follow(&ctl, &org, &name, id, timeout));
    }

    fn follow(&self, ctl: &Controller, org: &OrgId, name: &str, id: u64, timeout: Duration) {
        let q = super::qualified(org, name);
        let started = Instant::now();
        loop {
            let Ok(d) = self.deployment(org, name, id) else {
                return;
            };
            if d.status.finished() {
                return;
            }
            let (_, events) = ctl.events(d.events_seq, EVENTS_KEPT);
            let seq = events.last().map_or(d.events_seq, |e| e.seq);
            let lines: Vec<LogLine> = events
                .into_iter()
                .filter(|e| e.stack == q)
                .map(|e| LogLine {
                    at: e.at,
                    level: e.level,
                    service: e.service,
                    message: e.message,
                })
                .collect();
            let outcome = match settled(ctl, &q) {
                Err(e) => Some((Status::Failed, Some(e.to_string()))),
                Ok(Some(problems)) if problems.is_empty() => Some((Status::Done, None)),
                Ok(Some(problems)) => Some((Status::Failed, Some(problems.join("; ")))),
                Ok(None) if started.elapsed() >= timeout => Some((
                    Status::Failed,
                    Some(format!(
                        "the rollout did not settle in {}s",
                        timeout.as_secs()
                    )),
                )),
                Ok(None) => None,
            };
            let r = self.update(org, name, id, |d| {
                d.events.extend(lines);
                let over = d.events.len().saturating_sub(EVENTS_KEPT);
                d.events.drain(..over);
                d.events_seq = seq;
                if let Some((s, e)) = outcome {
                    d.status = s;
                    d.error = e;
                }
            });
            match r {
                Ok(d) if !d.status.finished() => {}
                _ => return,
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    }
}

/// Whether the stack's current deployment has settled: `None` while it
/// rolls out, else what failed or paused (empty: all converged). As
/// `wait_settled` judges it: only a status of the current revision and
/// replica count counts.
fn settled(ctl: &Controller, q: &str) -> Result<Option<Vec<String>>> {
    let def = ctl.definition(q)?;
    let st = ctl.status(q)?;
    let mut problems = Vec::new();
    for s in &st.services {
        let current = def.revision(&s.service).is_ok_and(|r| r == s.rev)
            && def
                .service(&s.service)
                .is_ok_and(|d| d.replicas() == s.replicas);
        match s.state.as_str() {
            "converged" if current => {}
            "paused" | "failing" if current => problems.push(format!(
                "{}: {}{}",
                s.service,
                s.state,
                s.message
                    .as_deref()
                    .map(|m| format!(" ({m})"))
                    .unwrap_or_default()
            )),
            _ => return Ok(None),
        }
    }
    Ok(Some(problems))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(stack: &str) -> Deployment {
        Deployment {
            id: 0,
            stack: stack.into(),
            trigger: "api".into(),
            action: "deploy".into(),
            actor: "u".into(),
            status: Status::Queued,
            rollback_of: None,
            services: vec!["web".into()],
            reused_secrets: vec![],
            error: None,
            created_at: 0,
            started_at: None,
            finished_at: None,
            source: "services: {}\n".into(),
            base_dir: "/srv".into(),
            env: "A=1\n".into(),
            domains: BTreeMap::new(),
            events: vec![],
            events_seq: 0,
        }
    }

    #[test]
    fn records_number_supersede_prune_and_recover() {
        let dir = tempfile::tempdir().unwrap();
        let m = StackMeta::new(dir.path());
        let org = OrgId::new("acme").unwrap();
        let a = m.start(&org, rec("shop")).unwrap();
        assert_eq!((a.id, a.status), (1, Status::Deploying));
        let b = m.start(&org, rec("shop")).unwrap();
        assert_eq!(b.id, 2);
        let a = m.deployment(&org, "shop", 1).unwrap();
        assert_eq!(a.status, Status::Superseded);
        assert_eq!(a.error.as_deref(), Some("superseded by deployment 2"));
        // A finished record is not changed again.
        m.fail(&org, "shop", 1, "late").unwrap();
        assert_eq!(
            m.deployment(&org, "shop", 1).unwrap().status,
            Status::Superseded
        );
        for _ in 0..KEEP_DEPLOYMENTS + 3 {
            m.start(&org, rec("shop")).unwrap();
        }
        let all = m.deployments(&org, "shop").unwrap();
        assert_eq!(all.len(), KEEP_DEPLOYMENTS);
        assert_eq!(all[0].id, KEEP_DEPLOYMENTS as u64 + 5);
        assert!(m.deployment(&org, "shop", 1).is_err());
        // The summary leaves the bulk out.
        let s = all[0].summary();
        assert!(s.get("source").is_none() && s.get("events").is_none());
        assert_eq!(s["status"], "deploying");
        // Reused secrets are named only when there are some, and kept.
        assert!(s.get("reused_secrets").is_none());
        let mut r = rec("shop");
        r.reused_secrets = vec!["db_password".into()];
        let r = m.start(&org, r).unwrap();
        assert_eq!(
            r.summary()["reused_secrets"],
            serde_json::json!(["db_password"])
        );
        assert_eq!(
            m.deployment(&org, "shop", r.id).unwrap().reused_secrets,
            ["db_password"]
        );
        let all = m.deployments(&org, "shop").unwrap();
        m.recover();
        let top = m.deployment(&org, "shop", all[0].id).unwrap();
        assert_eq!(top.status, Status::Failed);
        assert!(top.finished_at.is_some());
        // Settings live beside the records and go with them.
        m.set_env(&org, "shop", "A=1\n").unwrap();
        assert_eq!(m.env(&org, "shop").unwrap(), "A=1\n");
        assert_eq!(m.env(&org, "other").unwrap(), "");
        let doms = BTreeMap::from([(
            "web".to_string(),
            vec![DomainSpec {
                host: "a.io".into(),
                ..Default::default()
            }],
        )]);
        m.set_domains(&org, "shop", &doms).unwrap();
        assert_eq!(m.domains(&org, "shop").unwrap(), doms);
        assert!(dir.path().join("orgs/acme/stack-meta/shop/env").is_file());
        m.remove(&org, "shop").unwrap();
        assert!(!m.exists(&org, "shop"));
        assert!(m.deployments(&org, "shop").unwrap().is_empty());
    }

    #[test]
    fn the_log_reads_as_lines() {
        let mut d = rec("shop");
        d.events = vec![
            LogLine {
                at: 3_600_000 + 61_000,
                level: "info".into(),
                service: String::new(),
                message: "deployed by u".into(),
            },
            LogLine {
                at: 3_600_000 + 62_000,
                level: "warn".into(),
                service: "web".into(),
                message: "unhealthy".into(),
            },
        ];
        assert_eq!(
            d.log(),
            "01:01:01 deployed by u\n01:01:02 [warn] web: unhealthy\n"
        );
    }
}
