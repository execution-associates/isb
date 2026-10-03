//! Installing `isb serve` as a systemd user service.
//!
//! The unit runs the isb binary that installed it, by its canonical path. A
//! version manager that keeps each version in its own directory (mise does)
//! therefore pins that version: after an upgrade, install again to point the
//! unit at the new binary. Settings live in an environment file that is
//! created once and then left to the user.

use std::net::ToSocketAddrs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

pub const UNIT_NAME: &str = "isb.service";
pub const DEFAULT_LISTEN: &str = "127.0.0.1:8092";
pub const LISTEN_ENV: &str = "ISB_SERVE_LISTEN";

#[derive(Debug, Clone, Default)]
pub struct ServiceOptions {
    /// Loopback address to serve on. Default: the env file's
    /// `ISB_SERVE_LISTEN`, else [`DEFAULT_LISTEN`]. Given explicitly, it is
    /// written to the env file.
    pub listen: Option<String>,
    /// How long to wait for `/healthz` to answer 200. Default 30s.
    pub health_timeout: Option<Duration>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ServiceInstall {
    pub exe: PathBuf,
    pub unit_path: PathBuf,
    pub env_path: PathBuf,
    pub listen: String,
    pub health_url: String,
    /// Things the user should know, one sentence each (linger, version pinning).
    pub notes: Vec<String>,
}

/// Write the unit and env file, (re)start the service, and wait until it is
/// healthy. Safe to run again: it updates an existing installation.
pub fn install_user_service(opts: &ServiceOptions) -> Result<ServiceInstall> {
    if cfg!(target_os = "macos") {
        return Err(Error::invalid(
            "on macOS isb serve runs in the isb machine; install the LaunchAgent that starts \
             it with isb::machine::install_launch_agent (`isb serve install`)",
        ));
    }
    if !cfg!(target_os = "linux") {
        return Err(Error::invalid(
            "installing the service needs Linux with systemd user services",
        ));
    }
    let config = config_dir()?;
    let env_path = config.join("isb/serve.env");
    let unit_path = config.join("systemd/user").join(UNIT_NAME);
    let exe = std::env::current_exe()?.canonicalize()?;

    let existing = match std::fs::read_to_string(&env_path) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let listen = opts
        .listen
        .clone()
        .or_else(|| existing.as_deref().and_then(|s| env_value(s, LISTEN_ENV)))
        .unwrap_or_else(|| DEFAULT_LISTEN.to_string());
    check_loopback(&listen)?;

    let env_text = match &existing {
        None => Some(render_env(&listen)),
        Some(s) if opts.listen.is_some() && env_value(s, LISTEN_ENV).as_ref() != Some(&listen) => {
            Some(set_env_value(s, LISTEN_ENV, &listen))
        }
        Some(_) => None,
    };
    if let Some(text) = env_text {
        // The env file may come to hold credentials: keep it private.
        if let Some(dir) = env_path.parent() {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
        }
        write_atomic(&env_path, text.as_bytes(), 0o600)?;
    }
    write_atomic(&unit_path, render_unit(&exe, &env_path).as_bytes(), 0o644)?;

    for args in [
        &["daemon-reload"][..],
        &["enable", UNIT_NAME],
        &["restart", UNIT_NAME],
    ] {
        systemctl(args)?;
    }

    let health_url = format!("http://{listen}/healthz");
    wait_healthy(
        &listen,
        opts.health_timeout.unwrap_or(Duration::from_secs(30)),
    )
    .map_err(|e| Error::OperationFailed {
        step: format!("start {UNIT_NAME}"),
        message: format!(
            "{health_url} did not answer 200: {e}; inspect with `journalctl --user -u {UNIT_NAME}`"
        ),
    })?;

    let mut notes = Vec::new();
    if let Some(user) = std::env::var("USER")
        .ok()
        .or_else(|| std::env::var("LOGNAME").ok())
        .filter(|u| !u.is_empty() && !Path::new("/var/lib/systemd/linger").join(u).exists())
    {
        notes.push(format!(
            "lingering is off for {user}, so the service stops when you log out; \
             run `loginctl enable-linger {user}` to keep it running"
        ));
    }
    if exe.to_string_lossy().contains("/mise/installs/") {
        notes.push(format!(
            "the unit runs {} directly; install the service again after upgrading isb",
            exe.display()
        ));
    }
    Ok(ServiceInstall {
        exe,
        unit_path,
        env_path,
        listen,
        health_url,
        notes,
    })
}

fn config_dir() -> Result<PathBuf> {
    if let Some(d) = std::env::var_os("XDG_CONFIG_HOME").filter(|s| !s.is_empty()) {
        return Ok(PathBuf::from(d));
    }
    std::env::var_os("HOME")
        .filter(|s| !s.is_empty())
        .map(|h| PathBuf::from(h).join(".config"))
        .ok_or_else(|| Error::invalid("HOME is not set"))
}

fn check_loopback(listen: &str) -> Result<()> {
    let addrs: Vec<_> = listen
        .to_socket_addrs()
        .map_err(|e| Error::invalid(format!("listen address {listen:?}: {e}")))?
        .collect();
    if addrs.is_empty() || addrs.iter().any(|a| !a.ip().is_loopback()) {
        return Err(Error::invalid(format!(
            "listen address {listen:?} is not loopback; expose it through a Cloudflare Tunnel instead"
        )));
    }
    Ok(())
}

fn systemctl(args: &[&str]) -> Result<()> {
    let out = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()?;
    if !out.status.success() {
        return Err(Error::OperationFailed {
            step: format!("systemctl --user {}", args.join(" ")),
            message: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(())
}

fn wait_healthy(listen: &str, timeout: Duration) -> std::result::Result<(), String> {
    let started = Instant::now();
    let mut last = "no answer".to_string();
    while started.elapsed() < timeout {
        match super::client::healthz(listen, Duration::from_secs(2)) {
            Ok((200, _)) => return Ok(()),
            Ok((s, _)) => last = format!("HTTP {s}"),
            Err(e) => last = e.to_string(),
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err(last)
}

/// The unit file. No sandboxing directives: the service drives incusd and
/// reads the user's projects, and most of them need a system manager anyway.
pub fn render_unit(exe: &Path, env_path: &Path) -> String {
    format!(
        "[Unit]
Description=isb serve: incus app stacks and MCP server
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
EnvironmentFile=-{}
ExecStart={} serve
Restart=always
RestartSec=2

[Install]
WantedBy=default.target
",
        escape_env_path(&env_path.to_string_lossy()),
        quote(&exe.to_string_lossy()),
    )
}

/// The env file written on first install.
pub fn render_env(listen: &str) -> String {
    format!(
        "# isb serve settings, read by the {UNIT_NAME} user unit.

# Loopback address for /mcp and /healthz; point cloudflared here.
{LISTEN_ENV}={listen}

# The Cloudflare Access application in front of the tunnel hostname. Every
# /mcp request on {LISTEN_ENV} must then carry a valid Access assertion.
#CF_ACCESS_TEAM_DOMAIN=yourteam.cloudflareaccess.com
#CF_ACCESS_AUD=
"
    )
}

/// `KEY=value` from env-file text: comments skipped, `export ` and quotes
/// tolerated, the last assignment wins as it does for systemd.
fn env_value(text: &str, key: &str) -> Option<String> {
    let mut found = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        if let Some((_, v)) = line.split_once('=').filter(|(k, _)| k.trim() == key) {
            found = Some(v.trim().trim_matches(['"', '\'']).to_string());
        }
    }
    found
}

/// Replace every `key=` assignment with `key=value`, or append one.
fn set_env_value(text: &str, key: &str, value: &str) -> String {
    let mut done = false;
    let mut out: Vec<String> = text
        .lines()
        .map(|l| {
            let t = l.trim();
            let t = t.strip_prefix("export ").unwrap_or(t);
            if !t.starts_with('#') && t.split_once('=').is_some_and(|(k, _)| k.trim() == key) {
                done = true;
                format!("{key}={value}")
            } else {
                l.to_string()
            }
        })
        .collect();
    if !done {
        out.push(format!("{key}={value}"));
    }
    out.join("\n") + "\n"
}

/// An ExecStart argument: quoted, with systemd's `%` and `$` expansion escaped.
fn quote(s: &str) -> String {
    let s = s
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%")
        .replace('$', "$$");
    format!("\"{s}\"")
}

fn escape_env_path(s: &str) -> String {
    s.replace('\\', "\\x5c")
        .replace(' ', "\\x20")
        .replace('\t', "\\x09")
        .replace('%', "%%")
}

fn write_atomic(path: &Path, content: &[u8], mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if std::fs::read(path).is_ok_and(|c| c == content) {
        return Ok(());
    }
    let dir = path
        .parent()
        .ok_or_else(|| Error::invalid(format!("{} has no parent", path.display())))?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    std::fs::write(&tmp, content)?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_rendering() {
        let u = render_unit(
            Path::new("/home/me/.local/bin/isb"),
            Path::new("/home/me/.config/isb/serve.env"),
        );
        assert!(
            u.contains("\nExecStart=\"/home/me/.local/bin/isb\" serve\n"),
            "{u}"
        );
        assert!(u.contains("\nEnvironmentFile=-/home/me/.config/isb/serve.env\n"));
        assert!(u.contains("\nRestart=always\nRestartSec=2\n"));
        assert!(u.contains("After=network-online.target"));
        assert!(u.contains("WantedBy=default.target"));
        assert!(!u.contains("Protect"), "no sandboxing directives");
        let odd = render_unit(Path::new("/opt/my isb/100%$x\"/isb"), Path::new("/a b/%e"));
        assert!(
            odd.contains("ExecStart=\"/opt/my isb/100%%$$x\\\"/isb\" serve"),
            "{odd}"
        );
        assert!(odd.contains("EnvironmentFile=-/a\\x20b/%%e"), "{odd}");
    }

    #[test]
    fn env_rendering_and_editing() {
        let e = render_env("127.0.0.1:9000");
        assert_eq!(env_value(&e, LISTEN_ENV).as_deref(), Some("127.0.0.1:9000"));
        assert!(e.contains("#CF_ACCESS_TEAM_DOMAIN="));
        assert!(e.contains("#CF_ACCESS_AUD="));
        assert_eq!(env_value(&e, "CF_ACCESS_AUD"), None, "commented out");

        let edited = set_env_value(&e, LISTEN_ENV, "127.0.0.1:9001");
        assert_eq!(
            env_value(&edited, LISTEN_ENV).as_deref(),
            Some("127.0.0.1:9001")
        );
        assert!(edited.contains("#CF_ACCESS_AUD="), "rest untouched");
        let appended = set_env_value("A=1", LISTEN_ENV, "[::1]:1");
        assert_eq!(appended, "A=1\nISB_SERVE_LISTEN=[::1]:1\n");
        assert_eq!(
            env_value("export ISB_SERVE_LISTEN=\"localhost:1\"\n", LISTEN_ENV).as_deref(),
            Some("localhost:1")
        );
    }

    #[test]
    fn listen_must_be_loopback() {
        assert!(check_loopback("127.0.0.1:8092").is_ok());
        assert!(check_loopback("[::1]:8092").is_ok());
        assert!(check_loopback("0.0.0.0:8092").is_err());
        assert!(check_loopback("nonsense").is_err());
    }

    #[test]
    fn atomic_write_is_idempotent_and_sets_mode() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x/serve.env");
        write_atomic(&p, b"a", 0o600).unwrap();
        write_atomic(&p, b"a", 0o600).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"a");
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(std::fs::read_dir(p.parent().unwrap()).unwrap().count(), 1);
    }
}
