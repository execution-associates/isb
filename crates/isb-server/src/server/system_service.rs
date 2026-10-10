//! Installing `isb serve` as a systemd system unit that runs as the
//! operator: `isb serve install --system`.
//!
//! A user unit lives in the user's slice (`user-UID.slice`), with everything
//! else that user runs. On a host where agents run as that same user, the
//! slice's limits (MemoryHigh, MemoryMax, TasksMax) are theirs, and an agent
//! that fills the slice stalls the daemon with it. A system unit runs in
//! `system.slice` instead, with its own limits, while keeping the user, home,
//! runtime directory and state the CLI expects, so nothing else changes: the
//! CLI finds the daemon at `$XDG_RUNTIME_DIR/isb/serve.sock` as before.
//!
//! The installer runs as that user and uses sudo for what needs root (the
//! unit, the system credential, systemctl, lingering).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use super::service::{
    self, CREDENTIAL_FILE, ServiceInstall, ServiceOptions, UNIT_NAME, escape_env_path, quote,
};
use crate::error::{Error, Result};

/// Where the system unit is written.
pub const UNIT_PATH: &str = "/etc/systemd/system/isb.service";
/// The system credential store `LoadCredentialEncrypted=` searches by name.
pub const CREDSTORE: &str = "/etc/credstore.encrypted";
/// The unit's own ceilings. Far above what the daemon uses (hundreds of MB,
/// a few hundred tasks), so only a leak in isb itself reaches them. Raise
/// them with a drop-in (`sudo systemctl edit isb`).
pub const MEMORY_MAX: &str = "8G";
pub const TASKS_MAX: u32 = 4096;

/// Who the daemon runs as.
#[derive(Debug, Clone)]
pub struct Operator {
    pub user: String,
    pub uid: u32,
    pub home: PathBuf,
}

/// Is the daemon installed as a system unit on this host?
pub fn installed() -> bool {
    Path::new(UNIT_PATH).exists()
}

/// Write the system unit and its credential, retire a user unit, (re)start
/// the service and wait until it is healthy. Run as the daemon's user, not
/// root. Safe to run again: it updates an existing installation.
pub fn install_system_service(opts: &ServiceOptions) -> Result<ServiceInstall> {
    if !cfg!(target_os = "linux") {
        return Err(Error::invalid("--system needs Linux with systemd"));
    }
    if rustix::process::geteuid().is_root() {
        return Err(Error::invalid(
            "run `isb serve install --system` as the user the daemon runs as, not root: it uses \
             sudo for the parts that need root",
        ));
    }
    let op = operator()?;
    let config = service::config_dir()?;
    let env_path = config.join("isb/serve.env");
    let exe = std::env::current_exe()?.canonicalize()?;
    let listen = service::prepare_env(opts, &env_path)?;
    let mut notes = Vec::new();

    if !Path::new("/var/lib/systemd/linger").join(&op.user).exists() {
        sudo(&["loginctl", "enable-linger", &op.user], None)?;
        notes.push(format!(
            "turned on lingering for {}: the daemon's runtime directory /run/user/{} must exist \
             from boot, without a login",
            op.user, op.uid
        ));
    }
    let credential = setup_system_key(&config, &mut notes)?;

    // Retire a user unit first: both would serve the same socket and port.
    let user_unit = config.join("systemd/user").join(UNIT_NAME);
    if user_unit.exists() {
        let _ = Command::new("systemctl")
            .args(["--user", "disable", "--now", UNIT_NAME])
            .stdin(Stdio::null())
            .output();
        std::fs::remove_file(&user_unit)?;
        let _ = Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .stdin(Stdio::null())
            .output();
        notes.push(format!(
            "stopped and removed the user unit {}",
            user_unit.display()
        ));
    }

    let unit = render_system_unit(&op, &exe, &env_path, credential.is_some());
    sudo_write(Path::new(UNIT_PATH), unit.as_bytes(), "0644")?;
    for args in [
        &["systemctl", "daemon-reload"][..],
        &["systemctl", "enable", "--quiet", UNIT_NAME],
        &["systemctl", "restart", UNIT_NAME],
    ] {
        sudo(args, None)?;
    }

    let health_url = format!("http://{listen}/healthz");
    service::wait_healthy(
        &listen,
        opts.health_timeout.unwrap_or(Duration::from_secs(30)),
    )
    .map_err(|e| Error::OperationFailed {
        step: format!("start {UNIT_NAME}"),
        message: format!(
            "{health_url} did not answer 200: {e}; inspect with `sudo journalctl -u {UNIT_NAME}`"
        ),
    })?;
    Ok(ServiceInstall {
        exe,
        unit_path: PathBuf::from(UNIT_PATH),
        env_path,
        listen,
        health_url,
        key_credential: credential,
        notes,
    })
}

fn operator() -> Result<Operator> {
    let user = std::env::var("USER")
        .ok()
        .or_else(|| std::env::var("LOGNAME").ok())
        .filter(|u| !u.is_empty())
        .ok_or_else(|| Error::invalid("cannot tell who the daemon runs as ($USER is unset)"))?;
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| Error::invalid("HOME is not set"))?;
    Ok(Operator {
        user,
        uid: rustix::process::getuid().as_raw(),
        home,
    })
}

/// Put the daemon's age key in the system credential store, encrypted with
/// the host key, so `LoadCredentialEncrypted=isb-age-key` finds it. The key
/// comes from the plaintext file, else the user credential an earlier
/// `isb serve install` made; one is generated only when neither exists and
/// no system credential does either.
fn setup_system_key(config: &Path, notes: &mut Vec<String>) -> Result<Option<PathBuf>> {
    use crate::secrets::keys;
    let target = Path::new(CREDSTORE).join(keys::CREDENTIAL_NAME);
    let sources = keys::KeySources::from_env();
    if Command::new("systemd-creds")
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .map_or(true, |o| !o.status.success())
    {
        let k = keys::load_identity(&sources)?;
        notes.extend(k.notes);
        notes.push(format!(
            "systemd-creds is not available, so the daemon reads its secrets key from {}: keep \
             that file out of unencrypted backups",
            sources.default_file.display()
        ));
        return Ok(None);
    }
    let user_cred = config.join(CREDENTIAL_FILE);
    let text = match keys::find_identity(&sources) {
        Ok(k) => keys::identity_file_text(&k.identity),
        Err(_) if sudo_exists(&target) => {
            notes.push(format!(
                "kept the daemon's secrets key in the system credential {}",
                target.display()
            ));
            return Ok(Some(target));
        }
        Err(_) if user_cred.exists() => decrypt_user_credential(&user_cred)?,
        Err(_) => keys::identity_file_text(&keys::load_identity(&sources)?.identity),
    };
    let public = keys::parse_identity(&text)?.to_public();
    sudo(&["install", "-d", "-m", "0700", CREDSTORE], None)?;
    let tmp = Path::new(CREDSTORE).join(format!(".isb-age-key.{}.tmp", std::process::id()));
    let tmp_s = tmp.to_string_lossy();
    let name = format!("--name={}", keys::CREDENTIAL_NAME);
    let r = sudo(
        &["systemd-creds", "encrypt", &name, "-", &tmp_s],
        Some(text.as_bytes()),
    )
    .and_then(|()| sudo(&["chmod", "0600", &tmp_s], None))
    .and_then(|()| sudo(&["mv", "-f", &tmp_s, &target.to_string_lossy()], None));
    if r.is_err() {
        let _ = sudo(&["rm", "-f", &tmp_s], None);
    }
    r?;
    notes.push(format!(
        "the daemon's secrets key ({public}) is an encrypted system credential, {}, bound to this \
         host",
        target.display()
    ));
    if user_cred.exists() {
        notes.push(format!(
            "the user credential {} is no longer read; remove it once the daemon is healthy",
            user_cred.display()
        ));
    }
    Ok(Some(target))
}

/// The key text from a user credential (`systemd-creds encrypt --user`),
/// which only this user can decrypt.
fn decrypt_user_credential(path: &Path) -> Result<String> {
    let out = Command::new("systemd-creds")
        .args(["decrypt", "--user"])
        .arg(format!("--name={}", crate::secrets::keys::CREDENTIAL_NAME))
        .arg(path)
        .arg("-")
        .stdin(Stdio::null())
        .output()?;
    if !out.status.success() {
        return Err(Error::OperationFailed {
            step: format!("systemd-creds decrypt --user {}", path.display()),
            message: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    String::from_utf8(out.stdout).map_err(|_| Error::invalid("the user credential is not text"))
}

/// The unit. It runs as the operator with their runtime directory, so the
/// socket, state and config are where the CLI looks; it is ordered after the
/// user manager, which creates that directory (lingering starts it at boot).
pub fn render_system_unit(op: &Operator, exe: &Path, env_path: &Path, credential: bool) -> String {
    let uid = op.uid;
    let bin = escape_env_path(&op.home.join(".local/bin").to_string_lossy());
    let cred = if credential {
        format!(
            "LoadCredentialEncrypted={}\n",
            crate::secrets::keys::CREDENTIAL_NAME
        )
    } else {
        String::new()
    };
    format!(
        "[Unit]
Description=isb serve: incus app stacks and MCP server
Wants=network-online.target user@{uid}.service
After=network-online.target incus.service user@{uid}.service

[Service]
Type=simple
User={user}
WorkingDirectory=~
Environment=XDG_RUNTIME_DIR=/run/user/{uid}
Environment=PATH={bin}:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
EnvironmentFile=-{env}
{cred}ExecStart={exe} serve
Restart=always
RestartSec=2
MemoryMax={MEMORY_MAX}
TasksMax={TASKS_MAX}

[Install]
WantedBy=multi-user.target
",
        user = op.user,
        env = escape_env_path(&env_path.to_string_lossy()),
        exe = quote(&exe.to_string_lossy()),
    )
}

/// `sudo -- args`, with `input` on stdin (never in argv: it may be a key).
fn sudo(args: &[&str], input: Option<&[u8]>) -> Result<()> {
    let mut child = Command::new("sudo")
        .arg("--")
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::inherit()
        })
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::OperationFailed {
            step: format!("sudo {}", args.join(" ")),
            message: format!("could not run sudo: {e}"),
        })?;
    if let (Some(data), Some(mut stdin)) = (input, child.stdin.take()) {
        stdin.write_all(data)?;
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(Error::OperationFailed {
            step: format!("sudo {}", args.join(" ")),
            message: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(())
}

/// Does `path` exist, looked at as root (the credential store is 0700)?
fn sudo_exists(path: &Path) -> bool {
    Command::new("sudo")
        .args(["--", "test", "-e"])
        .arg(path)
        .stdin(Stdio::inherit())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Write a root-owned file through sudo: next to it, then renamed.
fn sudo_write(path: &Path, content: &[u8], mode: &str) -> Result<()> {
    if std::fs::read(path).is_ok_and(|c| c == content) {
        return Ok(());
    }
    let dir = path
        .parent()
        .ok_or_else(|| Error::invalid(format!("{} has no parent", path.display())))?;
    let tmp = dir.join(format!(".isb.service.{}.tmp", std::process::id()));
    let tmp_s = tmp.to_string_lossy();
    let r = sudo(&["sh", "-c", "cat > \"$1\"", "sh", &tmp_s], Some(content))
        .and_then(|()| sudo(&["chmod", mode, &tmp_s], None))
        .and_then(|()| sudo(&["mv", "-f", &tmp_s, &path.to_string_lossy()], None));
    if r.is_err() {
        let _ = sudo(&["rm", "-f", &tmp_s], None);
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op() -> Operator {
        Operator {
            user: "me".into(),
            uid: 1000,
            home: PathBuf::from("/home/me"),
        }
    }

    #[test]
    fn system_unit_runs_as_the_operator_outside_their_slice() {
        let u = render_system_unit(
            &op(),
            Path::new("/home/me/.local/bin/isb"),
            Path::new("/home/me/.config/isb/serve.env"),
            true,
        );
        assert!(u.contains("\nUser=me\n"), "{u}");
        assert!(u.contains("\nWorkingDirectory=~\n"), "{u}");
        assert!(
            u.contains("\nEnvironment=XDG_RUNTIME_DIR=/run/user/1000\n"),
            "the CLI's socket path: {u}"
        );
        assert!(u.contains("Environment=PATH=/home/me/.local/bin:/usr/local/sbin:"));
        assert!(u.contains("\nWants=network-online.target user@1000.service\n"));
        assert!(u.contains("\nAfter=network-online.target incus.service user@1000.service\n"));
        assert!(u.contains(
            "\nEnvironmentFile=-/home/me/.config/isb/serve.env\nLoadCredentialEncrypted=isb-age-key\nExecStart=\"/home/me/.local/bin/isb\" serve\n"
        ), "{u}");
        assert!(u.contains("\nMemoryMax=8G\nTasksMax=4096\n"), "{u}");
        assert!(u.contains("\nWantedBy=multi-user.target\n"), "{u}");
        assert!(!u.contains("Slice="), "system.slice, the default");
    }

    #[test]
    fn system_unit_without_a_credential() {
        let u = render_system_unit(
            &op(),
            Path::new("/opt/my isb/isb"),
            Path::new("/home/me/.config/isb/serve.env"),
            false,
        );
        assert!(!u.contains("Credential"), "{u}");
        assert!(u.contains("ExecStart=\"/opt/my isb/isb\" serve"), "{u}");
    }
}
