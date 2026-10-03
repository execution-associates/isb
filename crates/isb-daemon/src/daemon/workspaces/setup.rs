//! The workspace's first-boot setup script: run once as root on the first
//! start after a create or a rebuild (and again on request), on a thread of
//! its own, with its outcome and the tail of its output in the history.
//! The state machine is [`crate::workspace::setup_next`].

use std::collections::BTreeSet;

use super::*;
use crate::workspace::{SetupEvent, setup_due, setup_next};

/// Where the script is pushed inside the machine, and how long it may run.
const SETUP_PATH: &str = "/run/isb/setup.sh";
const SETUP_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// Output lines kept in the history row.
const KEEP_LINES: usize = 200;

/// Setups running now, by `<org>/<name>`.
static RUNNING: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// How the script runs: as its own program when it starts with `#!`, else
/// with /bin/sh.
pub(super) fn setup_argv(script: &str) -> Vec<&'static str> {
    if script.starts_with("#!") {
        vec![SETUP_PATH]
    } else {
        vec!["/bin/sh", SETUP_PATH]
    }
}

/// A setup script as a call gives it: `""` (or blank) is none.
pub(super) fn check_setup(s: Option<String>) -> Result<Option<String>> {
    match s {
        Some(s) if s.len() > ws::MAX_SETUP => Err(Error::invalid(format!(
            "setup: {} bytes; at most {}",
            s.len(),
            ws::MAX_SETUP
        ))),
        Some(s) if s.trim().is_empty() => Ok(None),
        other => Ok(other),
    }
}

/// The last `n` lines of the script's output, stdout then stderr.
pub(super) fn tail(out: &crate::exec::ExecOutput, n: usize) -> Vec<String> {
    let mut lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .chain(String::from_utf8_lossy(&out.stderr).lines())
        .map(str::to_string)
        .collect();
    if lines.len() > n {
        lines.drain(..lines.len() - n);
    }
    lines
}

impl Workspaces {
    /// Move a stored workspace's setup state (under the workspace lock).
    pub(super) fn setup_event(&self, org: &OrgId, name: &str, ev: SetupEvent) -> Result<Workspace> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut w = load(self, org, name)?;
        w.setup_state = setup_next(w.setup.is_some(), w.setup_state.as_ref(), ev, now())?;
        self.store.put(org, &w)?;
        Ok(w)
    }

    /// The daemon started: a run it had begun is gone.
    pub(super) fn setups_interrupted(&self) {
        for org in self.store.orgs() {
            for w in self.store.list(&org).unwrap_or_default() {
                if w.setup_state.as_ref().map(|s| s.status) == Some(ws::SetupStatus::Running) {
                    let _ = self.setup_event(&org, &w.name, SetupEvent::DaemonStarted);
                }
            }
        }
    }

    /// Run the workspace's setup script now if it is due and the machine is
    /// running, on a thread of its own. Idempotent: one run at a time.
    pub(super) fn kick_setup(self: &Arc<Self>, org: &OrgId, name: &str) {
        let Ok(Some(w)) = self.store.get(org, name) else {
            return;
        };
        if !setup_due(&w) {
            return;
        }
        let key = format!("{org}/{name}");
        if !RUNNING
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.clone())
        {
            return;
        }
        let done = |k: &str| {
            RUNNING.lock().unwrap_or_else(|e| e.into_inner()).remove(k);
        };
        let (me, org, name, k) = (self.clone(), org.clone(), name.to_string(), key.clone());
        let spawned = std::thread::Builder::new()
            .name(format!("isb-setup-{name}"))
            .spawn(move || {
                me.run_setup(&org, &name);
                done(&k);
            });
        if spawned.is_err() {
            done(&key);
        }
    }

    fn run_setup(&self, org: &OrgId, name: &str) {
        let running = Sandbox::get(&self.oc(org), name)
            .and_then(|sb| sb.info())
            .is_ok_and(|i| i.status.eq_ignore_ascii_case("running"));
        if !running {
            return;
        }
        let w = match self.setup_event(org, name, SetupEvent::Started) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("isb serve: workspace {org}/{name}: setup not started: {e}");
                return;
            }
        };
        let script = w.setup.clone().unwrap_or_default();
        let started = std::time::Instant::now();
        let r = self.exec_setup(org, &w, &script);
        let secs = started.elapsed().as_secs();
        let (ev, lines) = match r {
            Ok(out) => (SetupEvent::Finished(out.exit_code), tail(&out, KEEP_LINES)),
            Err(e) => (SetupEvent::Error(e.to_string()), Vec::new()),
        };
        let st = self
            .setup_event(org, name, ev)
            .ok()
            .and_then(|w| w.setup_state);
        let ok = st
            .as_ref()
            .is_some_and(|s| s.status == ws::SetupStatus::Succeeded);
        let what = match &st {
            Some(s) if ok => format!("succeeded in {secs}s (run {})", s.runs),
            Some(s) => match (&s.exit_code, &s.message) {
                (Some(c), _) => format!("failed with exit {c} after {secs}s (run {})", s.runs),
                (_, Some(m)) => format!("failed: {m}"),
                _ => "failed".into(),
            },
            None => "ended".into(),
        };
        eprintln!("isb serve: workspace {org}/{name}: setup script {what}");
        self.recorder.record(crate::history::NewRecord {
            source: "controller".into(),
            org: Some(org.as_str().to_string()),
            project: Some(org.incus_project()),
            kind: "workspace.setup".into(),
            object_type: Some("workspace".into()),
            object: Some(name.to_string()),
            objects: vec![name.to_string()],
            actor: Some("isb".into()),
            level: Some(if ok { "info" } else { "error" }.into()),
            message: Some(format!("workspace {name}: setup script {what}")),
            details: json!({"state": st, "seconds": secs, "output": lines}),
            ..Default::default()
        });
    }

    /// Push the script and run it as root, in the root's home, with the
    /// workspace's names in the environment.
    fn exec_setup(
        &self,
        org: &OrgId,
        w: &Workspace,
        script: &str,
    ) -> Result<crate::exec::ExecOutput> {
        let oc = self.oc(org);
        let inst = w.instance();
        oc.make_dir(inst, "/run/isb", 0, 0, 0o755)?;
        oc.push_file(inst, SETUP_PATH, script.as_bytes(), 0, 0, 0o700)?;
        let sb = Sandbox::get(&oc, inst)?;
        let opts = crate::exec::ExecOptions::default()
            .cwd("/root")
            .env("HOME", "/root")
            .env("DEBIAN_FRONTEND", "noninteractive")
            .env(
                "PATH",
                "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            )
            .env("ISB_ORG", org.as_str())
            .env("ISB_WORKSPACE", &w.name)
            .env("ISB_USER", &w.user)
            .env("ISB_HOME", w.home_dir())
            .timeout(SETUP_TIMEOUT);
        let r = match sb.exec_with(setup_argv(script), opts) {
            // incus fails the operation, rather than reporting the code,
            // when a command ends with 126 or 127.
            Err(e) if e.to_string().contains("Command not found") => Ok(exit(127)),
            Err(e) if e.to_string().contains("Permission denied") => Ok(exit(126)),
            r => r,
        };
        let _ = sb.exec_with(
            ["rm", "-f", SETUP_PATH],
            crate::exec::ExecOptions::default().timeout(Duration::from_secs(30)),
        );
        r
    }
}

fn exit(code: i32) -> crate::exec::ExecOutput {
    crate::exec::ExecOutput {
        exit_code: code,
        stdout: Vec::new(),
        stderr: Vec::new(),
    }
}

/// `workspace_setup_run`: run the setup script again (now when the machine
/// is running, else on its next start).
pub(super) fn workspace_setup_run(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    let org = super::super::arg_org(&a)?;
    let a: NameArgs = args(a)?;
    require(c, &org, Role::Admin, "running the setup script")?;
    let wsm = d.workspaces.clone();
    let name = resolve_name(&wsm, &org, a.name.as_deref())?;
    let w = wsm.setup_event(&org, &name, SetupEvent::Requested)?;
    wsm.record(
        &org,
        "workspace.setup_requested",
        &name,
        &creator(c),
        format!("workspace {name}: setup script requested by {}", creator(c)),
        json!({}),
    );
    wsm.kick_setup(&org, &name);
    Ok(json!({
        "name": name,
        "setup_state": w.setup_state,
        "message": "The setup script runs as root now if the workspace is running, else on its next start; its outcome and output are in the workspace's history.",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_scripts_run_as_their_own_program_or_with_sh() {
        assert_eq!(setup_argv("#!/bin/bash\necho"), [SETUP_PATH]);
        assert_eq!(
            setup_argv("apt-get install -y htop"),
            ["/bin/sh", SETUP_PATH]
        );
        assert_eq!(check_setup(Some("  \n".into())).unwrap(), None);
        assert_eq!(check_setup(None).unwrap(), None);
        assert!(check_setup(Some("x".repeat(ws::MAX_SETUP + 1))).is_err());
        let out = crate::exec::ExecOutput {
            exit_code: 0,
            stdout: b"a\nb\nc\n".to_vec(),
            stderr: b"warn\n".to_vec(),
        };
        assert_eq!(tail(&out, 2), ["c", "warn"]);
        assert_eq!(tail(&out, 10).len(), 4);
    }
}
