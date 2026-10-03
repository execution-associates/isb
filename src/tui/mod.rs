//! `isb tui`: a terminal dashboard for stacks and sandboxes.
//!
//! It reads the `isb serve` daemon's `overview` and `events` tools over the
//! unix socket, the same view a web UI would use; with no daemon it falls
//! back to incus directly and shows sandboxes only, read-only for stacks.
//! Background threads do every slow call, so a key never waits on the
//! network; shells and sandbox actions go to incus directly, as the CLI's do.

pub mod app;
pub mod fmt;
pub mod model;
pub mod source;
pub mod theme;
pub mod ui;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event as TermEvent, KeyEventKind};

use crate::client::Client;
use crate::error::{Error, Result};
use crate::exec::ExecOptions;
use crate::sandbox::Sandbox;
use app::{Action, App, Effect, LogLine, LogTarget, Mode};
use model::{Change, Event, Overview};
use source::Source;

enum Msg {
    Overview(std::result::Result<Overview, String>),
    Events(Vec<Event>),
    Logs(LogTarget, std::result::Result<Vec<LogLine>, String>),
    Done(String, std::result::Result<(), String>),
    Plan(std::result::Result<(String, serde_json::Value, Vec<Change>), String>),
}

/// A client on a qualified sandbox's org, and its bare name.
fn sandbox_client(client: &Client, q: &str) -> Result<(Client, String)> {
    match q.split_once('/') {
        Some((org, name)) => Ok((
            crate::org::client(client, &crate::org::OrgId::new(org)?),
            name.to_string(),
        )),
        None => Ok((client.clone(), q.to_string())),
    }
}

/// Run the dashboard until the user quits.
pub fn run(client: Client, socket: PathBuf) -> Result<()> {
    let source = Source::connect(socket.clone());
    let daemon = source.is_daemon();
    let mut app = App::new(theme::Theme::detect(), daemon);
    let (tx, rx) = channel::<Msg>();
    let stop = Arc::new(AtomicBool::new(false));
    let refresh = Arc::new(AtomicBool::new(false));
    spawn_poller(
        source,
        client.clone(),
        tx.clone(),
        stop.clone(),
        refresh.clone(),
    );
    if daemon {
        spawn_events(socket.clone(), tx.clone(), stop.clone());
    }
    let mut terminal = ratatui::init();
    let r = event_loop(
        &mut terminal,
        &mut app,
        &client,
        &socket,
        &tx,
        &rx,
        &refresh,
    );
    ratatui::restore();
    stop.store(true, Ordering::SeqCst);
    r
}

#[allow(clippy::too_many_arguments)]
fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    client: &Client,
    socket: &Path,
    tx: &Sender<Msg>,
    rx: &Receiver<Msg>,
    refresh: &Arc<AtomicBool>,
) -> Result<()> {
    loop {
        terminal.draw(|f| ui::draw(f, app))?;
        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                TermEvent::Key(k) if k.kind == KeyEventKind::Press => match app.key(k) {
                    Effect::None => {}
                    Effect::Quit => return Ok(()),
                    Effect::Refresh => refresh.store(true, Ordering::SeqCst),
                    Effect::Run(a) => run_action(app, a, client, socket, tx),
                    Effect::Logs(t) => fetch_logs(app, t, client, socket, tx),
                    Effect::Shell(name) => {
                        shell(terminal, client, &name)?;
                        refresh.store(true, Ordering::SeqCst);
                    }
                    Effect::Plan { file, name } => plan(file, name, socket, tx),
                },
                TermEvent::Resize(..) => {}
                _ => {}
            }
        }
        while let Ok(m) = rx.try_recv() {
            match m {
                Msg::Overview(Ok(ov)) => app.set_overview(ov),
                Msg::Overview(Err(e)) => app.error = Some(e),
                Msg::Events(evs) => app.add_events(evs),
                Msg::Logs(t, r) => app.logs_loaded(&t, r),
                Msg::Done(what, r) => {
                    app.busy = None;
                    match r {
                        Ok(()) => app.toast("ok", format!("✓ {what}")),
                        Err(e) => app.toast("error", format!("{what}: {e}")),
                    }
                    refresh.store(true, Ordering::SeqCst);
                }
                Msg::Plan(r) => {
                    app.busy = None;
                    match r {
                        Ok((stack, args, changes)) => app.plan_ready(stack, args, changes),
                        Err(e) => app.toast("error", format!("deploy: {e}")),
                    }
                }
            }
        }
        // A followed log refreshes itself.
        let due = match &app.mode {
            Mode::Logs(v) if v.follow && !v.loading => v
                .fetched
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(2)),
            _ => false,
        };
        if due {
            if let Mode::Logs(v) = &mut app.mode {
                v.loading = true;
                let t = v.target.clone();
                fetch_logs(app, t, client, socket, tx);
            }
        }
    }
}

fn spawn_poller(
    mut source: Source,
    client: Client,
    tx: Sender<Msg>,
    stop: Arc<AtomicBool>,
    refresh: Arc<AtomicBool>,
) {
    let every = if source.is_daemon() {
        Duration::from_secs(1)
    } else {
        Duration::from_secs(2)
    };
    std::thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            let r = source.overview(&client).map_err(|e| e.to_string());
            if tx.send(Msg::Overview(r)).is_err() {
                return;
            }
            let started = Instant::now();
            while started.elapsed() < every && !refresh.swap(false, Ordering::SeqCst) {
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    });
}

fn spawn_events(socket: PathBuf, tx: Sender<Msg>, stop: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let src = Source::Daemon { socket };
        let mut since = 0;
        while !stop.load(Ordering::SeqCst) {
            match src.events(since, Duration::from_secs(3)) {
                Ok((seq, evs)) => {
                    since = seq.max(since);
                    if !evs.is_empty() && tx.send(Msg::Events(evs)).is_err() {
                        return;
                    }
                }
                Err(_) => std::thread::sleep(Duration::from_secs(2)),
            }
        }
    });
}

fn run_action(app: &mut App, a: Action, client: &Client, socket: &Path, tx: &Sender<Msg>) {
    let what = a.describe();
    app.busy = Some(what.clone());
    let (client, socket, tx) = (client.clone(), socket.to_path_buf(), tx.clone());
    std::thread::spawn(move || {
        let src = Source::Daemon { socket };
        let r = match a {
            Action::Scale {
                stack,
                service,
                replicas,
            } => src.scale(&stack, &service, replicas),
            Action::Redeploy { stack, service } => src.redeploy(&stack, &service),
            Action::Rollback { stack } => src.rollback(&stack),
            Action::RemoveStack { stack } => src.remove_stack(&stack),
            Action::Deploy { args, .. } => src.deploy(args, true).map(|_| ()),
            Action::StartSandbox { name } => sandbox_client(&client, &name)
                .and_then(|(c, n)| Sandbox::get(&c, &n))
                .and_then(|s| s.start()),
            Action::StopSandbox { name } => sandbox_client(&client, &name)
                .and_then(|(c, n)| Sandbox::get(&c, &n))
                .and_then(|s| s.stop(false, Duration::from_secs(30))),
            Action::RemoveSandbox { name } => {
                sandbox_client(&client, &name).and_then(|(c, n)| Sandbox::remove(&c, &n, true))
            }
        };
        let _ = tx.send(Msg::Done(what, r.map_err(|e| e.to_string())));
    });
}

fn fetch_logs(app: &App, t: LogTarget, client: &Client, socket: &Path, tx: &Sender<Msg>) {
    // Slots by instance name, from what is on screen now.
    let slots: BTreeMap<String, u32> = app
        .ov
        .stacks
        .iter()
        .flat_map(|s| s.services.iter())
        .flat_map(|v| v.instances.iter())
        .map(|r| (r.name.clone(), r.slot))
        .collect();
    let (client, socket, tx) = (client.clone(), socket.to_path_buf(), tx.clone());
    std::thread::spawn(move || {
        let r: Result<Vec<LogLine>> = match &t {
            LogTarget::Service { stack, service } => Source::Daemon { socket }
                .stack_logs(stack, service, 400)
                .map(|by| app::merge_logs(by, &|n| slots.get(n).copied().unwrap_or(0))),
            LogTarget::Sandbox { name, oci } => sandbox_logs(&client, name, *oci),
        };
        let _ = tx.send(Msg::Logs(t, r.map_err(|e| e.to_string())));
    });
}

/// A sandbox's console log (OCI) or its whole journal.
fn sandbox_logs(client: &Client, q: &str, oci: bool) -> Result<Vec<LogLine>> {
    let (oc, name) = sandbox_client(client, q)?;
    let (client, name) = (&oc, name.as_str());
    let text = if oci {
        String::from_utf8_lossy(&client.console_log(name)?).into_owned()
    } else {
        let sb = Sandbox::get(client, name)?;
        let out = sb.exec_with(
            ["journalctl", "-n", "400", "-o", "short-iso", "--no-pager"],
            ExecOptions::default()
                .user("root")
                .cwd("/")
                .timeout(Duration::from_secs(30)),
        )?;
        if !out.success() {
            return Err(Error::invalid(format!(
                "journalctl: {}",
                out.stderr_text().trim()
            )));
        }
        out.stdout_text()
    };
    Ok(app::merge_logs(vec![(name.to_string(), text)], &|_| 0))
}

/// Hand the terminal to a shell in the instance, then take it back.
fn shell(terminal: &mut DefaultTerminal, client: &Client, name: &str) -> Result<()> {
    ratatui::restore();
    println!("\x1b[2m── shell in {name} · exit to return to isb ──\x1b[0m");
    let r = sandbox_client(client, name)
        .and_then(|(c, n)| Sandbox::get(&c, &n))
        .and_then(|sb| {
            sb.attach(
                [
                    "sh",
                    "-c",
                    "if command -v bash >/dev/null 2>&1; then exec bash -l; else exec sh -l; fi",
                ],
                ExecOptions::default()
                    .tty(true)
                    .user("root")
                    .stdin(crate::exec::Stdin::Inherit),
            )
        });
    if let Err(e) = &r {
        eprintln!("isb: {e}");
        std::thread::sleep(Duration::from_secs(2));
    }
    *terminal = ratatui::init();
    terminal.clear()?;
    Ok(())
}

/// Load a compose file here, as `isb stack deploy` does, and ask the daemon
/// for the plan.
fn plan(file: Option<String>, name: Option<String>, socket: &Path, tx: &Sender<Msg>) {
    let (socket, tx) = (socket.to_path_buf(), tx.clone());
    std::thread::spawn(move || {
        let r = (|| -> Result<(String, serde_json::Value, Vec<Change>)> {
            let files: Vec<PathBuf> = file.into_iter().map(PathBuf::from).collect();
            let p0 = crate::compose::load(&crate::compose::LoadOptions {
                files: files.clone(),
                ..Default::default()
            })?;
            let name = name.unwrap_or_else(|| p0.name.clone());
            let p = crate::compose::load(&crate::compose::LoadOptions {
                files: p0.files.clone(),
                project_name: Some(name.clone()),
                ..Default::default()
            })?;
            let args = crate::daemon::local_deploy_args(&p, &name, false, None)?;
            let changes = Source::Daemon { socket }.deploy(args.clone(), false)?;
            Ok((name, args, changes))
        })();
        let _ = tx.send(Msg::Plan(r.map_err(|e| e.to_string())));
    });
}
