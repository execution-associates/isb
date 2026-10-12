//! How long sandboxes live (docs/concepts/sandboxes.md#sandboxes-are-short-lived):
//! the org's defaults for a new sandbox's expiry and idle timeout, and the
//! pure decisions the daemon makes from them (a sandbox's deadlines, an
//! extension, whether the reaper takes it). The daemon's side, the reaper
//! and the activity it reads, is `daemon::sandboxes`.
//!
//! On disk: `sandboxes.json` in the org's state directory (`<state>/` for
//! the default org, `<state>/orgs/<org>/` for the others), 0600.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::org::OrgId;

/// Instance config keys (`user.*`) isb keeps on sandboxes.
pub const KEY_EXPIRES_AT: &str = "user.isb.expires_at";
pub const KEY_IDLE_TIMEOUT: &str = "user.isb.idle_timeout";

/// The longest a sandbox may live from now, by default or extended.
pub const MAX_SANDBOX_LIFETIME: Duration = Duration::from_secs(30 * 86400);

/// An org's sandbox defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// A new sandbox's lifetime (`24h`); at most 30 days.
    #[serde(default = "default_expiry")]
    pub sandbox_expiry: String,
    /// How long a sandbox may sit idle before it is reaped (`2h`), or
    /// `none`.
    #[serde(default = "default_idle")]
    pub sandbox_idle: String,
}

fn default_expiry() -> String {
    "24h".into()
}

fn default_idle() -> String {
    "2h".into()
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            sandbox_expiry: default_expiry(),
            sandbox_idle: default_idle(),
        }
    }
}

impl Settings {
    /// Check the values a caller set.
    pub fn validate(&self) -> Result<()> {
        lifetime(&self.sandbox_expiry)?;
        idle(&self.sandbox_idle)?;
        Ok(())
    }
}

fn path(state: &Path, org: &OrgId) -> PathBuf {
    crate::app::org_root(state, org).join("sandboxes.json")
}

/// Where isb 2 kept these defaults, with the org's workspace settings.
/// Read once, when `sandboxes.json` does not exist yet, so an org's
/// overrides carry over.
fn legacy_path(state: &Path, org: &OrgId) -> PathBuf {
    crate::app::org_root(state, org)
        .join("workspaces")
        .join("settings.json")
}

/// The org's sandbox defaults.
pub fn settings(state: &Path, org: &OrgId) -> Result<Settings> {
    let parse = |b: &[u8]| {
        serde_json::from_slice::<Settings>(b)
            .map_err(|e| Error::invalid(format!("sandbox settings of {org}: {e}")))
    };
    match std::fs::read(path(state, org)) {
        Ok(b) => return parse(&b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    match std::fs::read(legacy_path(state, org)) {
        Ok(b) => {
            let s = parse(&b)?;
            if let Err(e) = put_settings(state, org, &s) {
                eprintln!("isb serve: org {org}: sandbox settings not carried over: {e}");
            }
            Ok(s)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(e) => Err(e.into()),
    }
}

/// Write the org's sandbox defaults.
pub fn put_settings(state: &Path, org: &OrgId, s: &Settings) -> Result<()> {
    s.validate()?;
    crate::app::write_atomic(&path(state, org), &serde_json::to_vec_pretty(s)?)
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
    if labels.contains_key("isb.stack") {
        "replica"
    } else if labels.contains_key("isb.build") {
        "build"
    } else {
        "sandbox"
    }
}

/// Why the reaper takes this instance now, if it does. Only sandboxes isb
/// gave deadlines (`isb.expires_at`, `isb.idle_timeout`) are candidates:
/// never a stack replica or an instance isb did not make.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(kv: &[(&str, &str)]) -> BTreeMap<String, String> {
        kv.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn human_durations() {
        assert_eq!(human(0), "0s");
        assert_eq!(human(59), "59s");
        assert_eq!(human(3600 + 120 + 5), "1h 2m");
        assert_eq!(human(90061), "1d 1h");
    }

    #[test]
    fn settings_default_and_bounds() {
        let s = Settings::default();
        assert_eq!(s.sandbox_expiry, "24h");
        assert_eq!(s.sandbox_idle, "2h");
        s.validate().unwrap();
        let mut b = s.clone();
        b.sandbox_expiry = "31d".into();
        assert!(b.validate().is_err());
        b.sandbox_expiry = "7d".into();
        b.sandbox_idle = "none".into();
        b.validate().unwrap();
        let empty: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, Settings::default());
    }

    #[test]
    fn settings_round_trip_and_carry_over_from_the_workspace_settings() {
        let d = tempfile::tempdir().unwrap();
        let org = OrgId::new("acme").unwrap();
        assert_eq!(settings(d.path(), &org).unwrap(), Settings::default());
        // An isb 2 org's overrides, beside fields that are gone.
        let old = legacy_path(d.path(), &org);
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::write(
            &old,
            r#"{"max_workspaces": 1, "sandbox_expiry": "7d", "sandbox_idle": "none", "secret_refresh": "1h"}"#,
        )
        .unwrap();
        let s = settings(d.path(), &org).unwrap();
        assert_eq!(
            (s.sandbox_expiry.as_str(), s.sandbox_idle.as_str()),
            ("7d", "none")
        );
        assert!(path(d.path(), &org).is_file());
        // Once carried over, sandboxes.json is what counts.
        let mut n = s.clone();
        n.sandbox_idle = "4h".into();
        put_settings(d.path(), &org, &n).unwrap();
        assert_eq!(settings(d.path(), &org).unwrap(), n);
        n.sandbox_expiry = "40d".into();
        assert!(put_settings(d.path(), &org, &n).is_err());
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
        // Never a replica, a build or an instance without deadlines,
        // whatever its labels say.
        for extra in ["isb.stack", "isb.build"] {
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
}
