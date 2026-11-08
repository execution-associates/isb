//! Restarting a replica that stopped, and what a start is checked for.
//!
//! The controller is a replica's one supervisor: its `boot.autorestart` is
//! off ([`crate::plan::boot_config`]), so an exit is seen on the next pass
//! and started here, once. A replica that keeps exiting is started with a
//! growing delay, and `restart.failed` says so once per crash loop.
//! After a start, an OCI replica's `/etc/resolv.conf` must name a
//! nameserver, or `dns.failed` says that its DNS is broken.

use std::time::{Duration, Instant};

use serde_json::Value;

use super::Worker;
use crate::client::encode_segment;

/// A run shorter than this, from a start isb made to the exit, extends a
/// crash loop; a longer one ends it. Twice [`BACKOFF_MAX`], so a replica
/// at the longest delay that then stays up a while starts over.
pub(super) const STABLE_RUN: Duration = Duration::from_secs(600);
/// Short runs in a row that are restarted at once, before any backoff.
pub(super) const FREE_RESTARTS: u32 = 3;
/// The first delay past them, doubling up to [`BACKOFF_MAX`].
const BACKOFF_MIN: Duration = Duration::from_secs(10);
const BACKOFF_MAX: Duration = Duration::from_secs(300);
/// How long after a start resolv.conf is read: without pinned DNS, incus
/// writes the lease's nameserver about two seconds after the start.
const DNS_SETTLE: Duration = Duration::from_secs(10);

/// How long a stopped replica waits before its next start, after `streak`
/// short runs in a row.
pub(super) fn backoff(streak: u32) -> Duration {
    let Some(over) = streak.checked_sub(FREE_RESTARTS) else {
        return Duration::ZERO;
    };
    let factor = 1u32.checked_shl(over.min(16)).unwrap_or(u32::MAX);
    BACKOFF_MIN.saturating_mul(factor).min(BACKOFF_MAX)
}

/// The streak after an exit: `run` is how long the replica ran since isb
/// last started it (`None`: isb did not start it, so nothing is known).
pub(super) fn next_streak(streak: u32, run: Option<Duration>) -> u32 {
    match run {
        Some(r) if r < STABLE_RUN => streak.saturating_add(1),
        _ => 0,
    }
}

/// Whether resolv.conf's text names a nameserver.
pub(super) fn names_nameserver(resolv: &str) -> bool {
    resolv
        .lines()
        .any(|l| l.split_whitespace().next() == Some("nameserver"))
}

impl Worker {
    pub(super) fn count_restart(&mut self, name: &str) {
        self.rt
            .entry(name.to_string())
            .or_default()
            .restarts
            .push_back(Instant::now());
    }

    pub(super) fn restart_budget_spent(
        &mut self,
        name: &str,
        policy: Option<&crate::spec::RestartPolicy>,
    ) -> bool {
        let Some(p) = policy else { return false };
        let Some(max) = p.max_attempts else {
            return false;
        };
        let window = p
            .window
            .as_deref()
            .and_then(|w| crate::flex::parse_duration(w).ok());
        let rt = self.rt.entry(name.to_string()).or_default();
        if let Some(w) = window {
            while rt.restarts.front().is_some_and(|t| t.elapsed() > w) {
                rt.restarts.pop_front();
            }
        }
        rt.restarts.len() as u32 >= max
    }

    /// Whether a stopped replica's next start must wait. Its exit is
    /// counted once, when first seen: a short run extends its crash loop
    /// ([`next_streak`]), and the start waits out [`backoff`] from then.
    /// Raises `restart.failed` as the loop begins.
    pub(super) fn crash_backoff(&mut self, name: &str) -> bool {
        let now = Instant::now();
        let rt = self.rt.entry(name.to_string()).or_default();
        let stopped = *rt.stopped_at.get_or_insert_with(|| {
            let run = rt.last_start.map(|t| now.duration_since(t));
            rt.crash_streak = next_streak(rt.crash_streak, run);
            now
        });
        if rt.crash_streak == 0 {
            rt.crash_loop = false;
        }
        let streak = rt.crash_streak;
        let left = backoff(streak).saturating_sub(now.duration_since(stopped));
        if left.is_zero() {
            // Starting it now: its next run is measured from here.
            rt.stopped_at = None;
            rt.last_start = Some(now);
        }
        let first = streak >= FREE_RESTARTS && !rt.crash_loop;
        if first {
            rt.crash_loop = true;
            let msg = format!(
                "{name} keeps exiting ({streak} times in a row within {} minutes of a start); \
                 starting it again with a backoff of up to {} minutes",
                STABLE_RUN.as_secs() / 60,
                BACKOFF_MAX.as_secs() / 60
            );
            eprintln!("isb serve: {}/{}: {msg}", self.q, self.service);
            self.inner.emit_kind(
                Some("restart.failed"),
                "error",
                &self.q,
                &self.service,
                Some(name),
                msg,
            );
        }
        if left.is_zero() {
            return false;
        }
        self.message = Some(format!(
            "{name}: crash loop, next start in {}s",
            left.as_secs().max(1)
        ));
        true
    }

    /// Once per start, when it has settled: an OCI replica with a NIC whose
    /// `/etc/resolv.conf` names no nameserver raises `dns.failed`. A read
    /// that fails is tried again on the next pass.
    pub(super) fn check_dns(&mut self, name: &str, oci: bool) {
        let Some(rt) = self.rt.get_mut(name) else {
            return;
        };
        if !oci || rt.dns_checked || rt.since.is_none_or(|s| s.elapsed() < DNS_SETTLE) {
            return;
        }
        let resolv = match self.oclient.read_file(name, "/etc/resolv.conf") {
            Ok(b) => String::from_utf8_lossy(&b.unwrap_or_default()).into_owned(),
            Err(e) => {
                self.log(&format!("cannot read {name}'s /etc/resolv.conf: {e}"));
                return;
            }
        };
        if let Some(rt) = self.rt.get_mut(name) {
            rt.dns_checked = true;
        }
        // Without a NIC there is no DHCP, so nothing to resolve with
        // (isb-system's registry): not broken.
        if names_nameserver(&resolv)
            || !self
                .oclient
                .get(&format!("/1.0/instances/{}", encode_segment(name)))
                .is_ok_and(|v| has_nic(&v))
        {
            return;
        }
        let msg = format!(
            "{name}'s /etc/resolv.conf names no nameserver ({:?}): its DNS lookups fail until \
             it is restarted",
            resolv.trim()
        );
        eprintln!("isb serve: {}/{}: {msg}", self.q, self.service);
        self.inner.emit_kind(
            Some("dns.failed"),
            "error",
            &self.q,
            &self.service,
            Some(name),
            msg,
        );
    }
}

/// Whether an instance (`GET /1.0/instances/<name>`) has a NIC.
fn has_nic(inst: &Value) -> bool {
    inst["expanded_devices"]
        .as_object()
        .is_some_and(|d| d.values().any(|d| d["type"] == "nic"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_crash_loop_backs_off_to_five_minutes() {
        assert_eq!(backoff(0), Duration::ZERO);
        assert_eq!(backoff(FREE_RESTARTS - 1), Duration::ZERO);
        assert_eq!(backoff(FREE_RESTARTS), Duration::from_secs(10));
        assert_eq!(backoff(FREE_RESTARTS + 1), Duration::from_secs(20));
        assert_eq!(backoff(FREE_RESTARTS + 4), Duration::from_secs(160));
        assert_eq!(backoff(FREE_RESTARTS + 5), BACKOFF_MAX);
        assert_eq!(backoff(10_000), BACKOFF_MAX);
    }

    #[test]
    fn a_long_run_ends_the_streak() {
        assert_eq!(next_streak(0, Some(Duration::from_secs(1))), 1);
        assert_eq!(next_streak(7, Some(Duration::from_secs(1))), 8);
        assert_eq!(next_streak(7, Some(STABLE_RUN)), 0);
        // Not started by isb (found running when the daemon started).
        assert_eq!(next_streak(7, None), 0);
    }

    #[test]
    fn a_crash_loop_stays_at_the_longest_delay() {
        // 2026-11-05: 488 restarts in 79 minutes. With the backoff, the
        // starts in an hour of a replica that exits a second after each:
        let mut streak = 0;
        let mut t = Duration::ZERO;
        let mut last_start = None;
        let mut starts = Vec::new();
        while t < Duration::from_secs(3600) {
            let exit = t + Duration::from_secs(1);
            streak = next_streak(streak, last_start.map(|s: Duration| exit - s));
            t = exit + backoff(streak);
            last_start = Some(t);
            starts.push(t);
        }
        assert!(starts.len() < 25, "{} starts", starts.len());
        let gaps: Vec<Duration> = starts.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(gaps[8..].iter().all(|g| *g > BACKOFF_MAX), "{gaps:?}");
    }

    #[test]
    fn resolv_conf_needs_a_nameserver_line() {
        assert!(names_nameserver(
            "nameserver 10.108.36.1\ndomain ocai.isb\noptions edns0\n"
        ));
        assert!(names_nameserver("  nameserver\t10.0.0.1"));
        assert!(!names_nameserver("options edns0\n"));
        assert!(!names_nameserver("# nameserver 1.1.1.1\n"));
        assert!(!names_nameserver(""));
    }

    #[test]
    fn a_nic_is_any_expanded_nic_device() {
        let v = serde_json::json!({"expanded_devices": {
            "root": {"type": "disk"}, "eth0": {"type": "nic", "network": "isbbr1"}}});
        assert!(has_nic(&v));
        let v = serde_json::json!({"expanded_devices": {"root": {"type": "disk"}}});
        assert!(!has_nic(&v));
    }
}
