//! The pre-snapshot hook: before isb snapshots a volume, each running
//! instance using it runs its executable `/etc/isb/pre-snapshot` (if it has
//! one), as root, with a timeout, so an image can make its own state
//! consistent first (a SQLite checkpoint, a flushed session file). Its
//! output goes to the run's log. A failure is reported; it stops the
//! snapshot only when the volume's settings say `hook_required`.

use std::time::Duration;

use serde::Serialize;

use super::model::HOOK_PATH;
use crate::error::{Error, Result};
use crate::exec::{ExecEvent, ExecOptions};
use crate::jobs::RunLog;

/// How one instance's hook went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HookStatus {
    /// No hook in the instance.
    Absent,
    Ok,
    Failed,
    TimedOut,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HookResult {
    pub instance: String,
    pub status: HookStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Where hooks run: incus, or a fake in tests.
pub trait HookRunner {
    /// Does the instance have the hook file?
    fn present(&self, instance: &str) -> Result<bool>;
    /// Run it; its exit code (a timeout is [`Error::ExecTimeout`]).
    fn run(
        &self,
        instance: &str,
        env: &[(&str, &str)],
        timeout: Duration,
        log: &mut RunLog,
    ) -> Result<i32>;
}

/// Run the hook in each of `instances` (the running ones using the volume).
/// `Err` only when a hook failed and `required` is set: the snapshot must
/// not be taken.
pub fn run_hooks(
    r: &dyn HookRunner,
    instances: &[String],
    env: &[(&str, &str)],
    (timeout, required): (Duration, bool),
    log: &mut RunLog,
) -> Result<Vec<HookResult>> {
    let mut out = Vec::new();
    for inst in instances {
        let res = one(r, inst, env, timeout, log);
        if let Some(e) = &res.error {
            log.line(&format!("isb: pre-snapshot hook in {inst}: {e}"));
            if required {
                return Err(Error::invalid(format!(
                    "the pre-snapshot hook in {inst} failed ({e}); hook_required is set, so no snapshot was taken"
                )));
            }
            log.line("isb: continuing without it (hook_required is off)");
        }
        out.push(res);
    }
    Ok(out)
}

fn one(
    r: &dyn HookRunner,
    inst: &str,
    env: &[(&str, &str)],
    timeout: Duration,
    log: &mut RunLog,
) -> HookResult {
    let res = |status, error: Option<String>| HookResult {
        instance: inst.to_string(),
        status,
        error,
    };
    match r.present(inst) {
        Ok(false) => {
            log.line(&format!("isb: {inst} has no {HOOK_PATH}"));
            return res(HookStatus::Absent, None);
        }
        Ok(true) => {}
        Err(e) => return res(HookStatus::Failed, Some(format!("looking for it: {e}"))),
    }
    log.line(&format!(
        "isb: running {HOOK_PATH} in {inst} (timeout {timeout:?})"
    ));
    match r.run(inst, env, timeout, log) {
        Ok(0) => {
            log.line(&format!("isb: {HOOK_PATH} in {inst} done"));
            res(HookStatus::Ok, None)
        }
        Ok(126) => res(
            HookStatus::Failed,
            Some(format!("{HOOK_PATH} is not executable")),
        ),
        Ok(code) => res(HookStatus::Failed, Some(format!("exited {code}"))),
        Err(Error::ExecTimeout { .. }) => res(
            HookStatus::TimedOut,
            Some(format!("timed out after {timeout:?} (killed)")),
        ),
        Err(e) => res(HookStatus::Failed, Some(e.to_string())),
    }
}

/// Hooks run through incus, in the org's project.
pub struct IncusHooks<'a>(pub &'a crate::client::Client);

impl HookRunner for IncusHooks<'_> {
    fn present(&self, instance: &str) -> Result<bool> {
        Ok(self.0.read_file(instance, HOOK_PATH)?.is_some())
    }

    fn run(
        &self,
        instance: &str,
        env: &[(&str, &str)],
        timeout: Duration,
        log: &mut RunLog,
    ) -> Result<i32> {
        let sb = crate::sandbox::Sandbox::get(self.0, instance)?;
        let mut opts = ExecOptions::default().timeout(timeout).user("0");
        for (k, v) in env {
            opts = opts.env(*k, *v);
        }
        let mut s = sb.exec_stream([HOOK_PATH], opts)?;
        while let Some(ev) = s.next_event() {
            match ev {
                ExecEvent::Stdout(b) | ExecEvent::Stderr(b) => log.write(&b),
            }
        }
        s.wait()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Per instance: `None` no hook, `Some(Ok(code))`, or a timeout.
    struct Fake(BTreeMap<&'static str, Option<std::result::Result<i32, ()>>>);

    impl HookRunner for Fake {
        fn present(&self, i: &str) -> Result<bool> {
            Ok(self.0[i].is_some())
        }
        fn run(&self, i: &str, env: &[(&str, &str)], t: Duration, log: &mut RunLog) -> Result<i32> {
            assert_eq!(env, [("ISB_VOLUME", "home")]);
            log.line("hook output");
            match self.0[i] {
                Some(Ok(c)) => Ok(c),
                _ => Err(Error::ExecTimeout {
                    argv: HOOK_PATH.into(),
                    timeout: t,
                }),
            }
        }
    }

    fn insts(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn hooks_report_and_block_only_when_required() {
        let f = Fake(BTreeMap::from([
            ("none", None),
            ("good", Some(Ok(0))),
            ("bad", Some(Ok(3))),
            ("noexec", Some(Ok(126))),
            ("slow", Some(Err(()))),
        ]));
        let env = [("ISB_VOLUME", "home")];
        let t = Duration::from_secs(2);
        let mut log = RunLog::sink();
        let r = run_hooks(
            &f,
            &insts(&["none", "good", "bad", "noexec", "slow"]),
            &env,
            (t, false),
            &mut log,
        )
        .unwrap();
        let st: Vec<_> = r.iter().map(|x| x.status.clone()).collect();
        assert_eq!(
            st,
            [
                HookStatus::Absent,
                HookStatus::Ok,
                HookStatus::Failed,
                HookStatus::Failed,
                HookStatus::TimedOut
            ]
        );
        assert!(
            r[4].error
                .as_deref()
                .unwrap()
                .contains("timed out after 2s")
        );
        assert!(r[3].error.as_deref().unwrap().contains("not executable"));
        // Required: the first failure stops it.
        let e = run_hooks(
            &f,
            &insts(&["good", "slow", "bad"]),
            &env,
            (t, true),
            &mut log,
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("slow") && e.contains("hook_required"), "{e}");
        // Required, and nothing fails: fine.
        assert_eq!(
            run_hooks(&f, &insts(&["good", "none"]), &env, (t, true), &mut log)
                .unwrap()
                .len(),
            2
        );
    }
}
