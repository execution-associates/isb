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
    /// The daemon's secrets key as an encrypted systemd credential, when
    /// this systemd can make one.
    pub key_credential: Option<PathBuf>,
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
    let key = setup_key(&config)?;

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
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let cred = key
        .credential
        .as_deref()
        .map(|p| credential_path(p, home.as_deref()));
    write_atomic(
        &unit_path,
        render_unit(&exe, &env_path, cred.as_deref()).as_bytes(),
        0o644,
    )?;

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

    let mut notes = key.notes;
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
        key_credential: key.credential,
        notes,
    })
}

/// The daemon's secrets key as the install left it.
struct KeySetup {
    /// The encrypted credential, when systemd-creds could make one.
    credential: Option<PathBuf>,
    notes: Vec<String>,
}

/// The credential file `isb serve install` writes, under the config dir.
pub const CREDENTIAL_FILE: &str = "isb/isb-age-key.cred";

/// Put the daemon's age key in an encrypted systemd credential where
/// systemd-creds can make user credentials (systemd 256+); else leave it in
/// the key file. The key is generated if there is none, but never when a
/// credential already holds it.
fn setup_key(config: &Path) -> Result<KeySetup> {
    use crate::secrets::keys;
    let sources = keys::KeySources::from_env();
    let cred = config.join(CREDENTIAL_FILE);
    let key_file = sources.default_file.clone();
    let version = systemd_version();
    if version.is_none_or(|v| v < 256) {
        // Make sure there is a key, so the daemon does not generate one at
        // first start unnoticed.
        let k = keys::load_identity(&sources)?;
        let mut notes = k.notes;
        notes.push(format!(
            "systemd-creds here cannot encrypt user credentials ({}; needs systemd 256+), so the daemon reads its secrets key from {}: keep that file out of unencrypted backups, and add a break-glass recipient (docs/secrets.md)",
            version.map_or("not found".to_string(), |v| format!("systemd {v}")),
            key_file.display()
        ));
        return Ok(KeySetup {
            credential: None,
            notes,
        });
    }
    let mut notes = Vec::new();
    let k = match keys::find_identity(&sources) {
        Ok(k) => k,
        // The plaintext was removed after an earlier install: the
        // credential is the key now. Keep it; never mint a new one.
        Err(_) if cred.exists() => {
            notes.push(format!(
                "kept the daemon's secrets key in the systemd credential {}",
                cred.display()
            ));
            return Ok(KeySetup {
                credential: Some(cred),
                notes,
            });
        }
        Err(_) => keys::load_identity(&sources)?,
    };
    notes.extend(k.notes);
    encrypt_credential(&keys::identity_file_text(&k.identity), &cred)?;
    notes.push(format!(
        "the daemon's secrets key ({}) is now an encrypted systemd credential, {}, bound to this machine and user",
        k.identity.to_public(),
        cred.display()
    ));
    if key_file.exists() {
        notes.push(format!(
            "the daemon no longer needs the plaintext key; to remove it: `shred -u {}`. Before you do, add a break-glass recipient (recipients = [...] in {}) and `isb secret reencrypt --all`: the credential cannot be decrypted on another machine, so without one, losing this host loses every secret. `isb up` without a running daemon also reads that file",
            key_file.display(),
            keys::SecretsConfig::default_path().display()
        ));
    }
    Ok(KeySetup {
        credential: Some(cred),
        notes,
    })
}

/// `systemd-creds --version`'s major version.
fn systemd_version() -> Option<u32> {
    let out = Command::new("systemd-creds")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_systemd_version(&String::from_utf8_lossy(&out.stdout))
}

/// `systemd 259 (259.5-0ubuntu3.4)` -> 259.
fn parse_systemd_version(text: &str) -> Option<u32> {
    let first = text.lines().next()?;
    let mut words = first.split_whitespace();
    if words.next()? != "systemd" {
        return None;
    }
    words.next()?.parse().ok()
}

/// Encrypt `text` as the user credential `isb-age-key` into `path` (0600):
/// written next to it, then renamed, so a failure leaves the old one.
fn encrypt_credential(text: &str, path: &Path) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let dir = path
        .parent()
        .ok_or_else(|| Error::invalid(format!("{} has no parent", path.display())))?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let tmp = dir.join(format!(".isb-age-key.cred.{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let mut child = Command::new("systemd-creds")
        .args(["encrypt", "--user"])
        .arg(format!("--name={}", crate::secrets::keys::CREDENTIAL_NAME))
        .arg("-")
        .arg(&tmp)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes())?;
    }
    let out = child.wait_with_output()?;
    let r = (|| -> Result<()> {
        if !out.status.success() {
            return Err(Error::OperationFailed {
                step: "systemd-creds encrypt --user".into(),
                message: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            });
        }
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r
}

/// The credential's path as the unit names it: `%h/...` under the home
/// directory, else absolute; escaped for a unit file either way.
pub fn credential_path(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) => format!("%h/{}", escape_env_path(&rest.to_string_lossy())),
        None => escape_env_path(&path.to_string_lossy()),
    }
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
/// `credential` (from [`credential_path`]) loads the encrypted secrets key.
pub fn render_unit(exe: &Path, env_path: &Path, credential: Option<&str>) -> String {
    let cred = credential
        .map(|c| {
            format!(
                "LoadCredentialEncrypted={}:{c}\n",
                crate::secrets::keys::CREDENTIAL_NAME
            )
        })
        .unwrap_or_default();
    format!(
        "[Unit]
Description=isb serve: incus app stacks and MCP server
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
EnvironmentFile=-{}
{cred}ExecStart={} serve
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
            None,
        );
        assert!(!u.contains("Credential"), "{u}");
        assert!(
            u.contains("\nExecStart=\"/home/me/.local/bin/isb\" serve\n"),
            "{u}"
        );
        assert!(u.contains("\nEnvironmentFile=-/home/me/.config/isb/serve.env\n"));
        assert!(u.contains("\nRestart=always\nRestartSec=2\n"));
        assert!(u.contains("After=network-online.target"));
        assert!(u.contains("WantedBy=default.target"));
        assert!(!u.contains("Protect"), "no sandboxing directives");
        let odd = render_unit(
            Path::new("/opt/my isb/100%$x\"/isb"),
            Path::new("/a b/%e"),
            None,
        );
        assert!(
            odd.contains("ExecStart=\"/opt/my isb/100%%$$x\\\"/isb\" serve"),
            "{odd}"
        );
        assert!(odd.contains("EnvironmentFile=-/a\\x20b/%%e"), "{odd}");
    }

    #[test]
    fn unit_loads_the_encrypted_key() {
        let home = Path::new("/home/me");
        let c = credential_path(
            Path::new("/home/me/.config/isb/isb-age-key.cred"),
            Some(home),
        );
        assert_eq!(c, "%h/.config/isb/isb-age-key.cred");
        let u = render_unit(
            Path::new("/home/me/.local/bin/isb"),
            Path::new("/home/me/.config/isb/serve.env"),
            Some(&c),
        );
        assert!(
            u.contains(
                "\nEnvironmentFile=-/home/me/.config/isb/serve.env\nLoadCredentialEncrypted=isb-age-key:%h/.config/isb/isb-age-key.cred\nExecStart="
            ),
            "{u}"
        );
        // Outside the home directory: absolute, escaped.
        assert_eq!(
            credential_path(Path::new("/srv/cfg 1/k%.cred"), Some(home)),
            "/srv/cfg\\x201/k%%.cred"
        );
        assert_eq!(
            credential_path(Path::new("/srv/k.cred"), None),
            "/srv/k.cred"
        );
    }

    #[test]
    fn systemd_versions() {
        assert_eq!(
            parse_systemd_version("systemd 259 (259.5-0ubuntu3.4)\n+PAM +AUDIT"),
            Some(259)
        );
        assert_eq!(
            parse_systemd_version("systemd 255 (255.4-1ubuntu8)"),
            Some(255)
        );
        assert_eq!(parse_systemd_version("something else"), None);
        assert_eq!(parse_systemd_version(""), None);
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
