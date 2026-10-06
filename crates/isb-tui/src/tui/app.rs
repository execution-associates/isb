//! Dashboard state and what keys do to it. Rendering is in `ui`; anything
//! slow (a daemon call, a shell) comes back to the loop as an [`Effect`].

use std::time::Instant;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub use super::logs::{LogLine, LogTarget, LogView, find_matches, merge_logs};
use super::model::{Change, Event, Overview, Sandbox, Service, Stack};
use super::theme::Theme;

/// How many events the feed keeps.
const EVENTS_KEPT: usize = 300;

/// A row of the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    Stack(usize),
    Sandbox(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sidebar,
    Services,
    Replicas,
}

/// Something the user asked for that changes state somewhere.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Scale {
        stack: String,
        service: String,
        replicas: u32,
    },
    Redeploy {
        stack: String,
        service: String,
    },
    Rollback {
        stack: String,
    },
    RemoveStack {
        stack: String,
    },
    Deploy {
        args: serde_json::Value,
        stack: String,
    },
    StartSandbox {
        name: String,
    },
    StopSandbox {
        name: String,
    },
    RemoveSandbox {
        name: String,
    },
}

impl Action {
    pub fn describe(&self) -> String {
        match self {
            Action::Scale {
                stack,
                service,
                replicas,
            } => {
                format!("scale {stack}/{service} to {replicas}")
            }
            Action::Redeploy { stack, service } => format!("redeploy {stack}/{service}"),
            Action::Rollback { stack } => format!("roll back {stack}"),
            Action::RemoveStack { stack } => format!("remove stack {stack}"),
            Action::Deploy { stack, .. } => format!("deploy {stack}"),
            Action::StartSandbox { name } => format!("start {name}"),
            Action::StopSandbox { name } => format!("stop {name}"),
            Action::RemoveSandbox { name } => format!("remove {name}"),
        }
    }
}

/// What the event loop must do for the app.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    None,
    Quit,
    Refresh,
    Run(Action),
    /// Load (or reload) the open log view.
    Logs(LogTarget),
    /// Suspend the dashboard and attach a shell to this instance.
    Shell(String),
    /// Load a compose file and ask the daemon what deploying it would do.
    Plan {
        file: Option<String>,
        name: Option<String>,
    },
}

/// A yes/no question; `require` makes the user type that word first.
#[derive(Debug, Clone)]
pub struct Confirm {
    pub title: String,
    pub lines: Vec<(String, String)>,
    pub action: Action,
    pub require: Option<String>,
    pub typed: String,
    pub danger: bool,
}

#[derive(Debug, Clone)]
pub struct Prompt {
    pub title: String,
    pub label: String,
    pub text: String,
    pub stack: String,
    pub service: String,
}

#[derive(Debug, Clone)]
pub enum Mode {
    Normal,
    Help,
    Palette(String),
    Filter(String),
    Logs(LogView),
    Confirm(Confirm),
    Prompt(Prompt),
}

pub struct App {
    pub theme: Theme,
    pub ov: Overview,
    pub daemon: bool,
    /// The last refresh failed with this.
    pub error: Option<String>,
    pub updated: Option<Instant>,
    pub events: Vec<Event>,
    pub sel: usize,
    pub focus: Focus,
    pub svc: usize,
    pub rep: usize,
    pub filter: String,
    pub mode: Mode,
    pub toast: Option<(String, String, Instant)>,
    pub busy: Option<String>,
}

pub const PALETTE: &[(&str, &str)] = &[
    (
        "deploy [FILE] [NAME]",
        "plan and deploy a compose file as a stack",
    ),
    ("stacks", "jump to the first stack"),
    ("sandboxes", "jump to the first sandbox"),
    ("filter TEXT", "show only matching stacks and sandboxes"),
    ("refresh", "reload now"),
    ("help", "keys"),
    ("quit", "leave"),
];

impl App {
    pub fn new(theme: Theme, daemon: bool) -> App {
        App {
            theme,
            ov: Overview {
                isb: env!("CARGO_PKG_VERSION").into(),
                ..Default::default()
            },
            daemon,
            error: None,
            updated: None,
            events: Vec::new(),
            sel: 0,
            focus: Focus::Sidebar,
            svc: 0,
            rep: 0,
            filter: String::new(),
            mode: Mode::Normal,
            toast: None,
            busy: None,
        }
    }

    // ---- data in --------------------------------------------------------

    pub fn set_overview(&mut self, ov: Overview) {
        let keep = self.selected().map(|i| self.item_name(i));
        self.ov = ov;
        self.error = None;
        self.updated = Some(Instant::now());
        // Keep the cursor on the same stack or sandbox across refreshes.
        if let Some(name) = keep {
            if let Some(pos) = self.items().iter().position(|i| self.item_name(*i) == name) {
                self.sel = pos;
            }
        }
        self.clamp();
    }

    pub fn add_events(&mut self, mut evs: Vec<Event>) {
        self.events.append(&mut evs);
        let n = self.events.len();
        if n > EVENTS_KEPT {
            self.events.drain(..n - EVENTS_KEPT);
        }
    }

    pub fn toast(&mut self, level: &str, msg: impl Into<String>) {
        self.toast = Some((level.into(), msg.into(), Instant::now()));
    }

    // ---- selection ------------------------------------------------------

    fn item_name(&self, i: Item) -> String {
        match i {
            Item::Stack(n) => format!("s:{}", self.ov.stacks[n].qualified()),
            Item::Sandbox(n) => format!("b:{}", self.ov.sandboxes[n].qualified()),
        }
    }

    fn matches(&self, name: &str) -> bool {
        self.filter.is_empty() || name.to_lowercase().contains(&self.filter.to_lowercase())
    }

    /// Sidebar rows, filtered: stacks first, then sandboxes.
    pub fn items(&self) -> Vec<Item> {
        let mut v: Vec<Item> = (0..self.ov.stacks.len())
            .filter(|i| self.matches(&self.ov.stacks[*i].qualified()))
            .map(Item::Stack)
            .collect();
        v.extend(
            (0..self.ov.sandboxes.len())
                .filter(|i| self.matches(&self.ov.sandboxes[*i].qualified()))
                .map(Item::Sandbox),
        );
        v
    }

    pub fn selected(&self) -> Option<Item> {
        self.items().get(self.sel).copied()
    }

    pub fn stack(&self) -> Option<&Stack> {
        match self.selected()? {
            Item::Stack(i) => self.ov.stacks.get(i),
            _ => None,
        }
    }

    pub fn sandbox(&self) -> Option<&Sandbox> {
        match self.selected()? {
            Item::Sandbox(i) => self.ov.sandboxes.get(i),
            _ => None,
        }
    }

    pub fn service(&self) -> Option<&Service> {
        self.stack()?.services.get(self.svc)
    }

    pub(super) fn clamp(&mut self) {
        let n = self.items().len();
        self.sel = self.sel.min(n.saturating_sub(1));
        let ns = self.stack().map(|s| s.services.len()).unwrap_or(0);
        self.svc = self.svc.min(ns.saturating_sub(1));
        let nr = self.service().map(|s| s.instances.len()).unwrap_or(0);
        self.rep = self.rep.min(nr.saturating_sub(1));
        if self.stack().is_none() && self.focus != Focus::Sidebar {
            self.focus = Focus::Sidebar;
        }
    }

    pub(super) fn step(&mut self, delta: isize) {
        let (cur, len) = match self.focus {
            Focus::Sidebar => (self.sel, self.items().len()),
            Focus::Services => (
                self.svc,
                self.stack().map(|s| s.services.len()).unwrap_or(0),
            ),
            Focus::Replicas => (
                self.rep,
                self.service().map(|s| s.instances.len()).unwrap_or(0),
            ),
        };
        if len == 0 {
            return;
        }
        let next = (cur as isize + delta).clamp(0, len as isize - 1) as usize;
        match self.focus {
            Focus::Sidebar => {
                if next != self.sel {
                    self.sel = next;
                    self.svc = 0;
                    self.rep = 0;
                }
            }
            Focus::Services => {
                if next != self.svc {
                    self.svc = next;
                    self.rep = 0;
                }
            }
            Focus::Replicas => self.rep = next,
        }
    }

    fn deeper(&mut self) {
        self.focus = match (self.focus, self.stack().is_some()) {
            (Focus::Sidebar, true) => Focus::Services,
            (Focus::Services, true) => Focus::Replicas,
            (f, _) => f,
        };
    }

    fn shallower(&mut self) {
        self.focus = match self.focus {
            Focus::Replicas => Focus::Services,
            _ => Focus::Sidebar,
        };
    }

    // ---- keys -----------------------------------------------------------

    pub fn key(&mut self, k: KeyEvent) -> Effect {
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            return Effect::Quit;
        }
        match std::mem::replace(&mut self.mode, Mode::Normal) {
            Mode::Normal => self.key_normal(k),
            Mode::Help => Effect::None,
            Mode::Palette(text) => self.key_palette(k, text),
            Mode::Filter(text) => self.key_filter(k, text),
            Mode::Logs(view) => self.key_logs(k, view),
            Mode::Confirm(c) => self.key_confirm(k, c),
            Mode::Prompt(p) => self.key_prompt(k, p),
        }
    }

    fn need_daemon(&mut self) -> bool {
        if !self.daemon {
            self.toast(
                "warn",
                "that needs the isb serve daemon (isb serve install)",
            );
        }
        self.daemon
    }

    fn key_normal(&mut self, k: KeyEvent) -> Effect {
        match k.code {
            KeyCode::Char('q') => return Effect::Quit,
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char(':') => self.mode = Mode::Palette(String::new()),
            KeyCode::Char('/') => self.mode = Mode::Filter(self.filter.clone()),
            KeyCode::Down | KeyCode::Char('j') => self.step(1),
            KeyCode::Up | KeyCode::Char('k') => self.step(-1),
            KeyCode::PageDown => self.step(10),
            KeyCode::PageUp => self.step(-10),
            KeyCode::Char('g') | KeyCode::Home => self.step(-10_000),
            KeyCode::Char('G') | KeyCode::End => self.step(10_000),
            KeyCode::Tab | KeyCode::Right | KeyCode::Enter => self.deeper(),
            KeyCode::BackTab | KeyCode::Left | KeyCode::Esc => {
                if self.focus == Focus::Sidebar && !self.filter.is_empty() && k.code == KeyCode::Esc
                {
                    self.filter.clear();
                    self.clamp();
                } else {
                    self.shallower();
                }
            }
            KeyCode::Char('R') => return Effect::Refresh,
            KeyCode::Char('l') => return self.open_logs(),
            KeyCode::Char('e') => return self.shell(),
            KeyCode::Char('s') => self.ask_scale(),
            KeyCode::Char('r') => self.ask_redeploy(),
            KeyCode::Char('b') => self.ask_rollback(),
            KeyCode::Char('x') => self.ask_remove(),
            KeyCode::Char('t') => return self.toggle_sandbox(),
            KeyCode::Char('d') => {
                self.mode = Mode::Palette("deploy ".into());
            }
            _ => {}
        }
        self.clamp();
        Effect::None
    }

    fn open_logs(&mut self) -> Effect {
        let target = if let (Some(st), Some(svc)) = (self.stack(), self.service()) {
            LogTarget::Service {
                stack: st.qualified(),
                service: svc.service.clone(),
            }
        } else if let Some(sb) = self.sandbox() {
            LogTarget::Sandbox {
                name: sb.qualified(),
                oci: sb.kind == "oci",
            }
        } else {
            return Effect::None;
        };
        let only = (self.focus == Focus::Replicas)
            .then(|| {
                self.service()
                    .and_then(|s| s.instances.get(self.rep))
                    .map(|r| r.slot)
            })
            .flatten();
        self.mode = Mode::Logs(LogView {
            target: target.clone(),
            lines: Vec::new(),
            follow: true,
            wrap: false,
            scroll: 0,
            only,
            query: String::new(),
            typing: None,
            loading: true,
            error: None,
            fetched: None,
        });
        Effect::Logs(target)
    }

    fn shell(&mut self) -> Effect {
        if let Some(svc) = self.service() {
            let r = svc.instances.get(if self.focus == Focus::Replicas {
                self.rep
            } else {
                0
            });
            return match r {
                Some(r) => Effect::Shell(format!(
                    "{}{}",
                    self.stack()
                        .map(|s| super::model::org_prefix(&s.org))
                        .unwrap_or_default(),
                    r.name
                )),
                None => {
                    self.toast("warn", "no replica to open a shell in");
                    Effect::None
                }
            };
        }
        match self.sandbox() {
            Some(sb) if sb.running() => Effect::Shell(sb.qualified()),
            Some(sb) => {
                let n = sb.name.clone();
                self.toast("warn", format!("{n} is not running (t starts it)"));
                Effect::None
            }
            None => Effect::None,
        }
    }

    fn toggle_sandbox(&mut self) -> Effect {
        let Some(sb) = self.sandbox() else {
            return Effect::None;
        };
        let name = sb.qualified();
        Effect::Run(if sb.running() {
            Action::StopSandbox { name }
        } else {
            Action::StartSandbox { name }
        })
    }

    fn ask_scale(&mut self) {
        let (Some(st), Some(svc)) = (self.stack(), self.service()) else {
            return;
        };
        let p = Prompt {
            title: format!("Scale {}/{}", st.name, svc.service),
            label: format!("replicas (now {})", svc.replicas),
            text: svc.replicas.to_string(),
            stack: st.qualified(),
            service: svc.service.clone(),
        };
        if self.need_daemon() {
            self.mode = Mode::Prompt(p);
        }
    }

    fn ask_redeploy(&mut self) {
        let (Some(st), Some(svc)) = (self.stack(), self.service()) else {
            return;
        };
        let c = Confirm {
            title: format!("Redeploy {}/{}?", st.name, svc.service),
            lines: vec![
                ("replicas".into(), svc.replicas.to_string()),
                (
                    "effect".into(),
                    "fresh replicas, rolling, per update_config".into(),
                ),
            ],
            action: Action::Redeploy {
                stack: st.qualified(),
                service: svc.service.clone(),
            },
            require: None,
            typed: String::new(),
            danger: false,
        };
        if self.need_daemon() {
            self.mode = Mode::Confirm(c);
        }
    }

    fn ask_rollback(&mut self) {
        let Some(st) = self.stack() else { return };
        if !st.has_previous {
            let n = st.name.clone();
            self.toast("warn", format!("{n} has no previous deployment"));
            return;
        }
        let c = Confirm {
            title: format!("Roll back {}?", st.name),
            lines: vec![(
                "effect".into(),
                "the previous deployment rolls back in; b again undoes it".into(),
            )],
            action: Action::Rollback {
                stack: st.qualified(),
            },
            require: None,
            typed: String::new(),
            danger: false,
        };
        if self.need_daemon() {
            self.mode = Mode::Confirm(c);
        }
    }

    fn ask_remove(&mut self) {
        if let Some(st) = self.stack() {
            let (h, t) = st.replicas();
            let c = Confirm {
                title: format!("Remove stack {}?", st.name),
                lines: vec![
                    ("services".into(), st.services.len().to_string()),
                    ("replicas".into(), format!("{h}/{t} healthy")),
                    (
                        "effect".into(),
                        "deletes its replicas and ports; volumes stay".into(),
                    ),
                ],
                action: Action::RemoveStack {
                    stack: st.qualified(),
                },
                require: Some(st.name.clone()),
                typed: String::new(),
                danger: true,
            };
            if self.need_daemon() {
                self.mode = Mode::Confirm(c);
            }
        } else if let Some(sb) = self.sandbox() {
            self.mode = Mode::Confirm(Confirm {
                title: format!("Remove sandbox {}?", sb.name),
                lines: vec![
                    ("status".into(), sb.status.to_lowercase()),
                    (
                        "effect".into(),
                        "deletes the instance and everything in it".into(),
                    ),
                ],
                action: Action::RemoveSandbox {
                    name: sb.qualified(),
                },
                require: Some(sb.name.clone()),
                typed: String::new(),
                danger: true,
            });
        }
    }

    /// The deploy plan came back: ask before applying it.
    pub fn plan_ready(&mut self, stack: String, args: serde_json::Value, changes: Vec<Change>) {
        let lines = changes
            .iter()
            .map(|c| {
                let what = match c.change.as_str() {
                    "update" => {
                        format!("update → rev {} ({} replicas, rolling)", c.rev, c.replicas)
                    }
                    "create" => format!("create ({} replicas)", c.replicas),
                    "scale" => format!("scale to {}", c.replicas),
                    other => other.to_string(),
                };
                (c.service.clone(), what)
            })
            .collect();
        let danger = changes.iter().any(|c| c.change == "remove");
        self.mode = Mode::Confirm(Confirm {
            title: format!("Deploy {stack}?"),
            lines,
            action: Action::Deploy { args, stack },
            require: None,
            typed: String::new(),
            danger,
        });
    }

    fn key_confirm(&mut self, k: KeyEvent, mut c: Confirm) -> Effect {
        match (k.code, &c.require) {
            (KeyCode::Esc, _) => return Effect::None,
            (KeyCode::Char('n'), None) => return Effect::None,
            (KeyCode::Char('y') | KeyCode::Enter, None) => return Effect::Run(c.action),
            (KeyCode::Enter, Some(word)) if c.typed == *word => return Effect::Run(c.action),
            (KeyCode::Backspace, Some(_)) => {
                c.typed.pop();
            }
            (KeyCode::Char(ch), Some(_)) => c.typed.push(ch),
            _ => {}
        }
        self.mode = Mode::Confirm(c);
        Effect::None
    }

    fn key_prompt(&mut self, k: KeyEvent, mut p: Prompt) -> Effect {
        match k.code {
            KeyCode::Esc => return Effect::None,
            KeyCode::Enter => match p.text.trim().parse::<u32>() {
                Ok(n) if n <= 100 => {
                    return Effect::Run(Action::Scale {
                        stack: p.stack,
                        service: p.service,
                        replicas: n,
                    });
                }
                _ => self.toast("warn", "replicas is a number from 0 to 100"),
            },
            KeyCode::Backspace => {
                p.text.pop();
            }
            KeyCode::Up => {
                let n = p.text.parse::<u32>().unwrap_or(0);
                p.text = (n + 1).min(100).to_string();
            }
            KeyCode::Down => {
                let n = p.text.parse::<u32>().unwrap_or(0);
                p.text = n.saturating_sub(1).to_string();
            }
            KeyCode::Char(ch) if ch.is_ascii_digit() && p.text.len() < 3 => p.text.push(ch),
            _ => {}
        }
        self.mode = Mode::Prompt(p);
        Effect::None
    }

    fn key_filter(&mut self, k: KeyEvent, mut text: String) -> Effect {
        match k.code {
            KeyCode::Esc => {
                self.filter.clear();
                self.clamp();
                return Effect::None;
            }
            KeyCode::Enter => return Effect::None,
            KeyCode::Backspace => {
                text.pop();
            }
            KeyCode::Char(ch) => text.push(ch),
            _ => {}
        }
        self.filter = text.clone();
        self.sel = 0;
        self.clamp();
        self.mode = Mode::Filter(text);
        Effect::None
    }

    fn key_palette(&mut self, k: KeyEvent, mut text: String) -> Effect {
        match k.code {
            KeyCode::Esc => return Effect::None,
            KeyCode::Enter => return self.run_command(text.trim()),
            KeyCode::Tab => {
                if let Some((cmd, _)) = PALETTE.iter().find(|(c, _)| c.starts_with(text.trim())) {
                    text = cmd.split_whitespace().next().unwrap_or("").to_string() + " ";
                }
            }
            KeyCode::Backspace => {
                text.pop();
            }
            KeyCode::Char(ch) => text.push(ch),
            _ => {}
        }
        self.mode = Mode::Palette(text);
        Effect::None
    }

    fn run_command(&mut self, cmd: &str) -> Effect {
        let mut words = cmd.split_whitespace();
        match words.next().unwrap_or("") {
            "" => {}
            "q" | "quit" => return Effect::Quit,
            "help" => self.mode = Mode::Help,
            "refresh" => return Effect::Refresh,
            "stacks" => {
                self.focus = Focus::Sidebar;
                self.sel = 0;
            }
            "sandboxes" => {
                self.focus = Focus::Sidebar;
                self.sel = self
                    .items()
                    .iter()
                    .position(|i| matches!(i, Item::Sandbox(_)))
                    .unwrap_or(0);
            }
            "filter" => {
                self.filter = words.collect::<Vec<_>>().join(" ");
                self.sel = 0;
                self.clamp();
            }
            "deploy" => {
                if !self.need_daemon() {
                    return Effect::None;
                }
                let file = words.next().map(String::from);
                let name = words.next().map(String::from);
                self.busy = Some("planning the deploy".into());
                return Effect::Plan { file, name };
            }
            other => self.toast("warn", format!("unknown command {other:?} (try :help)")),
        }
        Effect::None
    }

    /// Keys the bar at the bottom offers right now.
    pub fn hints(&self) -> Vec<(&'static str, &'static str)> {
        match &self.mode {
            Mode::Logs(v) if v.typing.is_some() => {
                vec![("⏎", "search"), ("esc", "cancel")]
            }
            Mode::Logs(v) => {
                let mut h = vec![("/", "search")];
                if !v.query.is_empty() {
                    h.push(("n N", "older/newer match"));
                }
                h.extend([
                    ("f", "follow"),
                    ("w", "wrap"),
                    ("1-9", "replica"),
                    ("a", "all"),
                    ("↑↓", "scroll"),
                ]);
                h.push(if v.query.is_empty() {
                    ("esc", "back")
                } else {
                    ("esc", "clear search")
                });
                h
            }
            Mode::Palette(_) => vec![("tab", "complete"), ("⏎", "run"), ("esc", "cancel")],
            Mode::Filter(_) => vec![("⏎", "keep"), ("esc", "clear")],
            Mode::Confirm(c) if c.require.is_some() => {
                vec![("type the name", ""), ("⏎", "confirm"), ("esc", "cancel")]
            }
            Mode::Confirm(_) => vec![("y", "yes"), ("n", "no")],
            Mode::Prompt(_) => vec![("↑↓", "adjust"), ("⏎", "apply"), ("esc", "cancel")],
            Mode::Help => vec![("any key", "close")],
            Mode::Normal => {
                let mut v = vec![("↑↓", "move")];
                if self.stack().is_some() {
                    v.extend([
                        ("⏎", "open"),
                        ("l", "logs"),
                        ("e", "shell"),
                        ("s", "scale"),
                        ("r", "redeploy"),
                        ("b", "rollback"),
                        ("x", "remove"),
                    ]);
                } else if self.sandbox().is_some() {
                    v.extend([
                        ("l", "logs"),
                        ("e", "shell"),
                        ("t", "start/stop"),
                        ("x", "remove"),
                    ]);
                }
                if self.daemon {
                    v.push(("d", "deploy"));
                }
                v.extend([(":", "command"), ("/", "filter"), ("?", "help")]);
                v
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::model::{Replica, Service, Stack};

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn app() -> App {
        let mut a = App::new(Theme::mono(), true);
        a.set_overview(Overview {
            stacks: vec![Stack {
                name: "e2e".into(),
                has_previous: true,
                services: vec![Service {
                    service: "web".into(),
                    replicas: 2,
                    instances: vec![
                        Replica {
                            name: "e2e-web-1-aa".into(),
                            slot: 1,
                            ..Default::default()
                        },
                        Replica {
                            name: "e2e-web-2-bb".into(),
                            slot: 2,
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            sandboxes: vec![Sandbox {
                name: "box".into(),
                status: "Running".into(),
                ..Default::default()
            }],
            ..Default::default()
        });
        a
    }

    #[test]
    fn navigation_and_actions() {
        let mut a = app();
        assert_eq!(a.selected(), Some(Item::Stack(0)));
        a.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        a.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(a.focus, Focus::Replicas);
        a.key(key('j'));
        assert_eq!(a.key(key('e')), Effect::Shell("e2e-web-2-bb".into()));
        a.key(key('s'));
        assert!(matches!(a.mode, Mode::Prompt(_)));
        a.key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        a.key(key('4'));
        assert_eq!(
            a.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Effect::Run(Action::Scale {
                stack: "e2e".into(),
                service: "web".into(),
                replicas: 4
            })
        );
        // Removing needs the name typed.
        a.key(key('x'));
        assert_eq!(
            a.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Effect::None
        );
        assert!(matches!(a.mode, Mode::Confirm(_)));
        for c in "e2e".chars() {
            a.key(key(c));
        }
        assert_eq!(
            a.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Effect::Run(Action::RemoveStack {
                stack: "e2e".into()
            })
        );
    }

    #[test]
    fn filter_and_palette() {
        let mut a = app();
        a.key(key('/'));
        for c in "bo".chars() {
            a.key(key(c));
        }
        assert_eq!(a.items(), vec![Item::Sandbox(0)]);
        a.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            a.key(key('t')),
            Effect::Run(Action::StopSandbox { name: "box".into() })
        );
        a.key(key(':'));
        for c in "deploy ./x.yaml app".chars() {
            a.key(key(c));
        }
        assert_eq!(
            a.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Effect::Plan {
                file: Some("./x.yaml".into()),
                name: Some("app".into())
            }
        );
    }

    #[test]
    fn selection_survives_refresh() {
        let mut a = app();
        a.key(key('j'));
        assert_eq!(a.selected(), Some(Item::Sandbox(0)));
        let mut ov = a.ov.clone();
        ov.stacks.push(Stack {
            name: "aaa".into(),
            ..Default::default()
        });
        a.set_overview(ov);
        assert_eq!(a.sandbox().map(|s| s.name.as_str()), Some("box"));
    }
}
