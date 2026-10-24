//! A sandbox's expiry (docs/concepts/sandboxes.md#expiry): the deadline isb
//! keeps on the instance as `user.isb.expires_at`, after which `isb serve`'s
//! reaper deletes it. Set when isb creates the sandbox and renewed by each
//! later `isb up`, so it is a lease: a sandbox nobody brings up again goes
//! away on its own.

use std::borrow::Cow;
use std::time::Duration;

use serde_json::json;

use super::{Reporter, update_instance};
use crate::client::Client;
use crate::error::{Error, Result};
use crate::plan::{Action, Desired, SandboxPlan};

/// The instance config key holding the deadline: unix seconds, or `never`.
pub const KEY_EXPIRES_AT: &str = "user.isb.expires_at";

/// What a plain sandbox (outside every org) lives without an `expires`.
pub const DEFAULT_PLAIN_EXPIRY: Duration = Duration::from_secs(7 * 86400);

/// What an org's sandbox lives without an `expires`, unless the org's
/// admins changed its `sandbox_expiry`.
pub const DEFAULT_ORG_EXPIRY: Duration = Duration::from_secs(86400);

/// A renewal that moves the deadline by less than this is not written, so
/// `isb up` in a loop does not rewrite the instance each time.
const RENEW_SLACK: u64 = 60;

/// A spec's `expires`: a lifetime from now, or never.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expires {
    Never,
    After(Duration),
}

impl Expires {
    /// `24h`, `7d`, a number of seconds, or `never` (`none`, `off`).
    pub fn parse(s: &str) -> Result<Expires> {
        let s = s.trim();
        if matches!(s, "never" | "none" | "off") {
            return Ok(Expires::Never);
        }
        let d = crate::flex::parse_duration(s)
            .map_err(|e| Error::invalid(format!("expires {s:?}: {e}")))?;
        if d.is_zero() {
            return Err(Error::invalid(format!(
                "expires {s:?}: a positive duration, or never"
            )));
        }
        Ok(Expires::After(d))
    }
}

/// The lifetime a new sandbox in incus project `project` gets when its spec
/// names none: in an org's project, the org's `sandbox_expiry`
/// (`org_expiry`, else [`DEFAULT_ORG_EXPIRY`]); elsewhere,
/// [`DEFAULT_PLAIN_EXPIRY`].
pub fn default_for(
    project: &str,
    org_expiry: impl FnOnce(&crate::org::OrgId) -> Option<Duration>,
) -> Duration {
    match crate::org::OrgId::from_incus_project(project) {
        Some(org) => org_expiry(&org).unwrap_or(DEFAULT_ORG_EXPIRY),
        None => DEFAULT_PLAIN_EXPIRY,
    }
}

/// A stack replica and a build never expire: their controller owns them.
fn exempt(d: &Desired) -> bool {
    d.config.contains_key("user.isb.stack") || d.config.contains_key("user.isb.build")
}

/// The deadline a sandbox created now gets: unix seconds, `never`, or none
/// (no `expires` and no default: the instance carries no key).
fn at_creation(d: &Desired, default: Option<Duration>, now: u64) -> Option<String> {
    if exempt(d) {
        return None;
    }
    match d.expires {
        Some(Expires::Never) => Some("never".into()),
        Some(Expires::After(life)) => Some((now + life.as_secs()).to_string()),
        None => default.map(|life| (now + life.as_secs()).to_string()),
    }
}

/// What a later `up` does to an existing deadline (`current`, as stored):
/// `Some(new value)`, or `None` to leave it.
///
/// - `expires: never` makes it never.
/// - `expires: D` makes it now + D, unless that is not later.
/// - no `expires` renews a deadline the instance has by the default, and
///   never adds one: an instance without a deadline (made before isb had
///   them, or set to never) keeps none.
pub fn renewal(
    current: Option<&str>,
    expires: Option<Expires>,
    default: Option<Duration>,
    now: u64,
) -> Option<String> {
    let at: Option<u64> = current.and_then(|c| c.trim().parse().ok());
    let life = match expires {
        Some(Expires::Never) => {
            return (current.map(str::trim) != Some("never")).then(|| "never".into());
        }
        Some(Expires::After(life)) => life,
        None => match (at, default) {
            (Some(_), Some(life)) => life,
            _ => return None,
        },
    };
    let want = now + life.as_secs();
    match at {
        Some(at) if want < at + RENEW_SLACK => None,
        _ => Some(want.to_string()),
    }
}

/// `in 6d 23h`, `never`, or `5m ago`, for a stored deadline.
pub fn describe(v: &str, now: u64) -> String {
    match v.trim().parse::<u64>() {
        Ok(at) if at > now => format!("in {}", human(at - now)),
        Ok(at) => format!("{} ago", human(now - at)),
        Err(_) if v.trim() == "never" => "never".into(),
        Err(_) => "-".into(),
    }
}

/// `desired` with its deadline in the create config, when `plan` creates
/// the instance.
pub fn stamped<'a>(
    desired: &'a Desired,
    plan: &SandboxPlan,
    default: Option<Duration>,
) -> Cow<'a, Desired> {
    let creates = plan
        .actions
        .iter()
        .any(|a| matches!(a, Action::CreateInstance { .. }));
    match at_creation(desired, default, crate::stack::now_secs()).filter(|_| creates) {
        Some(v) => {
            let mut d = desired.clone();
            d.config.insert(KEY_EXPIRES_AT.into(), v);
            Cow::Owned(d)
        }
        None => Cow::Borrowed(desired),
    }
}

/// Say when a sandbox just created from `d` expires, if it does.
pub fn report_created(d: &Desired, report: Reporter<'_>) {
    if let Some(v) = d.config.get(KEY_EXPIRES_AT) {
        let when = describe(v, crate::stack::now_secs());
        report(&format!("{}: expires {when}", d.name));
    }
}

/// Renew an existing sandbox's deadline (see [`renewal`]).
pub(super) fn renew(
    client: &Client,
    desired: &Desired,
    default: Option<Duration>,
    report: Reporter<'_>,
) -> Result<()> {
    if exempt(desired) || (desired.expires.is_none() && default.is_none()) {
        return Ok(());
    }
    let now = crate::stack::now_secs();
    let inst = client.get(&super::inst_path(&desired.name))?;
    let current = inst["config"][KEY_EXPIRES_AT].as_str();
    let Some(v) = renewal(current, desired.expires, default, now) else {
        return Ok(());
    };
    update_instance(
        client,
        &desired.name,
        &format!("renew expiry of {}", desired.name),
        &mut |config, _| {
            config.insert(KEY_EXPIRES_AT.into(), json!(v));
            Ok(())
        },
    )?;
    report(&format!("{}: expires {}", desired.name, describe(&v, now)));
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86400;

    #[test]
    fn parses_durations_and_never() {
        assert_eq!(
            Expires::parse("24h").unwrap(),
            Expires::After(Duration::from_secs(DAY))
        );
        assert_eq!(Expires::parse(" never ").unwrap(), Expires::Never);
        assert_eq!(Expires::parse("none").unwrap(), Expires::Never);
        assert!(Expires::parse("0s").is_err());
        assert!(Expires::parse("soon").is_err());
    }

    #[test]
    fn defaults_by_project() {
        assert_eq!(default_for("default", |_| None), DEFAULT_PLAIN_EXPIRY);
        assert_eq!(default_for("titan-norm-ct", |_| None), DEFAULT_PLAIN_EXPIRY);
        assert_eq!(default_for("isb-norm", |_| None), DEFAULT_ORG_EXPIRY);
        let three = Duration::from_secs(3 * DAY);
        assert_eq!(
            default_for("isb-norm", |o| {
                assert_eq!(o.as_str(), "norm");
                Some(three)
            }),
            three
        );
    }

    #[test]
    fn an_explicit_expires_sets_never_shortens() {
        let d = Some(Expires::After(Duration::from_secs(DAY)));
        // Added where there was none.
        assert_eq!(renewal(None, d, None, 1000), Some((1000 + DAY).to_string()));
        assert_eq!(renewal(Some("never"), d, None, 0), Some(DAY.to_string()));
        // Renewed from now.
        assert_eq!(
            renewal(Some("500"), d, None, 1000),
            Some((1000 + DAY).to_string())
        );
        // A later deadline (from `isb expiry --in 30d`) stays.
        assert_eq!(renewal(Some(&(40 * DAY).to_string()), d, None, 0), None);
        // A renewal of under a minute is not written.
        assert_eq!(renewal(Some(&(DAY - 30).to_string()), d, None, 0), None);
    }

    #[test]
    fn never_clears_and_the_default_only_renews() {
        assert_eq!(
            renewal(Some("123"), Some(Expires::Never), None, 0),
            Some("never".into())
        );
        assert_eq!(renewal(Some("never"), Some(Expires::Never), None, 0), None);
        let week = Some(DEFAULT_PLAIN_EXPIRY);
        // An instance without a deadline keeps none: the default is for
        // new sandboxes only.
        assert_eq!(renewal(None, None, week, 1000), None);
        assert_eq!(renewal(Some("never"), None, week, 1000), None);
        assert_eq!(
            renewal(Some("2000"), None, week, 1000),
            Some((1000 + 7 * DAY).to_string())
        );
        // No default (a library caller, the stack controller): untouched.
        assert_eq!(renewal(Some("2000"), None, None, 1000), None);
    }

    #[test]
    fn describes_deadlines() {
        assert_eq!(describe("7200", 0), "in 2h");
        assert_eq!(describe("100", 400), "5m ago");
        assert_eq!(describe("never", 0), "never");
        assert_eq!(describe("soon", 0), "-");
    }
}
