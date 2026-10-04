//! Web terminals that survive a reload, through herdr when the workspace
//! has it (docs/concepts/workspaces.md#the-web-terminal). herdr is the
//! image's, not isb's: isb only runs its CLI as the workspace user.
//!
//! Each web terminal tab is a tab, labelled with the tab's name, in a herdr
//! workspace labelled [`HERDR_WORKSPACE`] on the user's own herdr server
//! (its default session, started when it is not running). Opening a tab
//! attaches to that tab's first pane with `herdr terminal attach ID
//! --takeover`; closing the websocket kills only the attach client, so the
//! shell lives on and the next attach (a reload, another browser) finds it.
//! Ending a session closes the herdr tab. The same tabs show up in herdr
//! itself (`herdr` over SSH, `herdr machine add`).
//!
//! Without herdr in the workspace, tabs are plain login shells, as on any
//! other instance.

use super::*;
use crate::exec::{ExecOptions, ExecOutput};

/// The herdr workspace that holds the web terminal's tabs.
pub const HERDR_WORKSPACE: &str = "isb web";
/// How long one herdr CLI call may take.
const CALL: Duration = Duration::from_secs(20);

/// A herdr CLI call through a login shell (so `~/.local/bin` and the
/// profile are on the path), as argv: the arguments are never shell text.
pub fn herdr_argv(args: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = ["sh", "-lc", "exec herdr \"$@\"", "herdr"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    v.extend(args.iter().map(|s| s.to_string()));
    v
}

/// The command a web terminal tab runs in herdr mode.
pub fn attach_argv(terminal_id: &str) -> Vec<String> {
    herdr_argv(&["terminal", "attach", terminal_id, "--takeover"])
}

/// Starts the user's herdr server, detached from the exec that starts it,
/// with the login environment (so new panes have `$ISB_TOKEN` and the rest)
/// and the user's login shell as `$SHELL`, which herdr gives new panes.
const START_SERVER: &str = r#"SHELL=$(getent passwd "$(id -un)" | cut -d: -f7); export SHELL="${SHELL:-/bin/sh}"
if command -v setsid >/dev/null 2>&1; then setsid -f herdr server </dev/null >/dev/null 2>&1; else nohup herdr server </dev/null >/dev/null 2>&1 & fi"#;

/// A web terminal tab's name: what the tab shows and the herdr tab's label.
pub fn check_session(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.chars().count() <= 40
        && !name.starts_with(['-', ' '])
        && !name.ends_with(' ')
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || " ._-:#()+@".contains(c));
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "terminal session {name:?}: up to 40 letters, digits, spaces and ._-:#()+@, not starting with - or a space"
        )))
    }
}

/// A herdr CLI reply: its `result`, or its error as ours.
pub fn reply(out: &ExecOutput) -> Result<Value> {
    let text = String::from_utf8_lossy(&out.stdout);
    let v: Value = text
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str(l.trim()).ok())
        .unwrap_or(Value::Null);
    if let Some(e) = v.get("error") {
        let code = e["code"].as_str().unwrap_or("error");
        let msg = e["message"].as_str().unwrap_or("herdr failed");
        return Err(if code == "server_not_running" {
            Error::invalid(format!("herdr: {code}"))
        } else {
            Error::invalid(format!("herdr: {msg}"))
        });
    }
    match v.get("result") {
        Some(r) if out.success() => Ok(r.clone()),
        _ => Err(Error::invalid(format!(
            "herdr failed (exit {}): {}",
            out.exit_code,
            String::from_utf8_lossy(&out.stderr).trim()
        ))),
    }
}

/// The herdr workspace labelled `label` in a `workspace list` result.
pub fn find_workspace(list: &Value, label: &str) -> Option<String> {
    list["workspaces"]
        .as_array()?
        .iter()
        .find(|w| w["label"] == label)
        .and_then(|w| w["workspace_id"].as_str().map(str::to_string))
}

/// A herdr tab: its id, label and pane count.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Tab {
    pub tab_id: String,
    pub name: String,
    pub panes: u64,
}

/// The tabs of a `tab list` result, in order.
pub fn tabs(list: &Value) -> Vec<Tab> {
    list["tabs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| {
            Some(Tab {
                tab_id: t["tab_id"].as_str()?.to_string(),
                name: t["label"].as_str()?.to_string(),
                panes: t["pane_count"].as_u64().unwrap_or(1),
            })
        })
        .collect()
}

/// The terminal a web tab attaches to: the tab's focused pane, else its
/// first, from a `pane list` result.
pub fn terminal_of(panes: &Value, tab_id: &str) -> Option<String> {
    let mine: Vec<&Value> = panes["panes"]
        .as_array()?
        .iter()
        .filter(|p| p["tab_id"] == tab_id)
        .collect();
    mine.iter()
        .find(|p| p["focused"] == true)
        .or(mine.first())
        .and_then(|p| p["terminal_id"].as_str().map(str::to_string))
}

/// herdr in one workspace, run as its user.
pub struct Herdr<'a> {
    sb: Sandbox,
    w: &'a Workspace,
}

impl<'a> Herdr<'a> {
    pub fn new(oc: &Client, w: &'a Workspace) -> Result<Herdr<'a>> {
        Ok(Herdr {
            sb: Sandbox::get(oc, w.instance())?,
            w,
        })
    }

    fn opts(&self) -> ExecOptions {
        let (u, h) = (self.w.user.clone(), self.w.home_dir());
        ExecOptions::default()
            .user(u.clone())
            .cwd(h.clone())
            .env("HOME", h)
            .env("USER", u.clone())
            .env("LOGNAME", u)
            .timeout(CALL)
    }

    fn sh(&self, script: &str) -> Result<ExecOutput> {
        self.sb.exec_with(["sh", "-lc", script], self.opts())
    }

    fn call(&self, args: &[&str]) -> Result<Value> {
        reply(&self.sb.exec_with(herdr_argv(args), self.opts())?)
    }

    /// herdr's version when the workspace has it on the user's path.
    pub fn version(&self) -> Option<String> {
        let out = self
            .sh("command -v herdr >/dev/null 2>&1 && herdr --version")
            .ok()?;
        out.success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn running(&self) -> bool {
        self.sh("herdr status server")
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("status: running"))
    }

    /// The user's herdr server, started when it is not running.
    pub fn ensure_server(&self) -> Result<()> {
        if self.running() {
            return Ok(());
        }
        self.sh(START_SERVER)?;
        for _ in 0..50 {
            if self.running() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        Err(Error::invalid(
            "herdr's server did not start in the workspace (try `herdr server` there)",
        ))
    }

    fn workspace_id(&self) -> Result<Option<String>> {
        Ok(find_workspace(
            &self.call(&["workspace", "list"])?,
            HERDR_WORKSPACE,
        ))
    }

    /// The web terminal's sessions; none while the server is not running
    /// (listing never starts it).
    pub fn sessions(&self) -> Result<Vec<Tab>> {
        if !self.running() {
            return Ok(Vec::new());
        }
        match self.workspace_id()? {
            Some(id) => Ok(tabs(&self.call(&["tab", "list", "--workspace", &id])?)),
            None => Ok(Vec::new()),
        }
    }

    fn find(&self, name: &str) -> Result<Option<(String, Tab)>> {
        let Some(ws_id) = self.workspace_id()? else {
            return Ok(None);
        };
        let all = tabs(&self.call(&["tab", "list", "--workspace", &ws_id])?);
        Ok(all.into_iter().find(|t| t.name == name).map(|t| (ws_id, t)))
    }

    /// The terminal session `name` attaches to, made when it is new: the
    /// server, the `isb web` workspace and the tab as needed.
    pub fn ensure_session(&self, name: &str) -> Result<String> {
        check_session(name)?;
        self.ensure_server()?;
        let home = self.w.home_dir();
        let (ws_id, tab_id) = match self.find(name)? {
            Some((w, t)) => (w, t.tab_id),
            None => match self.workspace_id()? {
                Some(w) => {
                    let r = self.call(&[
                        "tab",
                        "create",
                        "--workspace",
                        &w,
                        "--cwd",
                        &home,
                        "--label",
                        name,
                        "--no-focus",
                    ])?;
                    let t = r["tab"]["tab_id"].as_str().unwrap_or_default().to_string();
                    (w, t)
                }
                None => {
                    let r = self.call(&[
                        "workspace",
                        "create",
                        "--cwd",
                        &home,
                        "--label",
                        HERDR_WORKSPACE,
                        "--no-focus",
                    ])?;
                    let w = r["workspace"]["workspace_id"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    let t = r["tab"]["tab_id"].as_str().unwrap_or_default().to_string();
                    self.call(&["tab", "rename", &t, name])?;
                    (w, t)
                }
            },
        };
        let panes = self.call(&["pane", "list", "--workspace", &ws_id])?;
        terminal_of(&panes, &tab_id)
            .ok_or_else(|| Error::invalid(format!("herdr tab {name} has no pane to attach to")))
    }

    /// End session `name`: close its herdr tab and every shell in it.
    pub fn end(&self, name: &str) -> Result<bool> {
        match self.find(name)? {
            Some((_, t)) => {
                self.call(&["tab", "close", &t.tab_id])?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Rename session `name` (its herdr tab).
    pub fn rename(&self, name: &str, to: &str) -> Result<()> {
        check_session(to)?;
        let (_, t) = self
            .find(name)?
            .ok_or_else(|| Error::NotFound(format!("terminal session {name}")))?;
        if self.find(to)?.is_some() {
            return Err(Error::AlreadyExists(format!("terminal session {to}")));
        }
        self.call(&["tab", "rename", &t.tab_id, to])?;
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TermArgs {
    #[serde(default)]
    #[allow(dead_code)]
    org: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    rename: Option<String>,
    #[serde(default)]
    end: bool,
}

/// The workspace and herdr in it, for the terminal tools, once the caller
/// may attach and the machine runs.
fn open(d: &Daemon, org: &OrgId, a: &TermArgs, c: &Caller) -> Result<(Workspace, Client, bool)> {
    require(c, org, Role::Member, "the workspace's terminals")?;
    let wsm = &d.workspaces;
    let name = resolve_name(wsm, org, a.name.as_deref())?;
    let w = load(wsm, org, &name)?;
    let running = instance_status(d, org, &w).is_some_and(|s| s.eq_ignore_ascii_case("running"));
    Ok((w, wsm.oc(org), running))
}

/// `workspace_terminals`: the web terminal's mode and its sessions.
pub(super) fn workspace_terminals(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    let org = super::super::arg_org(&a)?;
    let a: TermArgs = args(a)?;
    let (w, oc, running) = open(d, &org, &a, c)?;
    if !running {
        return Ok(json!({"name": w.name, "running": false, "mode": null, "sessions": []}));
    }
    let h = Herdr::new(&oc, &w)?;
    Ok(match h.version() {
        Some(v) => json!({
            "name": w.name, "running": true, "mode": "herdr", "herdr": v,
            "herdr_workspace": HERDR_WORKSPACE, "sessions": h.sessions()?,
        }),
        None => json!({"name": w.name, "running": true, "mode": "shell", "sessions": []}),
    })
}

/// `workspace_terminal_update`: rename a herdr-backed session, or end it.
pub(super) fn workspace_terminal_update(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    let org = super::super::arg_org(&a)?;
    let a: TermArgs = args(a)?;
    let (w, oc, running) = open(d, &org, &a, c)?;
    let session = a
        .session
        .clone()
        .ok_or_else(|| Error::invalid("session is required"))?;
    if !running {
        return Err(Error::invalid(format!(
            "workspace {} is not running",
            w.name
        )));
    }
    let h = Herdr::new(&oc, &w)?;
    if h.version().is_none() {
        return Err(Error::invalid(
            "herdr is not installed in the workspace: its terminals are plain shells, which end when their tab closes",
        ));
    }
    if a.end {
        let ended = h.end(&session)?;
        return Ok(json!({"name": w.name, "session": session, "ended": ended}));
    }
    match &a.rename {
        Some(to) => {
            h.rename(&session, to)?;
            Ok(json!({"name": w.name, "session": to, "renamed_from": session}))
        }
        None => Err(Error::invalid("give rename or end: true")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(code: i32, stdout: &str) -> ExecOutput {
        ExecOutput {
            exit_code: code,
            stdout: stdout.as_bytes().to_vec(),
            stderr: b"oops".to_vec(),
        }
    }

    #[test]
    fn herdr_runs_as_argv_through_a_login_shell() {
        let a = attach_argv("term_65cf72a016f4d2");
        assert_eq!(
            a,
            [
                "sh",
                "-lc",
                "exec herdr \"$@\"",
                "herdr",
                "terminal",
                "attach",
                "term_65cf72a016f4d2",
                "--takeover"
            ]
        );
        // A session name is an argument, never shell text.
        let r = herdr_argv(&["tab", "rename", "w1:t2", "it's $(id)"]);
        assert_eq!(r.last().unwrap(), "it's $(id)");
    }

    #[test]
    fn session_names_cannot_pass_for_flags() {
        for ok in ["Shell 1", "build", "api: logs", "r2#3"] {
            assert!(check_session(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "--focus",
            "-x",
            " lead",
            "trail ",
            "a\nb",
            "a$(b)",
            &"x".repeat(41),
        ] {
            assert!(check_session(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn herdr_replies_find_the_workspace_tabs_and_terminal() {
        // Shapes as herdr 0.9.3 prints them.
        let list = reply(&out(0, r#"{"id":"cli:workspace:list","result":{"type":"workspace_list","workspaces":[{"label":"api","workspace_id":"w1"},{"label":"isb web","workspace_id":"w2"}]}}"#)).unwrap();
        assert_eq!(
            find_workspace(&list, HERDR_WORKSPACE).as_deref(),
            Some("w2")
        );
        assert_eq!(find_workspace(&list, "nope"), None);
        let t = reply(&out(0, r#"{"id":"cli:tab:list","result":{"tabs":[{"label":"Shell 1","pane_count":1,"tab_id":"w2:t1"},{"label":"logs","pane_count":2,"tab_id":"w2:t2"}],"type":"tab_list"}}"#)).unwrap();
        let t = tabs(&t);
        assert_eq!(t.len(), 2);
        assert_eq!(
            (t[1].name.as_str(), t[1].tab_id.as_str(), t[1].panes),
            ("logs", "w2:t2", 2)
        );
        let p = json!({"panes": [
            {"pane_id": "w2:p1", "tab_id": "w2:t1", "terminal_id": "term_a", "focused": true},
            {"pane_id": "w2:p2", "tab_id": "w2:t2", "terminal_id": "term_b", "focused": false},
            {"pane_id": "w2:p3", "tab_id": "w2:t2", "terminal_id": "term_c", "focused": true}
        ]});
        assert_eq!(terminal_of(&p, "w2:t1").as_deref(), Some("term_a"));
        assert_eq!(terminal_of(&p, "w2:t2").as_deref(), Some("term_c"));
        assert_eq!(terminal_of(&p, "w9:t9"), None);
        // Errors come back as ours.
        let e = reply(&out(1, r#"{"id":"cli:workspace:list","error":{"code":"server_not_running","message":"no herdr server"}}"#)).unwrap_err();
        assert!(e.to_string().contains("server_not_running"), "{e}");
        let e = reply(&out(2, "")).unwrap_err();
        assert!(e.to_string().contains("exit 2"), "{e}");
    }
}
