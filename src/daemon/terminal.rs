//! The web terminal's other end: a login shell in one of an app's replicas,
//! for [`crate::server::terminal`]; or a login shell in any instance of the
//! org the caller may exec into (a workspace, a sandbox). Nothing outside
//! the org is reachable this way.

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
    /// Counts the session (a workspace's live sessions, a sandbox's
    /// activity) until it closes.
    guard: Option<super::workspaces::SessionGuard>,
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
            // An org on another server: its agent opens the shell.
            if let Some((s, server)) = d.remote(org) {
                let who = crate::servers::wire::Assertion::for_caller(c)
                    .ok_or_else(|| Error::Forbidden(format!("{c} cannot open a terminal")))?;
                return s.client(&server)?.terminal(&who, org, t);
            }
            let oc = crate::org::client(&d.client, org);
            // Any instance of the org the caller may exec into (a
            // workspace, a sandbox): the org is the boundary.
            if let Some(name) = &t.instance {
                let info = d.reach(c, &oc, name)?;
                if info.status != "Running" {
                    return Err(Error::invalid(format!(
                        "{name} is {}, not running",
                        info.status.to_lowercase()
                    )));
                }
                // The workspace opens as its user, in its home.
                let user = match info.config.get(crate::workspace::KEY_WORKSPACE) {
                    Some(w) => d
                        .workspaces_def(org, w)
                        .ok()
                        .map(|w| (w.user.clone(), w.home_dir())),
                    None => None,
                };
                let mut pty = shell(&oc, name, t, user)?;
                pty.guard = Some(d.workspaces.session(&org.incus_project(), name));
                return Ok(pty);
            }
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
            let pty = shell(&oc, &inst.name, t, None)?;
            d.ctl.service_event(
                "info",
                &stack,
                &t.app,
                format!("terminal opened on replica {} by {c}", inst.slot),
            );
            Ok(pty)
        },
    )
}

/// A login shell in `instance`, on a pseudo-terminal of the asked size.
fn shell(
    oc: &crate::client::Client,
    instance: &str,
    t: &TermRequest,
    user: Option<(String, String)>,
) -> Result<Box<ExecPty>> {
    let sb = Sandbox::get(oc, instance)?;
    let mut opts = ExecOptions::default()
        .tty(true)
        .stdin(Stdin::Piped)
        .env("TERM", "xterm-256color");
    if let Some((u, h)) = user {
        opts = opts
            .user(u.clone())
            .cwd(h.clone())
            .env("HOME", h)
            .env("USER", u.clone())
            .env("LOGNAME", u);
    }
    opts.width = Some(t.cols);
    opts.height = Some(t.rows);
    let stream = sb
        .exec_stream(["/bin/sh", "-c", SHELL], opts)
        .map_err(|e| Error::invalid(format!("cannot start a shell in {instance}: {e}")))?;
    Ok(Box::new(ExecPty {
        ctl: stream.controller(),
        stream,
        done: false,
        instance: instance.to_string(),
        guard: None,
    }))
}
