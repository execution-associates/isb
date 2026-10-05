//! Workspaces (docs/concepts/workspaces.md): an org's long-lived machine, where its
//! people and agents work, and the rules for the short-lived sandboxes
//! beside it.
//!
//! This module is the model: a workspace's definition and where it is kept,
//! the org's workspace settings (how many workspaces, sandbox expiry and
//! idle defaults), and the pure decisions the daemon makes from them (a
//! sandbox's deadlines, whether the reaper takes it). The daemon's side,
//! the tools, the instance and the token, is `daemon::workspaces`.
//!
//! On disk, under the org's state directory (`<state>/workspaces/` for the
//! default org, `<state>/orgs/<org>/workspaces/` for the others), 0600:
//!
//! - `<name>.json`: the definition, with the token's id and SHA-256 (never
//!   the token);
//! - `<name>.token.age`: the token itself, encrypted to the daemon's age
//!   key, so it can be delivered again into a restarted or rebuilt machine;
//! - `settings.json`: the org's workspace settings.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::auth::Role;
use crate::error::{Error, Result};
use crate::org::OrgId;

/// The name a workspace gets when none is given; with one workspace per
/// org, the only name most orgs ever see.
pub const DEFAULT_NAME: &str = "workspace";

/// The workspace user when none is given (`dev-base`'s, uid 1000).
pub const DEFAULT_USER: &str = "dev";

/// The home volume's size when none is given.
pub const DEFAULT_HOME_SIZE: &str = "20GiB";

/// Instance config keys (`user.*`) isb keeps on workspaces and sandboxes.
pub const KEY_WORKSPACE: &str = "user.isb.workspace";
pub const KEY_EXPIRES_AT: &str = "user.isb.expires_at";
pub const KEY_IDLE_TIMEOUT: &str = "user.isb.idle_timeout";

/// The longest a sandbox may live from now, by default or extended.
pub const MAX_SANDBOX_LIFETIME: Duration = Duration::from_secs(30 * 86400);

/// Where the token is delivered inside the workspace.
pub const TOKEN_PATH: &str = "/run/isb/token";
/// Named org secrets, one file each.
pub const SECRETS_DIR: &str = "/run/isb/secrets";
/// Login shells' environment: `ISB_URL`, `ISB_ORG`, `ISB_TOKEN`, the
/// workspace's own variables.
pub const PROFILE_PATH: &str = "/etc/profile.d/isb.sh";

/// A workspace's definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub name: String,
    /// Stable across rebuilds; a new workspace of the same name gets a new
    /// one.
    pub id: String,
    /// An incus image (`dev-base`, `images:ubuntu/24.04`) or a registry
    /// image (`registry:APP:TAG`).
    pub image: String,
    /// The workspace user: its home is the home volume.
    pub user: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpus: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
    /// The root disk's size; the pool's default when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_size: Option<String>,
    pub home_size: String,
    /// Plain variables for login shells (not secrets: use `secrets`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Org secrets delivered as files under `/run/isb/secrets/<NAME>`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    /// The role the workspace's token has in its org.
    pub token_role: Role,
    /// A host directory bound as the home instead of a volume: under the
    /// daemon's `--workspace-home-root`, or a path a superadmin named.
    /// isb never deletes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_bind: Option<String>,
    /// The storage pool the home volume is in, decided at create.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<String>,
    pub created_at: u64,
    pub created_by: String,
    #[serde(default)]
    pub updated_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rebuilt_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<TokenMeta>,
    /// A first-boot script: run once as root on the first start after a
    /// create or a rebuild, and again on demand. Not for secrets: the
    /// definition shows it (use `secrets`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<String>,
    /// Where the setup script is: pending, running, succeeded or failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_state: Option<SetupState>,
    /// The ports it publishes (`workspace_port_add`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<PublishedPort>,
}

/// The largest setup script accepted.
pub const MAX_SETUP: usize = 64 * 1024;

/// A setup script's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SetupStatus {
    /// To run on the next start (or now, when running).
    Pending,
    Running,
    Succeeded,
    Failed,
}

/// The setup script's last run, or the one to come.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupState {
    pub status: SetupStatus,
    /// When the status last changed.
    pub at: u64,
    /// How many times it has started.
    #[serde(default)]
    pub runs: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// What happens to a setup script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupEvent {
    /// The machine was created or rebuilt.
    Built,
    /// Someone asked to run it again.
    Requested,
    /// The daemon began running it.
    Started,
    /// It ended with this exit code.
    Finished(i32),
    /// It could not run (the push or the exec failed, or it timed out).
    Error(String),
    /// The daemon started: a run it had begun is gone.
    DaemonStarted,
}

/// The setup state after `ev`: `None` when there is no script.
pub fn setup_next(
    script: bool,
    cur: Option<&SetupState>,
    ev: SetupEvent,
    now: u64,
) -> Result<Option<SetupState>> {
    use SetupStatus::*;
    if !script {
        return match ev {
            SetupEvent::Requested => Err(Error::invalid(
                "the workspace has no setup script (set one with workspace_update)",
            )),
            _ => Ok(None),
        };
    }
    let runs = cur.map_or(0, |c| c.runs);
    let status = cur.map(|c| c.status);
    let st = |status, exit_code, message| SetupState {
        status,
        at: now,
        runs,
        exit_code,
        message,
    };
    Ok(Some(match (ev, status) {
        (SetupEvent::Requested, Some(Running)) => {
            return Err(Error::invalid(
                "the setup script is running; wait for it to end",
            ));
        }
        (SetupEvent::Built | SetupEvent::Requested, _) => st(Pending, None, None),
        (SetupEvent::Started, Some(Pending)) => SetupState {
            runs: runs + 1,
            ..st(Running, None, None)
        },
        (SetupEvent::Finished(0), Some(Running)) => st(Succeeded, Some(0), None),
        (SetupEvent::Finished(c), Some(Running)) => st(Failed, Some(c), None),
        (SetupEvent::Error(m), Some(Running)) => st(Failed, None, Some(m)),
        (SetupEvent::DaemonStarted, Some(Running)) => st(
            Failed,
            None,
            Some("interrupted: the daemon restarted while it ran".into()),
        ),
        (ev @ (SetupEvent::Started | SetupEvent::Finished(_) | SetupEvent::Error(_)), _) => {
            return Err(Error::invalid(format!(
                "setup: {ev:?} while it is {}",
                status.map_or("not set up".into(), |s| format!("{s:?}").to_lowercase())
            )));
        }
        (SetupEvent::DaemonStarted, _) => return Ok(cur.cloned()),
    }))
}

/// Whether the setup script should run now (the machine running).
pub fn setup_due(w: &Workspace) -> bool {
    w.setup.is_some()
        && w.setup_state
            .as_ref()
            .is_some_and(|s| s.status == SetupStatus::Pending)
}

/// A port the workspace publishes: always through isb's own preview proxy
/// (for members, on a preview origin of its own), and on `host` through
/// the org's ingress when one is given (docs/concepts/workspaces.md#ports).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedPort {
    pub port: u16,
    /// The hostname the org's ingress serves it on, like an app's domain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// `host` was generated (`auto`, under sslip.io): outside the allowlist.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto: bool,
    pub added_by: String,
    pub added_at: u64,
}

/// How many ports one workspace may publish.
pub const MAX_PORTS: usize = 20;

/// What is kept of the workspace's token: never the token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenMeta {
    /// A short random id, for the audit log and the UI.
    pub id: String,
    /// SHA-256 of the token, hex.
    pub hash: String,
    pub created_at: u64,
}

impl Workspace {
    /// The incus instance: the workspace's name.
    pub fn instance(&self) -> &str {
        &self.name
    }

    /// The home volume in the org's project.
    pub fn home_volume(&self, org: &OrgId) -> String {
        home_volume(org, &self.name)
    }

    /// The workspace user's home directory.
    pub fn home_dir(&self) -> String {
        if self.user == "root" {
            "/root".into()
        } else {
            format!("/home/{}", self.user)
        }
    }
}

/// A host-folder home under `root`: `<root>/<org>/home` for the org's
/// `workspace`, `<root>/<org>/<name>/home` for another name.
pub fn host_home(root: &Path, org: &OrgId, name: &str) -> PathBuf {
    if name == DEFAULT_NAME {
        root.join(org.as_str()).join("home")
    } else {
        root.join(org.as_str()).join(name).join("home")
    }
}

/// `<org>_<name>_home`, e.g. `acme_workspace_home`.
pub fn home_volume(org: &OrgId, name: &str) -> String {
    format!("{}_{name}_home", org.as_str())
}

/// A workspace name: `[a-z][a-z0-9-]*`, at most 30 characters (it is also
/// the instance's name).
pub fn check_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 30
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.ends_with('-');
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "workspace name {name:?}: [a-z0-9-], starting with a letter, at most 30 characters"
        )))
    }
}

/// A guest user name for the workspace user.
pub fn check_user(user: &str) -> Result<()> {
    let ok = !user.is_empty()
        && user.len() <= 32
        && user.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
        && user
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "user {user:?}: a lowercase user name ([a-z_][a-z0-9_-]*)"
        )))
    }
}

/// An environment variable name a login shell accepts.
pub fn check_env_name(k: &str) -> Result<()> {
    let ok = !k.is_empty()
        && k.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if !ok {
        return Err(Error::invalid(format!(
            "environment variable {k:?}: letters, digits and _, not starting with a digit"
        )));
    }
    if k.starts_with("ISB_") {
        return Err(Error::invalid(format!(
            "environment variable {k}: ISB_* are set by isb"
        )));
    }
    Ok(())
}

/// An org's workspace settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// How many workspaces the org may have. One: the org's box is a single
    /// place. Platform admins can lift it.
    #[serde(default = "one")]
    pub max_workspaces: u32,
    /// A new sandbox's lifetime (`24h`); at most 30 days.
    #[serde(default = "default_expiry")]
    pub sandbox_expiry: String,
    /// How long a sandbox may sit idle before it is reaped (`2h`), or
    /// `none`.
    #[serde(default = "default_idle")]
    pub sandbox_idle: String,
    /// How often the org's workspaces' secrets that are driver references
    /// (`vault/item/field`) are checked for a new version (`1h`; at least
    /// `10s`). A new one is written into the running workspaces.
    #[serde(default = "default_secret_refresh")]
    pub secret_refresh: String,
    /// `volume` or `host`: where new homes go, overriding the daemon
    /// (`host` needs `--workspace-home-root`). Platform admins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_kind: Option<String>,
    /// The storage pool new workspace homes go in (platform admins); unset:
    /// the daemon's `--workspace-pool`, else the org's default pool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_pool: Option<String>,
}

fn one() -> u32 {
    1
}

fn default_expiry() -> String {
    "24h".into()
}

fn default_idle() -> String {
    "2h".into()
}

fn default_secret_refresh() -> String {
    "1h".into()
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            max_workspaces: one(),
            sandbox_expiry: default_expiry(),
            sandbox_idle: default_idle(),
            secret_refresh: default_secret_refresh(),
            home_kind: None,
            home_pool: None,
        }
    }
}

impl Settings {
    /// Check the values a caller set.
    pub fn validate(&self) -> Result<()> {
        if self.max_workspaces == 0 || self.max_workspaces > 100 {
            return Err(Error::invalid("max_workspaces: 1 to 100"));
        }
        lifetime(&self.sandbox_expiry)?;
        idle(&self.sandbox_idle)?;
        secret_refresh(&self.secret_refresh)?;
        Ok(())
    }
}

/// A sandbox lifetime: a duration up to [`MAX_SANDBOX_LIFETIME`].
pub fn lifetime(s: &str) -> Result<Duration> {
    let d = crate::flex::parse_duration(s.trim())
        .map_err(|e| Error::invalid(format!("expiry {s:?}: {e}")))?;
    if d.is_zero() || d > MAX_SANDBOX_LIFETIME {
        return Err(Error::invalid(format!(
            "expiry {s:?}: between a second and 30 days"
        )));
    }
    Ok(d)
}

/// How often workspace driver references are polled: at least 10 seconds.
pub fn secret_refresh(s: &str) -> Result<Duration> {
    let d = crate::flex::parse_duration(s.trim())
        .map_err(|e| Error::invalid(format!("secret_refresh {s:?}: {e}")))?;
    if d < Duration::from_secs(10) {
        return Err(Error::invalid(format!(
            "secret_refresh {s:?}: at least 10s"
        )));
    }
    Ok(d)
}

/// An idle timeout: a duration of at least a minute, or `none`.
pub fn idle(s: &str) -> Result<Option<Duration>> {
    let s = s.trim();
    if matches!(s, "none" | "never" | "off") {
        return Ok(None);
    }
    let d = crate::flex::parse_duration(s)
        .map_err(|e| Error::invalid(format!("idle timeout {s:?}: {e}")))?;
    if d < Duration::from_secs(60) || d > MAX_SANDBOX_LIFETIME {
        return Err(Error::invalid(format!(
            "idle timeout {s:?}: between a minute and 30 days, or none"
        )));
    }
    Ok(Some(d))
}

/// A new sandbox's deadlines from the org's settings and what the caller
/// asked for: `(expires_at, idle_timeout_secs)`.
pub fn sandbox_deadlines(
    settings: &Settings,
    expires: Option<&str>,
    idle_timeout: Option<&str>,
    now: u64,
) -> Result<(u64, Option<u64>)> {
    let life = lifetime(expires.unwrap_or(&settings.sandbox_expiry))?;
    let idle = idle(idle_timeout.unwrap_or(&settings.sandbox_idle))?;
    Ok((now + life.as_secs(), idle.map(|d| d.as_secs())))
}

/// A sandbox's expiry pushed out by `by` (from the later of now and its
/// current expiry), capped at 30 days from now.
pub fn extended(current: Option<u64>, by: Duration, now: u64) -> Result<u64> {
    if by.is_zero() {
        return Err(Error::invalid("extend by a positive duration"));
    }
    let from = current.unwrap_or(now).max(now);
    let want = from.saturating_add(by.as_secs());
    let cap = now + MAX_SANDBOX_LIFETIME.as_secs();
    if want > cap {
        return Err(Error::invalid(format!(
            "a sandbox lives at most 30 days from now; the most it can be extended by is {}",
            human(cap.saturating_sub(from))
        )));
    }
    Ok(want)
}

/// `90061` -> `1d 1h`: the two largest units.
pub fn human(secs: u64) -> String {
    let units = [(86400, "d"), (3600, "h"), (60, "m"), (1, "s")];
    let mut parts = Vec::new();
    let mut rest = secs;
    for (n, u) in units {
        if rest >= n {
            parts.push(format!("{}{u}", rest / n));
            rest %= n;
        }
        if parts.len() == 2 {
            break;
        }
    }
    if parts.is_empty() {
        "0s".into()
    } else {
        parts.join(" ")
    }
}

/// What kind of instance this is to isb, from its `user.*` config (keys
/// without the `user.` prefix, as the metrics sample has them).
pub fn kind_of(labels: &BTreeMap<String, String>) -> &'static str {
    if labels.contains_key("isb.workspace") {
        "workspace"
    } else if labels.contains_key("isb.stack") {
        "replica"
    } else if labels.contains_key("isb.build") {
        "build"
    } else {
        "sandbox"
    }
}

/// Why the reaper takes this instance now, if it does. Only sandboxes isb
/// gave deadlines (`isb.expires_at`, `isb.idle_timeout`) are candidates:
/// never a workspace, a stack replica or an instance isb did not make.
/// `last_active` is the latest activity isb saw (unix seconds).
pub fn reap_reason(
    labels: &BTreeMap<String, String>,
    now: u64,
    last_active: u64,
) -> Option<String> {
    if kind_of(labels) != "sandbox" {
        return None;
    }
    let num = |k: &str| labels.get(k).and_then(|v| v.trim().parse::<u64>().ok());
    if let Some(at) = num("isb.expires_at") {
        if now >= at {
            return Some(format!("expired {} ago", human(now - at)));
        }
    }
    if let Some(idle) = num("isb.idle_timeout").filter(|s| *s > 0) {
        if now.saturating_sub(last_active) >= idle {
            return Some(format!(
                "idle for {}",
                human(now.saturating_sub(last_active))
            ));
        }
    }
    None
}

/// The directory an org's workspaces are kept in.
pub fn dir(state: &Path, org: &OrgId) -> PathBuf {
    crate::app::org_root(state, org).join("workspaces")
}

/// Reads and writes workspace definitions and settings.
#[derive(Debug, Clone)]
pub struct Store {
    state: PathBuf,
}

impl Store {
    pub fn new(state: &Path) -> Store {
        Store {
            state: state.to_path_buf(),
        }
    }

    fn path(&self, org: &OrgId, name: &str) -> PathBuf {
        dir(&self.state, org).join(format!("{name}.json"))
    }

    pub fn token_path(&self, org: &OrgId, name: &str) -> PathBuf {
        dir(&self.state, org).join(format!("{name}.token.age"))
    }

    pub fn list(&self, org: &OrgId) -> Result<Vec<Workspace>> {
        let d = dir(&self.state, org);
        let mut out = Vec::new();
        let rd = match std::fs::read_dir(&d) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e.into()),
        };
        for e in rd.flatten() {
            let p = e.path();
            let Some(stem) = p
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".json"))
            else {
                continue;
            };
            if stem == "settings" || check_name(stem).is_err() {
                continue;
            }
            match self.get(org, stem) {
                Ok(Some(w)) => out.push(w),
                Ok(None) => {}
                Err(e) => eprintln!("isb serve: workspace {}: {e}", p.display()),
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    pub fn get(&self, org: &OrgId, name: &str) -> Result<Option<Workspace>> {
        check_name(name)?;
        match std::fs::read(self.path(org, name)) {
            Ok(b) => Ok(Some(serde_json::from_slice(&b).map_err(|e| {
                Error::invalid(format!("workspace {name} in {org}: {e}"))
            })?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn put(&self, org: &OrgId, w: &Workspace) -> Result<()> {
        check_name(&w.name)?;
        crate::app::write_atomic(&self.path(org, &w.name), &serde_json::to_vec_pretty(w)?)
    }

    pub fn delete(&self, org: &OrgId, name: &str) -> Result<()> {
        for p in [self.path(org, name), self.token_path(org, name)] {
            match std::fs::remove_file(&p) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    pub fn settings(&self, org: &OrgId) -> Result<Settings> {
        match std::fs::read(dir(&self.state, org).join("settings.json")) {
            Ok(b) => serde_json::from_slice(&b)
                .map_err(|e| Error::invalid(format!("workspace settings of {org}: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn put_settings(&self, org: &OrgId, s: &Settings) -> Result<()> {
        s.validate()?;
        crate::app::write_atomic(
            &dir(&self.state, org).join("settings.json"),
            &serde_json::to_vec_pretty(s)?,
        )
    }

    /// Every org with a workspace directory: the default org's, then each
    /// `orgs/<org>/workspaces`.
    pub fn orgs(&self) -> Vec<OrgId> {
        let mut out = Vec::new();
        if dir(&self.state, &OrgId::default_org()).is_dir() {
            out.push(OrgId::default_org());
        }
        if let Ok(rd) = std::fs::read_dir(self.state.join("orgs")) {
            for e in rd.flatten() {
                let Some(n) = e.file_name().to_str().map(String::from) else {
                    continue;
                };
                if let Ok(o) = OrgId::new(n) {
                    if !o.is_default() && dir(&self.state, &o).is_dir() {
                        out.push(o);
                    }
                }
            }
        }
        out.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        out
    }
}

/// Shell-quote `s` for a POSIX shell (single quotes).
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `/etc/profile.d/isb.sh`: the workspace's variables, then isb's. The token
/// is read from its file, never written here.
pub fn profile(url: Option<&str>, org: &OrgId, w: &Workspace) -> String {
    let mut s = String::from(
        "# Written by isb for this workspace (docs/concepts/workspaces.md); rewritten on every start.\n",
    );
    for (k, v) in &w.env {
        s.push_str(&format!("export {k}={}\n", sh_quote(v)));
    }
    if let Some(u) = url {
        s.push_str(&format!("export ISB_URL={}\n", sh_quote(u)));
    }
    s.push_str(&format!("export ISB_ORG={}\n", sh_quote(org.as_str())));
    s.push_str(&format!("export ISB_WORKSPACE={}\n", sh_quote(&w.name)));
    s.push_str(&format!(
        "if [ -r {TOKEN_PATH} ]; then ISB_TOKEN=$(cat {TOKEN_PATH}); export ISB_TOKEN; fi\n"
    ));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(kv: &[(&str, &str)]) -> BTreeMap<String, String> {
        kv.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn host_folder_homes() {
        let org = OrgId::new("ocai").unwrap();
        let root = Path::new("/srv/workspaces");
        assert_eq!(
            host_home(root, &org, DEFAULT_NAME),
            Path::new("/srv/workspaces/ocai/home")
        );
        assert_eq!(
            host_home(root, &org, "lab"),
            Path::new("/srv/workspaces/ocai/lab/home")
        );
    }

    #[test]
    fn human_durations() {
        assert_eq!(human(0), "0s");
        assert_eq!(human(59), "59s");
        assert_eq!(human(3600 + 120 + 5), "1h 2m");
        assert_eq!(human(90061), "1d 1h");
    }

    #[test]
    fn names_users_and_env() {
        assert!(check_name("workspace").is_ok());
        assert!(check_name("ws-2").is_ok());
        for bad in ["", "2ws", "Ws", "ws_", "ws-", &"a".repeat(31)] {
            assert!(check_name(bad).is_err(), "{bad}");
        }
        assert!(check_user("dev").is_ok());
        assert!(check_user("Dev").is_err());
        assert!(check_env_name("EDITOR").is_ok());
        assert!(check_env_name("ISB_TOKEN").is_err());
        assert!(check_env_name("1X").is_err());
    }

    #[test]
    fn settings_default_and_bounds() {
        let s = Settings::default();
        assert_eq!(s.max_workspaces, 1);
        assert_eq!(s.sandbox_expiry, "24h");
        assert_eq!(s.sandbox_idle, "2h");
        s.validate().unwrap();
        let mut b = s.clone();
        b.sandbox_expiry = "31d".into();
        assert!(b.validate().is_err());
        b.sandbox_expiry = "7d".into();
        b.sandbox_idle = "none".into();
        b.validate().unwrap();
        b.max_workspaces = 0;
        assert!(b.validate().is_err());
        // A file from before a field existed gets its default.
        let old: Settings = serde_json::from_str(r#"{"max_workspaces": 2}"#).unwrap();
        assert_eq!(old.sandbox_idle, "2h");
    }

    #[test]
    fn deadlines_and_extension() {
        let s = Settings::default();
        let (exp, idle) = sandbox_deadlines(&s, None, None, 1000).unwrap();
        assert_eq!(exp, 1000 + 86400);
        assert_eq!(idle, Some(7200));
        let (exp, idle) = sandbox_deadlines(&s, Some("1h"), Some("none"), 0).unwrap();
        assert_eq!((exp, idle), (3600, None));
        assert!(sandbox_deadlines(&s, Some("40d"), None, 0).is_err());
        assert!(sandbox_deadlines(&s, None, Some("10s"), 0).is_err());
        // From the later of now and the current expiry.
        assert_eq!(
            extended(Some(500), Duration::from_secs(100), 1000).unwrap(),
            1100
        );
        assert_eq!(
            extended(Some(2000), Duration::from_secs(100), 1000).unwrap(),
            2100
        );
        assert!(extended(None, Duration::from_secs(31 * 86400), 0).is_err());
        assert!(extended(None, Duration::ZERO, 0).is_err());
    }

    #[test]
    fn the_reaper_takes_only_sandboxes_past_their_deadlines() {
        let now = 10_000;
        let exp = labels(&[("isb.expires_at", "9000")]);
        assert!(reap_reason(&exp, now, now).unwrap().starts_with("expired"));
        let live = labels(&[("isb.expires_at", "20000"), ("isb.idle_timeout", "600")]);
        assert_eq!(reap_reason(&live, now, now - 100), None);
        assert!(
            reap_reason(&live, now, now - 600)
                .unwrap()
                .starts_with("idle")
        );
        // Never a workspace, a replica, a build or an instance without
        // deadlines, whatever its labels say.
        for extra in ["isb.workspace", "isb.stack", "isb.build"] {
            let mut l = exp.clone();
            l.insert(extra.into(), "x".into());
            assert_eq!(reap_reason(&l, now, 0), None, "{extra}");
        }
        assert_eq!(
            reap_reason(&labels(&[("isb.owner", "mcp:a")]), now, 0),
            None
        );
        assert_eq!(
            reap_reason(&labels(&[("isb.expires_at", "soon")]), now, 0),
            None
        );
    }

    #[test]
    fn the_store_round_trips() {
        let d = tempfile::tempdir().unwrap();
        let st = Store::new(d.path());
        let org = OrgId::new("acme").unwrap();
        assert!(st.list(&org).unwrap().is_empty());
        assert_eq!(st.settings(&org).unwrap(), Settings::default());
        let w = Workspace {
            name: "workspace".into(),
            id: "abc".into(),
            image: "dev-base".into(),
            user: "dev".into(),
            cpus: Some(2),
            memory: None,
            root_size: None,
            home_size: "10GiB".into(),
            env: BTreeMap::new(),
            secrets: vec![],
            labels: BTreeMap::new(),
            token_role: Role::Admin,
            home_bind: None,
            pool: None,
            created_at: 1,
            created_by: "a@x.io".into(),
            updated_at: 1,
            rebuilt_at: None,
            token: None,
            setup: None,
            setup_state: None,
            ports: vec![],
        };
        st.put(&org, &w).unwrap();
        st.put_settings(&org, &Settings::default()).unwrap();
        assert_eq!(st.list(&org).unwrap(), vec![w.clone()]);
        assert_eq!(st.orgs(), vec![org.clone()]);
        assert_eq!(w.home_volume(&org), "acme_workspace_home");
        assert_eq!(w.home_dir(), "/home/dev");
        st.delete(&org, "workspace").unwrap();
        assert!(st.get(&org, "workspace").unwrap().is_none());
    }

    #[test]
    fn the_profile_quotes_values_and_never_holds_the_token() {
        let org = OrgId::new("acme").unwrap();
        let mut w: Workspace = serde_json::from_value(serde_json::json!({
            "name": "workspace", "id": "x", "image": "dev-base", "user": "dev",
            "home_size": "1GiB", "token_role": "admin", "created_at": 0, "created_by": "a"
        }))
        .unwrap();
        w.env.insert("GREETING".into(), "it's".into());
        let p = profile(Some("http://10.0.0.1:8481"), &org, &w);
        assert!(p.contains("export GREETING='it'\\''s'\n"));
        assert!(p.contains("export ISB_URL='http://10.0.0.1:8481'\n"));
        assert!(p.contains("export ISB_ORG='acme'\n"));
        assert!(p.contains("ISB_TOKEN=$(cat /run/isb/token)"));
    }

    #[test]
    fn the_setup_script_runs_once_per_build_and_again_on_request() {
        use SetupStatus::*;
        let next = |cur: Option<&SetupState>, ev| setup_next(true, cur, ev, 7).unwrap();
        // Built: pending; started: running, counted; finished: done.
        let p = next(None, SetupEvent::Built).unwrap();
        assert_eq!((p.status, p.runs), (Pending, 0));
        let r = next(Some(&p), SetupEvent::Started).unwrap();
        assert_eq!((r.status, r.runs), (Running, 1));
        let ok = next(Some(&r), SetupEvent::Finished(0)).unwrap();
        assert_eq!((ok.status, ok.exit_code), (Succeeded, Some(0)));
        let bad = next(Some(&r), SetupEvent::Finished(3)).unwrap();
        assert_eq!((bad.status, bad.exit_code), (Failed, Some(3)));
        let err = next(Some(&r), SetupEvent::Error("timed out".into())).unwrap();
        assert_eq!(
            (err.status, err.message.as_deref()),
            (Failed, Some("timed out"))
        );
        // Done stays done until a rebuild or a request; both keep the count.
        let again = next(Some(&ok), SetupEvent::Requested).unwrap();
        assert_eq!((again.status, again.runs), (Pending, 1));
        assert_eq!(next(Some(&bad), SetupEvent::Built).unwrap().status, Pending);
        // Not twice at once, and no start or finish out of turn.
        assert!(setup_next(true, Some(&r), SetupEvent::Requested, 7).is_err());
        assert!(setup_next(true, Some(&ok), SetupEvent::Started, 7).is_err());
        assert!(setup_next(true, Some(&p), SetupEvent::Finished(0), 7).is_err());
        // A daemon restart fails a run it had begun, and nothing else.
        let lost = next(Some(&r), SetupEvent::DaemonStarted).unwrap();
        assert_eq!(lost.status, Failed);
        assert!(lost.message.unwrap().contains("restarted"));
        assert_eq!(next(Some(&ok), SetupEvent::DaemonStarted), Some(ok.clone()));
        // No script: no state, and nothing to request.
        assert_eq!(
            setup_next(false, Some(&p), SetupEvent::Built, 7).unwrap(),
            None
        );
        assert!(setup_next(false, None, SetupEvent::Requested, 7).is_err());
        // Due only when pending with a script.
        let mut w: Workspace = serde_json::from_value(serde_json::json!({
            "name": "workspace", "id": "x", "image": "dev-base", "user": "dev",
            "home_size": "1GiB", "token_role": "admin", "created_at": 0, "created_by": "a",
            "setup": "apt-get install -y htop", "setup_state": {"status": "pending", "at": 1}
        }))
        .unwrap();
        assert!(setup_due(&w));
        w.setup_state = Some(ok);
        assert!(!setup_due(&w));
    }
}
