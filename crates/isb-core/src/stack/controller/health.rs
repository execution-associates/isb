//! What a worker remembers of each replica, and how a probe result moves
//! its health: starting (never passed: out of rotation, not restarted
//! through its startup grace), healthy, unhealthy (its app is restarted).

use std::collections::{BTreeMap, VecDeque};
use std::net::IpAddr;
use std::time::{Duration, Instant};

use crate::spec::HealthProbe;

/// Per-instance memory of a worker.
#[derive(Debug, Default)]
pub(super) struct InstRt {
    /// The init pid secrets and the unit were last set up for.
    pub(super) pid: i64,
    /// When that pid was first seen: the start of `start_period`.
    pub(super) since: Option<Instant>,
    pub(super) ip: Option<IpAddr>,
    pub(super) failures: u32,
    /// `None` while starting: not yet passed since `since`, nor failed
    /// `retries` times after its startup grace.
    pub(super) healthy: Option<bool>,
    /// Passed its healthcheck since `since`: failures then count as soon
    /// as `start_period` is over, not the startup grace.
    pub(super) passed: bool,
    pub(super) next_probe: Option<Instant>,
    pub(super) last_probe: String,
    /// App restarts for failing health, since it was last healthy.
    pub(super) unhealthy_restarts: u32,
    /// Restarts counted against `restart_policy.max_attempts`.
    pub(super) restarts: VecDeque<Instant>,
    pub(super) in_rotation: bool,
    /// The `none` secret versions whose files were last delivered live.
    pub(super) delivered: BTreeMap<String, u64>,
}

impl InstRt {
    /// The app (re)started: probing, and its startup grace, start over.
    pub(super) fn started(&mut self, now: Instant) {
        self.since = Some(now);
        self.failures = 0;
        self.healthy = None;
        self.passed = false;
    }

    /// Take one probe result. A pass makes it healthy; a failure that
    /// counts ([`HealthProbe::failure_counts`]) brings it nearer to
    /// unhealthy, which restarts its app. A replica that has never passed
    /// stays starting (out of rotation, not restarted) through its grace.
    pub(super) fn record_probe(&mut self, ok: bool, p: &HealthProbe, now: Instant) {
        if ok {
            self.failures = 0;
            self.healthy = Some(true);
            self.passed = true;
            self.unhealthy_restarts = 0;
            return;
        }
        let since = self.since.map_or(Duration::ZERO, |s| now.duration_since(s));
        if p.failure_counts(self.passed, since) {
            self.failures += 1;
            if self.failures >= p.retries {
                self.healthy = Some(false);
            }
        }
    }
}
