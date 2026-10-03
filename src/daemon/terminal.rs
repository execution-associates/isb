//! The web terminal's other end: a login shell in one of an app's replicas,
//! for [`crate::server::terminal`]. Only replicas of the org's own apps are
//! reachable this way, never an arbitrary instance.

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
                    "the shell could not run: {e}. Images built FROM scratch have no /bin/sh"
                ))
            }
        }
    }

    fn close(&mut self) {
        if !self.done {
            // The shell and whatever it started go with the session.
            let _ = self.ctl.signal(9);
            self.done = true;
        }
    }
}

pub(super) fn terminal(d: Arc<Daemon>) -> Terminal {
    Arc::new(
        move |c: &Caller, org: &OrgId, t: &TermRequest| -> Result<Box<dyn Pty>> {
            let app = d.apps.get(org, &t.app)?;
            let stack = crate::stack::qualified(org, &app.spec.stack()?);
            let st = d
                .ctl
                .status(&stack)
                .map_err(|_| Error::NotFound(format!("{} is not deployed", t.app)))?;
            let svc = st
                .services
                .iter()
                .find(|s| s.service == t.app)
                .ok_or_else(|| Error::NotFound(format!("{} is not deployed", t.app)))?;
            let inst = match t.slot {
                Some(n) => svc
                    .instances
                    .iter()
                    .find(|i| i.slot == n)
                    .ok_or_else(|| Error::NotFound(format!("{} has no replica {n}", t.app)))?,
                None => svc
                    .instances
                    .iter()
                    .filter(|i| i.status == "Running")
                    .min_by_key(|i| (!i.in_rotation, i.slot))
                    .ok_or_else(|| Error::NotFound(format!("{} has no running replica", t.app)))?,
            };
            if inst.status != "Running" {
                return Err(Error::invalid(format!(
                    "replica {} is {}, not running",
                    inst.slot,
                    inst.status.to_lowercase()
                )));
            }
            let oc = crate::org::client(&d.client, org);
            let sb = Sandbox::get(&oc, &inst.name)?;
            let mut opts = ExecOptions::default()
                .tty(true)
                .stdin(Stdin::Piped)
                .env("TERM", "xterm-256color");
            opts.width = Some(t.cols);
            opts.height = Some(t.rows);
            let stream = sb
                .exec_stream(["/bin/sh", "-c", SHELL], opts)
                .map_err(|e| {
                    Error::invalid(format!("cannot start a shell in {}: {e}", inst.name))
                })?;
            d.ctl.service_event(
                "info",
                &stack,
                &t.app,
                format!("terminal opened on replica {} by {c}", inst.slot),
            );
            Ok(Box::new(ExecPty {
                ctl: stream.controller(),
                stream,
                done: false,
            }))
        },
    )
}
