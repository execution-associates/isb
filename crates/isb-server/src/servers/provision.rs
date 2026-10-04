//! Progress of a server being added: over SSH (`server_add`) or as a
//! dedicated VM this control plane makes itself (`org_create` with
//! `placement: {vm: ...}`). Each run is a fixed list of steps, the lines it
//! logged and how it ended, kept in memory so the web UI and the CLI can
//! follow one that runs in the background (`server_provision_get`, and
//! `provisions` in `server_list`). Nothing secret goes in: the SSH key never
//! reaches a log line.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::Value;

use crate::stack::now_secs;

/// Lines of log kept per run.
const MAX_LOG: usize = 400;
/// How long a finished run stays listed: failed ones longer, so they can be
/// read and retried.
const KEEP_DONE: u64 = 10 * 60;
const KEEP_FAILED: u64 = 60 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    Pending,
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct Step {
    pub id: &'static str,
    pub title: &'static str,
    pub state: StepState,
    pub started_at: Option<u64>,
    pub finished_at: Option<u64>,
}

/// How a server is being made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// `server_add`: a box reached over SSH.
    Ssh,
    /// A dedicated VM on this control plane's host, for one org.
    Vm,
}

/// The steps of each kind, in order.
pub fn steps(kind: Kind) -> &'static [(&'static str, &'static str)] {
    match kind {
        Kind::Ssh => &[
            ("check", "Check the box over SSH"),
            ("binary", "Get the isb binary"),
            ("upload", "Upload isb"),
            ("install", "Install incus, the agent and its unit"),
            ("agent", "Wait for the agent's heartbeat"),
        ],
        Kind::Vm => &[
            ("support", "Check this host can run VMs"),
            ("vm", "Create and start the VM"),
            ("boot", "Wait for the VM to boot"),
            ("upload", "Copy isb into the VM"),
            ("install", "Install incus, the agent and its firewall"),
            ("agent", "Wait for the agent's heartbeat"),
            ("org", "Create the org on it"),
        ],
    }
}

/// One run, as the tools answer it.
#[derive(Debug, Clone, Serialize)]
pub struct View {
    /// The server's name (`vm-<org>` for a dedicated VM).
    pub name: String,
    pub kind: Kind,
    /// The org a dedicated VM is for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org: Option<String>,
    pub state: RunState,
    pub started_at: u64,
    pub finished_at: Option<u64>,
    pub steps: Vec<Step>,
    pub log: Vec<String>,
    /// Lines dropped off the top of `log` (the first kept line's number).
    pub log_start: usize,
    pub error: Option<String>,
    /// What was asked, without secrets: enough for "retry" to ask again.
    pub request: Value,
    /// What the run produced (the server, or the org it created).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
}

/// A handle on one run: cheap to clone, shared with the thread doing it.
#[derive(Debug, Clone)]
pub struct Provision(Arc<Mutex<View>>);

impl Provision {
    pub fn new(name: &str, kind: Kind, org: Option<&str>, request: Value) -> Provision {
        Provision(Arc::new(Mutex::new(View {
            name: name.to_string(),
            kind,
            org: org.map(str::to_string),
            state: RunState::Running,
            started_at: now_secs(),
            finished_at: None,
            steps: steps(kind)
                .iter()
                .map(|(id, title)| Step {
                    id,
                    title,
                    state: StepState::Pending,
                    started_at: None,
                    finished_at: None,
                })
                .collect(),
            log: Vec::new(),
            log_start: 0,
            error: None,
            request,
            result: None,
        })))
    }

    pub fn view(&self) -> View {
        self.0.lock().unwrap().clone()
    }

    /// Start step `id`: the one running before it is done.
    pub fn step(&self, id: &str) {
        let now = now_secs();
        let mut v = self.0.lock().unwrap();
        for s in v.steps.iter_mut() {
            if s.state == StepState::Running {
                s.state = StepState::Done;
                s.finished_at = Some(now);
            }
        }
        if let Some(s) = v.steps.iter_mut().find(|s| s.id == id) {
            s.state = StepState::Running;
            s.started_at = Some(now);
        }
    }

    /// A line of progress (also the daemon's log).
    pub fn log(&self, line: &str) {
        let mut v = self.0.lock().unwrap();
        eprintln!("isb serve: server {}: {line}", v.name);
        v.log.push(line.to_string());
        let over = v.log.len().saturating_sub(MAX_LOG);
        if over > 0 {
            v.log.drain(..over);
            v.log_start += over;
        }
    }

    /// The run ended: every step done, or the running one failed.
    pub fn finish(&self, r: std::result::Result<Value, String>) {
        let now = now_secs();
        let mut v = self.0.lock().unwrap();
        v.finished_at = Some(now);
        match r {
            Ok(result) => {
                for s in v.steps.iter_mut() {
                    if s.state != StepState::Done {
                        s.state = StepState::Done;
                        s.started_at.get_or_insert(now);
                        s.finished_at = Some(now);
                    }
                }
                v.state = RunState::Done;
                v.result = Some(result);
            }
            Err(e) => {
                for s in v.steps.iter_mut() {
                    if s.state == StepState::Running {
                        s.state = StepState::Failed;
                        s.finished_at = Some(now);
                    }
                }
                v.state = RunState::Failed;
                v.error = Some(e);
            }
        }
    }

    pub fn running(&self) -> bool {
        self.0.lock().unwrap().state == RunState::Running
    }
}

/// Every run by server name: one at a time per name.
#[derive(Debug, Default)]
pub struct Runs(Mutex<BTreeMap<String, Provision>>);

impl Runs {
    /// Start a run for `name`, refused while one is running.
    pub fn begin(&self, p: Provision) -> crate::error::Result<Provision> {
        let mut m = self.0.lock().unwrap();
        let name = p.view().name;
        if m.get(&name).is_some_and(Provision::running) {
            return Err(crate::error::Error::AlreadyExists(format!(
                "server {name} is being added already (server_provision_get shows how far it got)"
            )));
        }
        m.insert(name, p.clone());
        Ok(p)
    }

    pub fn get(&self, name: &str) -> Option<View> {
        self.0.lock().unwrap().get(name).map(Provision::view)
    }

    /// Runs going on, and recent finished ones (failed ones for an hour).
    pub fn list(&self) -> Vec<View> {
        let now = now_secs();
        let mut m = self.0.lock().unwrap();
        m.retain(|_, p| {
            let v = p.view();
            match (v.state, v.finished_at) {
                (RunState::Running, _) | (_, None) => true,
                (RunState::Done, Some(t)) => now.saturating_sub(t) < KEEP_DONE,
                (RunState::Failed, Some(t)) => now.saturating_sub(t) < KEEP_FAILED,
            }
        });
        m.values().map(Provision::view).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn steps_advance_and_a_failure_marks_the_running_one() {
        let p = Provision::new("vm-acme", Kind::Vm, Some("acme"), json!({"org": "acme"}));
        p.step("support");
        p.step("vm");
        let v = p.view();
        assert_eq!(v.steps[0].state, StepState::Done);
        assert_eq!(v.steps[1].state, StepState::Running);
        assert_eq!(v.steps[2].state, StepState::Pending);
        p.finish(Err("no kvm".into()));
        let v = p.view();
        assert_eq!(v.state, RunState::Failed);
        assert_eq!(v.steps[1].state, StepState::Failed);
        assert_eq!(v.steps[2].state, StepState::Pending);
        assert_eq!(v.error.as_deref(), Some("no kvm"));

        let ok = Provision::new("box", Kind::Ssh, None, json!({}));
        ok.step("check");
        ok.finish(Ok(json!({"name": "box"})));
        assert!(ok.view().steps.iter().all(|s| s.state == StepState::Done));
    }

    #[test]
    fn one_run_per_name_and_the_log_is_bounded() {
        let runs = Runs::default();
        let a = runs
            .begin(Provision::new("box", Kind::Ssh, None, json!({})))
            .unwrap();
        assert!(
            runs.begin(Provision::new("box", Kind::Ssh, None, json!({})))
                .is_err()
        );
        a.finish(Err("x".into()));
        runs.begin(Provision::new("box", Kind::Ssh, None, json!({})))
            .unwrap();
        for i in 0..(MAX_LOG + 10) {
            a.log(&format!("line {i}"));
        }
        assert_eq!(a.view().log.len(), MAX_LOG);
        assert_eq!(a.view().log_start, 10);
        assert_eq!(a.view().log[0], "line 10");
        assert_eq!(runs.list().len(), 1);
    }
}
