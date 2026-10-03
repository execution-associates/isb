//! Long-running services: the app is supervised inside the guest, never held
//! open by an isb process, so it outlives `isb up`, a daemon restart or an isb
//! upgrade.
//!
//! - A system image runs `command` as a systemd unit, `isb-<service>.service`,
//!   with docker's restart policy mapped to `Restart=` and its environment in a
//!   0600 `EnvironmentFile`. Logs go to the guest's journal.
//! - An OCI image's process is the instance's init (`oci.entrypoint`), so
//!   incus itself restarts it (`boot.autorestart`). Logs are the console log.
//!
//! Secrets are files under `/run/secrets`, a tmpfs in a systemd guest. A
//! root-only copy is kept in `/var/lib/isb/secrets` with a script that puts
//! them back, which the unit runs before every start: after a reboot the app
//! has its secrets even when no isb is around to push them.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::client::Client;
use crate::error::{Error, Result};
use crate::exec::{ExecOptions, ExecOutput};
use crate::sandbox::Sandbox;
use crate::spec::{RestartCondition, RestartMode, SandboxSpec};

/// The persisted copies of a guest's secrets, and the script restoring them.
pub const SECRETS_STORE: &str = "/var/lib/isb/secrets";
pub const SECRETS_RESTORE: &str = "/var/lib/isb/secrets/restore";

/// The systemd unit for a service.
pub fn unit_name(service: &str) -> String {
    format!("isb-{}.service", crate::compose::sanitize_name(service))
}

fn env_path(service: &str) -> String {
    format!("/etc/isb/{}.env", crate::compose::sanitize_name(service))
}

/// The argv systemd should run: `command`, through the user's login shell
/// when `exec.login` is set, else through `/bin/sh` when the program is not an
/// absolute path, so `$PATH` from the environment file applies (systemd
/// itself only searches a fixed path).
pub fn effective_argv(spec: &SandboxSpec, login_shell: Option<&str>) -> Option<Vec<String>> {
    let argv = spec.command.clone().filter(|a| !a.is_empty())?;
    let wrap = |shell: &str, login: bool| {
        let mut v = vec![shell.to_string()];
        if login {
            v.push("-l".into());
        }
        v.extend(["-c".into(), "exec \"$@\"".into(), "isb".into()]);
        v.extend(argv.clone());
        v
    };
    Some(if spec.exec.login {
        wrap(login_shell.unwrap_or("/bin/sh"), true)
    } else if !argv[0].starts_with('/') {
        wrap("/bin/sh", false)
    } else {
        argv
    })
}

/// Quote one ExecStart word: systemd expands `$VAR` and `%` specifiers and
/// takes C escapes inside double quotes.
fn unit_word(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '$' => out.push_str("$$"),
            '%' => out.push_str("%%"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// One `KEY="VALUE"` line of an EnvironmentFile, escaped as systemd's
/// shell-like parser expects.
fn env_line(k: &str, v: &str) -> String {
    let mut out = format!("{k}=\"");
    for c in v.chars() {
        if matches!(c, '\\' | '"' | '`' | '$') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push_str("\"\n");
    out
}

/// What to write into the guest for a systemd-supervised service.
#[derive(Debug, Clone, PartialEq)]
pub struct UnitFiles {
    pub unit: String,
    pub env: String,
}

/// Render the unit and its environment file.
pub fn render(
    service: &str,
    spec: &SandboxSpec,
    login_shell: Option<&str>,
    uses_secrets: bool,
) -> Result<UnitFiles> {
    let argv = effective_argv(spec, login_shell).ok_or_else(|| {
        Error::invalid(format!("{service}: restart needs a command to supervise"))
    })?;
    let policy = spec.deploy.as_ref().and_then(|d| d.restart_policy.clone());
    let restart = match (
        spec.restart.unwrap_or_default(),
        policy.as_ref().and_then(|p| p.condition),
    ) {
        (_, Some(RestartCondition::None)) => "no",
        (_, Some(RestartCondition::OnFailure)) => "on-failure",
        (_, Some(RestartCondition::Any)) => "always",
        (RestartMode::OnFailure, None) => "on-failure",
        (RestartMode::No, None) => "no",
        _ => "always",
    };
    let delay = match policy.as_ref().and_then(|p| p.delay.as_deref()) {
        Some(d) => crate::flex::parse_duration(d).map_err(Error::invalid)?,
        None => Duration::from_secs(5),
    };
    let (burst, interval) = match policy.as_ref().and_then(|p| p.max_attempts) {
        Some(n) => {
            let window = match policy.as_ref().and_then(|p| p.window.as_deref()) {
                Some(w) => crate::flex::parse_duration(w).map_err(Error::invalid)?,
                // docker counts forever; a day is systemd's nearest useful window.
                None => Duration::from_secs(86400),
            };
            (Some(n.max(1)), window.as_secs().max(1))
        }
        None => (None, 0),
    };

    let mut u = String::new();
    u.push_str("# Written by isb; overwritten on the next `isb up` or deploy.\n");
    u.push_str(&format!("[Unit]\nDescription=isb service {service}\n"));
    u.push_str("After=network-online.target\nWants=network-online.target\n");
    u.push_str(&format!("StartLimitIntervalSec={interval}\n"));
    if let Some(b) = burst {
        u.push_str(&format!("StartLimitBurst={b}\n"));
    }
    u.push_str("\n[Service]\nType=simple\n");
    if let Some(user) = &spec.user {
        match user.split_once(':') {
            Some((name, group)) => {
                u.push_str(&format!("User={name}\nGroup={group}\n"));
            }
            None => u.push_str(&format!("User={user}\n")),
        }
    }
    match &spec.working_dir {
        Some(w) => u.push_str(&format!("WorkingDirectory={w}\n")),
        None if spec.user.is_some() => u.push_str("WorkingDirectory=~\n"),
        None => {}
    }
    u.push_str(&format!("EnvironmentFile={}\n", env_path(service)));
    if uses_secrets {
        // As root (+), whatever the service's user: the store is root-only.
        u.push_str(&format!("ExecStartPre=+/bin/sh {SECRETS_RESTORE}\n"));
    }
    u.push_str("ExecStart=");
    u.push_str(
        &argv
            .iter()
            .map(|a| unit_word(a))
            .collect::<Vec<_>>()
            .join(" "),
    );
    u.push('\n');
    u.push_str(&format!(
        "Restart={restart}\nRestartSec={}ms\n",
        delay.as_millis()
    ));
    u.push_str("KillMode=mixed\nTimeoutStopSec=10\n");
    u.push_str("\n[Install]\nWantedBy=multi-user.target\n");

    // incus' environment.* reaches exec, not systemd's services: repeat it.
    let mut env: BTreeMap<String, String> = spec.env.clone();
    env.extend(spec.exec.env.clone());
    let mut e = String::from("# Written by isb.\n");
    for (k, v) in &env {
        e.push_str(&env_line(k, v));
    }
    Ok(UnitFiles { unit: u, env: e })
}

fn root_exec(sb: &Sandbox, argv: &[&str], timeout: Duration) -> Result<ExecOutput> {
    sb.exec_with(
        argv.iter().map(|s| s.to_string()),
        ExecOptions::default()
            .user("root")
            .cwd("/")
            .timeout(timeout),
    )
}

fn check(out: ExecOutput, what: &str) -> Result<ExecOutput> {
    if out.success() {
        return Ok(out);
    }
    let detail = format!("{}{}", out.stdout_text(), out.stderr_text());
    Err(Error::OperationFailed {
        step: what.to_string(),
        message: format!("exit {}: {}", out.exit_code, detail.trim()),
    })
}

/// Install (or update) the unit for a long-running service and make sure it
/// is enabled and running. Returns true when the unit or its environment
/// changed, in which case the app was restarted.
pub fn install(
    sb: &Sandbox,
    service: &str,
    spec: &SandboxSpec,
    uses_secrets: bool,
) -> Result<bool> {
    let client = sb.client();
    let name = sb.name();
    if !root_exec(
        sb,
        &["test", "-d", "/run/systemd/system"],
        Duration::from_secs(30),
    )?
    .success()
    {
        return Err(Error::invalid(format!(
            "{name}: restart needs systemd in the guest to supervise command; this image has none (use an OCI image, or drop restart)"
        )));
    }
    wait_for_systemd(sb, Duration::from_secs(120))?;
    let shell = match (&spec.user, spec.exec.login) {
        (Some(u), true) => crate::exec::resolve_user(client, name, u)?.shell,
        (None, true) => crate::exec::resolve_user(client, name, "root")?.shell,
        _ => None,
    }
    .filter(|s| !s.ends_with("nologin") && !s.ends_with("/false"));
    let files = render(service, spec, shell.as_deref(), uses_secrets)?;
    let unit_path = format!("/etc/systemd/system/{}", unit_name(service));
    let env_file = env_path(service);
    let same = |path: &str, want: &str| -> Result<bool> {
        Ok(client.read_file(name, path)?.as_deref() == Some(want.as_bytes()))
    };
    let changed = !same(&unit_path, &files.unit)? || !same(&env_file, &files.env)?;
    let unit = unit_name(service);
    if changed {
        client.make_dir(name, "/etc/isb", 0, 0, 0o755)?;
        client.push_file(name, &env_file, files.env.as_bytes(), 0, 0, 0o600)?;
        client.push_file(name, &unit_path, files.unit.as_bytes(), 0, 0, 0o644)?;
    }
    // Also when unchanged: an earlier attempt may have written the files and
    // failed before reloading.
    let loaded = root_exec(
        sb,
        &[
            "systemctl",
            "show",
            "--value",
            "-p",
            "NeedDaemonReload",
            &unit,
        ],
        Duration::from_secs(30),
    )?;
    if changed || loaded.stdout_text().trim() != "no" {
        check(
            root_exec(sb, &["systemctl", "daemon-reload"], Duration::from_secs(60))?,
            "systemctl daemon-reload",
        )?;
    }
    check(
        root_exec(
            sb,
            &["systemctl", "enable", "--quiet", &unit],
            Duration::from_secs(60),
        )?,
        &format!("systemctl enable {unit}"),
    )?;
    // --no-block: a unit waiting for its secrets would otherwise hold this.
    let verb = if changed { "restart" } else { "start" };
    check(
        root_exec(
            sb,
            &["systemctl", verb, "--no-block", &unit],
            Duration::from_secs(60),
        )?,
        &format!("systemctl {verb} {unit}"),
    )?;
    Ok(changed)
}

/// A just-started guest has /run/systemd/system before systemd answers on
/// its bus. Wait for boot to finish (`degraded` counts: one failed unit of
/// the image's own is not ours to judge).
fn wait_for_systemd(sb: &Sandbox, deadline: Duration) -> Result<()> {
    let started = Instant::now();
    loop {
        let out = root_exec(
            sb,
            &["systemctl", "is-system-running", "--wait"],
            Duration::from_secs(60),
        )?;
        let state = out.stdout_text().trim().to_string();
        if matches!(state.as_str(), "running" | "degraded" | "maintenance") {
            return Ok(());
        }
        if started.elapsed() >= deadline {
            return Err(Error::NotReady {
                sandbox: sb.name().to_string(),
                check: "systemd".into(),
                detail: format!("{state} {}", out.stderr_text().trim()),
                waited: started.elapsed(),
            });
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Stop and disable a service's unit, if it is installed.
pub fn uninstall(sb: &Sandbox, service: &str) -> Result<()> {
    let unit = unit_name(service);
    let _ = root_exec(
        sb,
        &["systemctl", "disable", "--now", "--quiet", &unit],
        Duration::from_secs(60),
    )?;
    Ok(())
}

/// Restart the app: the unit for a system image, the instance for OCI.
pub fn restart_app(sb: &Sandbox, service: &str, oci: bool) -> Result<()> {
    if oci {
        return sb.restart();
    }
    let unit = unit_name(service);
    check(
        root_exec(
            sb,
            &["systemctl", "restart", "--no-block", &unit],
            Duration::from_secs(60),
        )?,
        &format!("systemctl restart {unit}"),
    )
    .map(|_| ())
}

/// The unit's state: `active`, `activating`, `failed`, `inactive`, ...
pub fn unit_state(sb: &Sandbox, service: &str) -> Result<String> {
    let out = root_exec(
        sb,
        &["systemctl", "is-active", &unit_name(service)],
        Duration::from_secs(30),
    )?;
    Ok(out.stdout_text().trim().to_string())
}

/// Write the service's secrets under `/run/secrets` (or their targets), and
/// the persisted copies plus the script that restores them after a boot.
/// `values` maps a top-level secret key to its value.
pub fn push_secrets(
    sb: &Sandbox,
    spec: &SandboxSpec,
    values: &BTreeMap<String, Vec<u8>>,
) -> Result<()> {
    if spec.secrets.is_empty() {
        return Ok(());
    }
    let client = sb.client();
    let name = sb.name();
    let (def_uid, def_gid) = numeric_user(spec.user.as_deref()).unwrap_or((0, 0));
    make_dirs(client, name, "/var/lib/isb")?;
    client.make_dir(name, SECRETS_STORE, 0, 0, 0o700)?;
    let mut script =
        String::from("#!/bin/sh\n# Written by isb: puts the secrets back after a boot.\nset -e\n");
    for (n, s) in spec.secrets.iter().enumerate() {
        let value = values
            .get(&s.source)
            .ok_or_else(|| Error::invalid(format!("{name}: no value for secret {:?}", s.source)))?;
        let path = s.guest_path();
        let parent = path.rsplit_once('/').map(|(p, _)| p).unwrap_or("/");
        make_dirs(client, name, parent)?;
        let mode = s.file_mode().map_err(Error::invalid)?;
        let (uid, gid) = (s.uid.unwrap_or(def_uid), s.gid.or(s.uid).unwrap_or(def_gid));
        client.push_file(name, &path, value, uid, gid, mode)?;
        let stored = format!("{SECRETS_STORE}/{n}");
        client.push_file(name, &stored, value, 0, 0, 0o400)?;
        script.push_str(&format!(
            "install -D -m {mode:04o} -o {uid} -g {gid} {} {}\n",
            sh_quote(&stored),
            sh_quote(&path)
        ));
    }
    client.push_file(name, SECRETS_RESTORE, script.as_bytes(), 0, 0, 0o700)
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn make_dirs(client: &Client, name: &str, path: &str) -> Result<()> {
    let mut cur = String::new();
    for part in path.split('/').filter(|p| !p.is_empty()) {
        cur.push('/');
        cur.push_str(part);
        client.make_dir(name, &cur, 0, 0, 0o755)?;
    }
    Ok(())
}

/// `1000` or `1000:1000` as ids; a name needs the guest to resolve it.
fn numeric_user(user: Option<&str>) -> Option<(u32, u32)> {
    let u = user?;
    let (a, b) = u.split_once(':').unwrap_or((u, u));
    Some((a.parse().ok()?, b.parse().ok()?))
}

/// Read every secret a file declares from where the deployer stands: a host
/// file (relative to `base`) or one of `vars` / the environment.
pub fn resolve_secret_values(
    file: &crate::spec::ComposeFile,
    base: &std::path::Path,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<BTreeMap<String, Vec<u8>>> {
    let used: std::collections::BTreeSet<&String> = file
        .services
        .values()
        .flat_map(|s| s.secrets.iter().map(|r| &r.source))
        .collect();
    let mut out = BTreeMap::new();
    for (key, def) in &file.secrets {
        if !used.contains(key) {
            continue;
        }
        let v = if let Some(f) = &def.file {
            let p = crate::plan::resolve_host_path(f, base)?;
            std::fs::read(&p)
                .map_err(|e| Error::invalid(format!("secret {key:?}: cannot read {p}: {e}")))?
        } else if let Some(var) = &def.environment {
            lookup(var)
                .ok_or_else(|| {
                    Error::invalid(format!(
                        "secret {key:?}: environment variable {var} is not set"
                    ))
                })?
                .into_bytes()
        } else {
            // external, age, driver: the daemon's store and key hold these
            // (P1.9/P1.10 wire them into stack deploys).
            return Err(Error::invalid(format!(
                "secret {key:?}: {} secrets are not resolved by `isb up` or by the client running `isb stack deploy`",
                def.source_kind()
            )));
        };
        out.insert(key.clone(), v);
    }
    Ok(out)
}

/// The outcome of one health probe.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Probe {
    pub ok: bool,
    /// Exit code, or None when it could not run or timed out.
    pub exit_code: Option<i32>,
    /// The tail of its output, for status displays.
    pub output: String,
    #[serde(skip)]
    pub took: Duration,
}

/// Run a service's healthcheck once.
pub fn probe(sb: &Sandbox, check: &crate::spec::HealthProbe) -> Probe {
    let started = Instant::now();
    let r = sb.exec_with(
        check.argv.clone(),
        ExecOptions::default().timeout(check.timeout),
    );
    let took = started.elapsed();
    match r {
        Ok(out) => {
            let mut text = format!("{}{}", out.stdout_text(), out.stderr_text());
            if text.len() > 512 {
                text = text[text.len() - 512..].to_string();
            }
            Probe {
                ok: out.success(),
                exit_code: Some(out.exit_code),
                output: text.trim().to_string(),
                took,
            }
        }
        Err(e) => Probe {
            ok: false,
            exit_code: None,
            output: e.to_string(),
            took,
        },
    }
}

/// The last `lines` lines of a service's output: its journal for a system
/// image, the console log for an OCI image.
pub fn logs(sb: &Sandbox, service: &str, oci: bool, lines: usize) -> Result<String> {
    if oci {
        let raw = console_log(sb.client(), sb.name())?;
        let text = String::from_utf8_lossy(&raw);
        let all: Vec<&str> = text.lines().collect();
        return Ok(all[all.len().saturating_sub(lines)..].join("\n"));
    }
    let n = lines.to_string();
    let out = root_exec(
        sb,
        &[
            "journalctl",
            "-u",
            &unit_name(service),
            "-n",
            &n,
            "-o",
            "short-iso",
            "--no-pager",
        ],
        Duration::from_secs(60),
    )?;
    Ok(check(out, "journalctl")?.stdout_text())
}

/// The argv that follows a unit's journal from now on, for foreground `up`.
pub fn follow_argv(service: &str) -> Vec<String> {
    ["journalctl", "-f", "-n", "0", "-o", "cat", "-u"]
        .iter()
        .map(|s| s.to_string())
        .chain([unit_name(service)])
        .collect()
}

/// An instance's console log (an OCI app's stdout and stderr).
pub fn console_log(client: &Client, name: &str) -> Result<Vec<u8>> {
    client.console_log(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(y: &str) -> SandboxSpec {
        serde_yaml_ng::from_str(y).unwrap()
    }

    #[test]
    fn renders_a_unit() {
        let s = spec(
            "image: x\nuser: dev\nworking_dir: /srv\nrestart: always\ncommand: bun run dev --port=$PORT\nenvironment: {A: 'x \"y\" $z'}\n",
        );
        let f = render("web", &s, None, false).unwrap();
        assert!(f.unit.contains("User=dev\n"), "{}", f.unit);
        assert!(f.unit.contains("WorkingDirectory=/srv\n"));
        assert!(f.unit.contains("Restart=always\n"));
        assert!(f.unit.contains("RestartSec=5000ms\n"));
        assert!(f.unit.contains("StartLimitIntervalSec=0\n"));
        assert!(
            f.unit.contains(
                "ExecStart=\"/bin/sh\" \"-c\" \"exec \\\"$$@\\\"\" \"isb\" \"bun\" \"run\" \"dev\" \"--port=$$PORT\"\n"
            ),
            "{}",
            f.unit
        );
        assert!(!f.unit.contains("ExecStartPre"));
        assert_eq!(f.env, "# Written by isb.\nA=\"x \\\"y\\\" \\$z\"\n");
    }

    #[test]
    fn absolute_command_runs_directly_and_policy_maps() {
        let s = spec(
            "image: x\nrestart: on-failure\ncommand: [/usr/bin/app, '50%']\ndeploy: {restart_policy: {delay: 2s, max_attempts: 3, window: 1m}}\n",
        );
        let f = render("api", &s, None, true).unwrap();
        assert!(
            f.unit.contains("ExecStart=\"/usr/bin/app\" \"50%%\"\n"),
            "{}",
            f.unit
        );
        assert!(f.unit.contains("Restart=on-failure\n"));
        assert!(f.unit.contains("RestartSec=2000ms\n"));
        assert!(
            f.unit
                .contains("StartLimitIntervalSec=60\nStartLimitBurst=3\n")
        );
        assert!(
            f.unit
                .contains("ExecStartPre=+/bin/sh /var/lib/isb/secrets/restore\n")
        );
        assert!(!f.unit.contains("User="));
    }

    #[test]
    fn login_shell_wraps() {
        let s = spec("image: x\nrestart: always\ncommand: [bun, dev]\nexec: {login: true}\n");
        assert_eq!(
            effective_argv(&s, Some("/bin/bash")).unwrap(),
            ["/bin/bash", "-l", "-c", "exec \"$@\"", "isb", "bun", "dev"]
        );
        assert!(render("x", &spec("image: x\nrestart: always\n"), None, false).is_err());
    }

    #[test]
    fn numeric_users() {
        assert_eq!(numeric_user(Some("1000")), Some((1000, 1000)));
        assert_eq!(numeric_user(Some("1000:44")), Some((1000, 44)));
        assert_eq!(numeric_user(Some("dev")), None);
    }
}
