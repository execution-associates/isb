//! The web terminal's other end: a login shell, or a command (`isb ...
//! exec -t`), in one of an app's or a stack service's replicas, for
//! [`crate::server::terminal`]; or in any instance of the org the caller
//! may exec into (a sandbox, a replica). Nothing outside the org is
//! reachable this way.

use std::sync::Arc;
use std::time::Duration;

use crate::error::{Error, Result};
use crate::exec::{ExecController, ExecEvent, ExecOptions, ExecStream, Stdin};
use crate::org::OrgId;
use crate::sandbox::Sandbox;
use crate::server::Caller;
use crate::server::terminal::{Pty, PtyOutput, TermRequest, Terminal};

use super::Daemon;

/// bash when the image has it, else sh; a clear message when it has neither
/// (a `FROM scratch` image such as traefik/whoami).
const SHELL: &str = "if command -v bash >/dev/null 2>&1; then exec bash -l; fi; exec sh -l";

struct ExecPty {
    stream: ExecStream,
    ctl: ExecController,
    done: bool,
    instance: String,
    /// What runs, for the message when it cannot: "the shell", or the
    /// command's name.
    what: String,
    /// Counts the session (a sandbox's activity: the reaper leaves it
    /// alone) until it closes.
    guard: Option<super::sandboxes::SessionGuard>,
}

impl Pty for ExecPty {
    fn input(&mut self, data: &[u8]) -> Result<()> {
        self.ctl.write_stdin(data)
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        let _ = self.ctl.resize(cols, rows);
    }

    fn output(&mut self, wait: Duration) -> PtyOutput {
        if self.done {
            return PtyOutput::Exit(None);
        }
        match self.stream.poll_event(wait) {
            Ok(Some(ExecEvent::Stdout(b) | ExecEvent::Stderr(b))) => PtyOutput::Data(b),
            Ok(None) => PtyOutput::Idle,
            Err(Ok(code)) => {
                self.done = true;
                PtyOutput::Exit(Some(code))
            }
            Err(Err(e)) => {
                self.done = true;
                PtyOutput::Failed(format!(
                    "{} could not run: {e}. Images built FROM scratch have no /bin/sh",
                    self.what
                ))
            }
        }
    }

    fn target(&self) -> Option<String> {
        Some(self.instance.clone())
    }

    fn close(&mut self) {
        if !self.done {
            // The shell and whatever it started go with the session.
            let _ = self.ctl.signal(9);
            self.done = true;
        }
        self.guard = None;
    }
}

pub(super) fn terminal(d: Arc<Daemon>) -> Terminal {
    Arc::new(
        move |c: &Caller, org: &OrgId, t: &TermRequest| -> Result<Box<dyn Pty>> {
            let oc = crate::org::client(&d.client, org);
            // Any instance of the org the caller may exec into: the org is
            // the boundary.
            if let Some(name) = &t.instance {
                let info = d.reach(c, &oc, name)?;
                if info.status != "Running" {
                    return Err(Error::invalid(format!(
                        "{name} is {}, not running",
                        info.status.to_lowercase()
                    )));
                }
                let mut pty = open(&oc, name, t)?;
                pty.guard = Some(d.sandboxes.session(&org.incus_project(), name));
                return Ok(pty);
            }
            // An app is the service of its own stack; a stack names its own.
            let (stack, service) = match &t.stack {
                Some(s) => (crate::stack::qualified(org, s), t.app.clone()),
                None => {
                    let app = d.apps.get(org, &t.app)?;
                    (
                        crate::stack::qualified(org, &app.spec.stack()?),
                        t.app.clone(),
                    )
                }
            };
            let what = t.target();
            let st = d
                .ctl
                .status(&stack)
                .map_err(|_| Error::NotFound(format!("{what} is not deployed")))?;
            let svc = st
                .services
                .iter()
                .find(|s| s.service == service)
                .ok_or_else(|| Error::NotFound(format!("{what} is not deployed")))?;
            let inst = super::kube::pick_replica(&what, &svc.instances, t.slot, None)?;
            let pty = open(&oc, &inst.name, t)?;
            let opened = match &t.command {
                Some(_) => "exec -t",
                None => "terminal",
            };
            d.ctl.service_event(
                "info",
                &stack,
                &service,
                format!("{opened} opened on replica {} by {c}", inst.slot),
            );
            Ok(pty)
        },
    )
}

/// The terminal's program in `instance`, on a pseudo-terminal of the asked
/// size: its command (as the exec tools would run it), else a login shell.
fn open(oc: &crate::client::Client, instance: &str, t: &TermRequest) -> Result<Box<ExecPty>> {
    let sb = Sandbox::get(oc, instance)?;
    let mut opts = ExecOptions::default().tty(true).stdin(Stdin::Piped);
    opts.width = Some(t.cols);
    opts.height = Some(t.rows);
    let (argv, what) = match &t.command {
        Some(cmd) => {
            opts.user = cmd.user.clone();
            opts.cwd = cmd.cwd.clone();
            opts.env = cmd.env.clone();
            (cmd.argv.clone(), format!("{:?}", cmd.argv[0]))
        }
        None => (
            ["/bin/sh", "-c", SHELL].map(String::from).to_vec(),
            "the shell".to_string(),
        ),
    };
    opts.env
        .entry("TERM".into())
        .or_insert_with(|| "xterm-256color".into());
    let stream = sb
        .exec_stream(argv, opts)
        .map_err(|e| Error::invalid(format!("cannot start {what} in {instance}: {e}")))?;
    Ok(Box::new(ExecPty {
        ctl: stream.controller(),
        stream,
        done: false,
        instance: instance.to_string(),
        what,
        guard: None,
    }))
}
