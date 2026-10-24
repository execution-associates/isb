//! The names a sandbox's connections asked for, as the proxy saw them:
//! each `host:port` once, allowed or denied. `isb x` reads it to say which
//! host a failed command was refused, and to turn a `@public` run into an
//! allow list. It lives beside the CAs (`<state>/egress/names/<network>`),
//! where the daemon writes and the CLI of the same user reads.

use std::io::Write;
use std::path::PathBuf;

/// Most names kept per sandbox; past that the file stops growing.
pub const MAX_NAMES: usize = 1024;

/// One name a connection asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub allowed: bool,
    pub host: String,
    pub port: u16,
}

fn path(network: &str) -> PathBuf {
    super::ca::root().join("names").join(network)
}

fn applied_path(network: &str) -> PathBuf {
    super::ca::root()
        .join("names")
        .join(format!("{network}.applied"))
}

/// The proxy took up `policy`: say so, for a caller that just changed the
/// list and must not start a command before the proxy knows it.
pub fn mark_applied(network: &str, policy: &super::Policy) {
    let p = applied_path(network);
    if let Some(d) = p.parent() {
        let _ = super::ca::make_private_dir(d);
    }
    if let Ok(json) = serde_json::to_string(policy) {
        let _ = crate::secrets::local::write_atomic(&p, json.as_bytes());
    }
}

/// Wait, at most `within`, until the proxy of `network` says it applies
/// `policy`. False when it did not in time (an older `isb serve`, or none).
pub fn wait_applied(network: &str, policy: &super::Policy, within: std::time::Duration) -> bool {
    let Ok(want) = serde_json::to_string(policy) else {
        return false;
    };
    let until = std::time::Instant::now() + within;
    loop {
        if std::fs::read_to_string(applied_path(network)).is_ok_and(|got| got == want) {
            return true;
        }
        if std::time::Instant::now() >= until {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Append one line. Best effort: a full disk must not break a connection.
pub fn record(network: &str, allowed: bool, host: &str, port: u16) {
    let p = path(network);
    if let Some(d) = p.parent() {
        let _ = super::ca::make_private_dir(d);
    }
    let word = if allowed { "allowed" } else { "denied" };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&p)
    {
        let _ = writeln!(f, "{word} {host}:{port}");
    }
}

/// Everything recorded for `network`, oldest first; nothing when no proxy
/// has seen a connection from it.
pub fn read(network: &str) -> Vec<Seen> {
    std::fs::read_to_string(path(network))
        .unwrap_or_default()
        .lines()
        .filter_map(parse)
        .collect()
}

fn parse(line: &str) -> Option<Seen> {
    let (word, target) = line.split_once(' ')?;
    let (host, port) = target.rsplit_once(':')?;
    Some(Seen {
        allowed: match word {
            "allowed" => true,
            "denied" => false,
            _ => return None,
        },
        host: host.to_string(),
        port: port.parse().ok()?,
    })
}

/// Start over, as before a run whose refusals are to be reported.
pub fn clear(network: &str) {
    let _ = std::fs::remove_file(path(network));
}

pub(crate) fn forget(network: &str) {
    clear(network);
    let _ = std::fs::remove_file(applied_path(network));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_round_trip_and_junk_is_skipped() {
        assert_eq!(
            parse("denied pypi.org:443"),
            Some(Seen {
                allowed: false,
                host: "pypi.org".into(),
                port: 443
            })
        );
        assert_eq!(parse("allowed -:22").map(|s| s.port), Some(22));
        assert_eq!(parse("maybe x:1"), None);
        assert_eq!(parse("denied nope"), None);
    }
}
