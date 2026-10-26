//! `isb expiry`, and the expiry a sandbox the CLI makes gets by default
//! (docs/concepts/sandboxes.md#expiry).

use isb::sandbox::expiry::{self as exp, KEY_EXPIRES_AT};

use super::*;

#[derive(Args)]
pub(crate) struct ExpiryArgs {
    /// Sandboxes (instance names).
    #[arg(required = true)]
    names: Vec<String>,
    /// Expire this long from now, e.g. 24h.
    #[arg(long = "in", value_name = "DURATION", conflicts_with_all = ["by", "never"])]
    in_: Option<String>,
    /// Push the expiry out by this long, from the later of now and the
    /// current expiry, e.g. 24h.
    #[arg(long, value_name = "DURATION", conflicts_with = "never")]
    by: Option<String>,
    /// Never expire.
    #[arg(long)]
    never: bool,
}

/// `isb serve`'s state directory on this host, as it picks it.
pub(crate) fn serve_state_dir() -> PathBuf {
    std::env::var_os("ISB_SERVE_STATE_DIR")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(isb::daemon::default_state_dir)
}

/// An org's `sandbox_expiry`, read from `isb serve`'s state on this host.
pub(crate) fn org_expiry(org: &isb::org::OrgId) -> Option<Duration> {
    let s = isb::sandbox_life::settings(&serve_state_dir(), org).ok()?;
    isb::sandbox_life::lifetime(&s.sandbox_expiry).ok()
}

/// The lifetime of a new sandbox in `c`'s project whose spec gives none.
pub(crate) fn default_expiry(c: &Client) -> Option<Duration> {
    Some(exp::default_for(c.project_name(), org_expiry))
}

fn now() -> u64 {
    isb::stack::now_secs()
}

/// The stored deadline after this command, or `None` to only show it.
fn wanted(a: &ExpiryArgs, current: Option<&str>) -> Result<Option<String>> {
    let life = |s: &str| match exp::Expires::parse(s)? {
        exp::Expires::After(d) => Ok(d.as_secs()),
        exp::Expires::Never => Err(Error::Invalid(format!("{s:?}: a duration; --never clears"))),
    };
    if a.never {
        return Ok(Some("never".into()));
    }
    if let Some(s) = &a.in_ {
        return Ok(Some((now() + life(s)?).to_string()));
    }
    if let Some(s) = &a.by {
        let at = current.and_then(|c| c.trim().parse::<u64>().ok());
        let from = at.unwrap_or(0).max(now());
        return Ok(Some((from + life(s)?).to_string()));
    }
    Ok(None)
}

pub(crate) fn expiry(ctx: &Ctx, a: ExpiryArgs) -> Result<u8> {
    let c = ctx.client(None);
    for name in &a.names {
        let info = Sandbox::get(&c, name)?.info()?;
        if ["isb.stack", "isb.build"]
            .iter()
            .any(|k| info.labels.contains_key(*k))
        {
            return Err(Error::Invalid(format!(
                "{name} is a stack replica or a build: its controller removes it, it does not expire"
            )));
        }
        let current = info.config.get(KEY_EXPIRES_AT).map(String::as_str);
        let shown = match wanted(&a, current)? {
            Some(v) => {
                sandbox::update_instance(
                    &c,
                    name,
                    &format!("set expiry of {name}"),
                    &mut |cfg, _| {
                        cfg.insert(KEY_EXPIRES_AT.into(), serde_json::json!(v));
                        Ok(())
                    },
                )?;
                v
            }
            None => current.unwrap_or("-").to_string(),
        };
        println!("{name}: expires {}", exp::describe(&shown, now()));
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(in_: Option<&str>, by: Option<&str>, never: bool) -> ExpiryArgs {
        ExpiryArgs {
            names: vec!["x".into()],
            in_: in_.map(String::from),
            by: by.map(String::from),
            never,
        }
    }

    #[test]
    fn wanted_deadlines() {
        let t = now();
        assert_eq!(wanted(&args(None, None, false), Some("5")).unwrap(), None);
        assert_eq!(
            wanted(&args(None, None, true), Some("5"))
                .unwrap()
                .as_deref(),
            Some("never")
        );
        let at = |v: Option<String>| v.unwrap().parse::<u64>().unwrap();
        let in_day = at(wanted(&args(Some("1d"), None, false), None).unwrap());
        assert!((t + 86400..t + 86410).contains(&in_day));
        // --by counts from the later of now and the current deadline.
        let later = (t + 10 * 86400).to_string();
        let by = at(wanted(&args(None, Some("1d"), false), Some(&later)).unwrap());
        assert_eq!(by, t + 11 * 86400);
        let by = at(wanted(&args(None, Some("1h"), false), Some("never")).unwrap());
        assert!((t + 3600..t + 3610).contains(&by));
        assert!(wanted(&args(Some("never"), None, false), None).is_err());
    }
}
