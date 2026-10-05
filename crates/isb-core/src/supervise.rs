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

/// The variables a service gets from secrets (`KEY: {secret: NAME}`), from
/// the values of its top-level secrets. A variable's value must be text.
pub fn secret_env(
    spec: &SandboxSpec,
    values: &BTreeMap<String, Vec<u8>>,
) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for (var, key) in &spec.env.secrets {
        let v = values
            .get(key)
            .ok_or_else(|| Error::invalid(format!("no value for secret {key:?}")))?;
        let text = String::from_utf8(v.clone())
            .ok()
            .filter(|t| !t.contains('\0'))
            .ok_or_else(|| {
                Error::invalid(format!(
                    "secret {key:?} cannot be the variable {var}: it is not text (NUL or invalid UTF-8); mount it as a file instead"
                ))
            })?;
        out.insert(var.clone(), text);
    }
    Ok(out)
}

/// Render the unit and its environment file. `secret_env` (from
/// [`secret_env`]) lands in the 0600 environment file only, after the
/// plain variables.
pub fn render(
    service: &str,
    spec: &SandboxSpec,
    login_shell: Option<&str>,
    uses_secrets: bool,
    secret_env: &BTreeMap<String, String>,
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
    let mut env: BTreeMap<String, String> = spec.env.vars.clone();
    env.extend(spec.exec.env.clone());
    env.extend(secret_env.clone());
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
    secret_env: &BTreeMap<String, String>,
) -> Result<bool> {
    install_with(sb, service, spec, uses_secrets, secret_env, true)
}

/// [`install`], restarting the app for a changed environment file only when
/// `restart_on_env`: a secret variable with `on_change: none` is written for
/// the next start and left alone. A changed unit always restarts it.
pub fn install_with(
    sb: &Sandbox,
    service: &str,
    spec: &SandboxSpec,
    uses_secrets: bool,
    secret_env: &BTreeMap<String, String>,
    restart_on_env: bool,
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
    let files = render(service, spec, shell.as_deref(), uses_secrets, secret_env)?;
    let unit_path = format!("/etc/systemd/system/{}", unit_name(service));
    let env_file = env_path(service);
    let same = |path: &str, want: &str| -> Result<bool> {
        Ok(client.read_file(name, path)?.as_deref() == Some(want.as_bytes()))
    };
    let unit_changed = !same(&unit_path, &files.unit)?;
    let env_changed = !same(&env_file, &files.env)?;
    let changed = unit_changed || env_changed;
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
    let restart = unit_changed || (env_changed && restart_on_env);
    let verb = if restart { "restart" } else { "start" };
    check(
        root_exec(
            sb,
            &["systemctl", verb, "--no-block", &unit],
            Duration::from_secs(60),
        )?,
        &format!("systemctl {verb} {unit}"),
    )?;
    Ok(restart)
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
/// `values` maps a top-level secret key to its value. Returns whether a
/// file was missing or different: an OCI app, already running when its
/// files arrive, needs a restart to read them.
pub fn push_secrets(
    sb: &Sandbox,
    spec: &SandboxSpec,
    values: &BTreeMap<String, Vec<u8>>,
) -> Result<bool> {
    push_secrets_detailed(sb, spec, values).map(|p| p.missing || !p.changed.is_empty())
}

/// What [`push_secrets_detailed`] found in the guest before writing.
#[derive(Debug, Default)]
pub struct Pushed {
    /// A file was not there at all: the app started without it.
    pub missing: bool,
    /// The top-level secrets whose file held another value.
    pub changed: std::collections::BTreeSet<String>,
}

/// [`push_secrets`], saying which secrets' files were missing or different,
/// so a caller can restart the app only for the ones that warrant it.
pub fn push_secrets_detailed(
    sb: &Sandbox,
    spec: &SandboxSpec,
    values: &BTreeMap<String, Vec<u8>>,
) -> Result<Pushed> {
    push_secret_files(sb.client(), sb.name(), spec, values)
}

/// [`push_secrets_detailed`] by instance name. The files API works on a
/// stopped container too, so a new OCI instance gets its files before its
/// first start (see `plan::Desired::before_start`).
pub fn push_secret_files(
    client: &Client,
    name: &str,
    spec: &SandboxSpec,
    values: &BTreeMap<String, Vec<u8>>,
) -> Result<Pushed> {
    let mut pushed = Pushed::default();
    if !spec.has_secret_files() {
        return Ok(pushed);
    }
    let (def_uid, def_gid) = numeric_user(spec.user.as_deref()).unwrap_or((0, 0));
    let refs = file_refs(client, name, spec)?;
    make_dirs(client, name, "/var/lib/isb")?;
    client.make_dir(name, SECRETS_STORE, 0, 0, 0o700)?;
    let mut script =
        String::from("#!/bin/sh\n# Written by isb: puts the secrets back after a boot.\nset -e\n");
    for (n, s) in refs.iter().enumerate() {
        let value = values
            .get(&s.source)
            .ok_or_else(|| Error::invalid(format!("{name}: no value for secret {:?}", s.source)))?;
        let path = s.guest_path();
        let parent = path.rsplit_once('/').map(|(p, _)| p).unwrap_or("/");
        make_dirs(client, name, parent)?;
        let mode = s.file_mode().map_err(Error::invalid)?;
        let (uid, gid) = (s.uid.unwrap_or(def_uid), s.gid.or(s.uid).unwrap_or(def_gid));
        match client.read_file(name, &path).ok().flatten() {
            None => pushed.missing = true,
            Some(v) if v != *value => {
                pushed.changed.insert(s.source.clone());
            }
            Some(_) => {}
        }
        client.push_file(name, &path, value, uid, gid, mode)?;
        let stored = format!("{SECRETS_STORE}/{n}");
        client.push_file(name, &stored, value, 0, 0, 0o400)?;
        script.push_str(&format!(
            "install -D -m {mode:04o} -o {uid} -g {gid} {} {}\n",
            sh_quote(&stored),
            sh_quote(&path)
        ));
    }
    client.push_file(name, SECRETS_RESTORE, script.as_bytes(), 0, 0, 0o700)?;
    Ok(pushed)
}

/// The files to write: `secrets:` as given, then each `as: file` variable's
/// secret at `/run/secrets/NAME`, 0400 and owned by the user the app starts
/// as (the numeric `user:`, else an OCI image's own `oci.uid`/`oci.gid`), so
/// an image that runs as non-root can read it. A path `secrets:` already
/// writes is left to it.
fn file_refs(
    client: &Client,
    name: &str,
    spec: &SandboxSpec,
) -> Result<Vec<crate::spec::SecretRef>> {
    let mut refs = spec.secrets.clone();
    if spec.env.files.is_empty() {
        return Ok(refs);
    }
    let (uid, gid) = match numeric_user(spec.user.as_deref()) {
        Some(ids) => ids,
        None => oci_ids(client, name)?,
    };
    let taken: std::collections::BTreeSet<String> = refs.iter().map(|r| r.guest_path()).collect();
    let keys: std::collections::BTreeSet<&String> = spec.env.files.values().collect();
    for key in keys {
        let r = crate::spec::SecretRef {
            source: key.clone(),
            uid: Some(uid),
            gid: Some(gid),
            mode: Some("0400".into()),
            ..Default::default()
        };
        if !taken.contains(&r.guest_path()) {
            refs.push(r);
        }
    }
    Ok(refs)
}

/// An OCI instance's `oci.uid`/`oci.gid` (incus fills them from the image's
/// `USER`); 0 when unset, as for a system image.
fn oci_ids(client: &Client, name: &str) -> Result<(u32, u32)> {
    let inst = client.get(&format!(
        "/1.0/instances/{}",
        crate::client::encode_segment(name)
    ))?;
    let id = |k: &str| {
        inst.pointer(&format!("/config/{}", k.replace('/', "~1")))
            .and_then(serde_json::Value::as_str)
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0)
    };
    Ok((id("oci.uid"), id("oci.gid")))
}

/// Set an OCI instance's secret variables in its config (`environment.KEY`),
/// where its next start reads them; the running app keeps what it has.
pub fn set_oci_env(sb: &Sandbox, env: &BTreeMap<String, String>) -> Result<()> {
    if env.is_empty() {
        return Ok(());
    }
    let config: serde_json::Map<String, serde_json::Value> = env
        .iter()
        .map(|(k, v)| {
            (
                format!("environment.{k}"),
                serde_json::Value::from(v.as_str()),
            )
        })
        .collect();
    sb.client().mutate(
        "PATCH",
        &format!(
            "/1.0/instances/{}",
            crate::client::encode_segment(sb.name())
        ),
        Some(&serde_json::json!({ "config": config })),
        "set secret variables",
        Duration::from_secs(60),
    )?;
    Ok(())
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

/// Read the secrets the file's services use that come from where the
/// deployer stands: a host file (relative to `base`) or one of `vars` / the
/// environment. The others (`external`, `age`, `driver`) come from the org's
/// store and the daemon's key, and are skipped here.
pub fn resolve_secret_values(
    file: &crate::spec::ComposeFile,
    base: &std::path::Path,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<BTreeMap<String, Vec<u8>>> {
    let used = crate::stack::secrets::used_keys(file);
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
            continue;
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
        let f = render("web", &s, None, false, &BTreeMap::new()).unwrap();
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
        let f = render("api", &s, None, true, &BTreeMap::new()).unwrap();
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
        assert!(
            render(
                "x",
                &spec("image: x\nrestart: always\n"),
                None,
                false,
                &BTreeMap::new()
            )
            .is_err()
        );
    }

    #[test]
    fn secret_variables_go_to_the_env_file_only() {
        let s = spec(
            "image: x\nrestart: always\ncommand: [/usr/bin/app]\nenvironment: {A: plain, TOKEN: {secret: tok}, DB: {secret: db}}\nexec: {env: {B: two}}\n",
        );
        let values = BTreeMap::from([
            ("tok".to_string(), b"s3cr$t \"x\"".to_vec()),
            ("db".to_string(), b"pw".to_vec()),
        ]);
        let env = secret_env(&s, &values).unwrap();
        assert_eq!(env["TOKEN"], "s3cr$t \"x\"");
        let f = render("app", &s, None, false, &env).unwrap();
        assert_eq!(
            f.env,
            "# Written by isb.\nA=\"plain\"\nB=\"two\"\nDB=\"pw\"\nTOKEN=\"s3cr\\$t \\\"x\\\"\"\n"
        );
        assert!(!f.unit.contains("s3cr"), "{}", f.unit);
        // Not text: refused, pointing at a file mount instead.
        let bad = BTreeMap::from([
            ("tok".to_string(), vec![0xff, 0x00]),
            ("db".to_string(), b"pw".to_vec()),
        ]);
        let e = secret_env(&s, &bad).unwrap_err().to_string();
        assert!(e.contains("not text") && e.contains("TOKEN"), "{e}");
        let e = secret_env(&s, &BTreeMap::new()).unwrap_err().to_string();
        assert!(e.contains("no value"), "{e}");
    }

    #[test]
    fn numeric_users() {
        assert_eq!(numeric_user(Some("1000")), Some((1000, 1000)));
        assert_eq!(numeric_user(Some("1000:44")), Some((1000, 44)));
        assert_eq!(numeric_user(Some("dev")), None);
    }
}
