//! A monitor's state machine: failure and recovery thresholds (hysteresis),
//! one notification per change (de-duplication) and flap damping. No I/O,
//! so every rule is tested on its own.
//!
//! - `failure_threshold` failed checks in a row take a monitor down;
//!   `recovery_threshold` successful ones bring it back up. A new monitor's
//!   first success makes it up at once.
//! - A new monitor is `pending` until its first success. Failures before
//!   that are *pending* checks, not downtime: no threshold, no incident, no
//!   `down`. If it has only failed for [`NEVER_UP_MS`], channels hear once
//!   that it *never came up* (a `down`), and the monitor stays pending
//!   (flagged `never_up`) until a check succeeds, which then sends an `up`.
//! - Channels hear `down` once per incident and `up` once after it, always
//!   alternating: an up is only sent after a down was.
//! - A monitor that went down [`FLAP_DOWNS`] times within [`FLAP_WINDOW_MS`]
//!   is *flapping*: the change that makes it so is still sent (saying so),
//!   then nothing more until it has held one state for [`FLAP_WINDOW_MS`];
//!   then the state it settled in is sent if channels last heard otherwise.
//! - An app or stack service scaled to 0 (stopped) is not checked: its
//!   monitor is *stopped*, quietly. Stopping closes any open incident
//!   without telling channels, and starting again begins as a new monitor
//!   does, pending until its first success.

use serde::{Deserialize, Serialize};

/// How long a monitor must hold one state to stop flapping, and the window
/// downs are counted in.
pub const FLAP_WINDOW_MS: u64 = 30 * 60 * 1000;
/// Downs within the window that make a monitor flapping.
pub const FLAP_DOWNS: usize = 3;
/// How long a new monitor may only fail before it is called "never came up".
pub const NEVER_UP_MS: u64 = 30 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Not checked yet (or channels were never told anything).
    #[default]
    Pending,
    Up,
    Down,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::Up => "up",
            Status::Down => "down",
        }
    }
}

/// How many checks in a row change the status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Thresholds {
    pub failures: u32,
    pub recoveries: u32,
}

/// A monitor's state, kept across daemon restarts so a restart never sends
/// a down (or an up) twice.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub status: Status,
    /// Unix milliseconds the status began.
    pub since: u64,
    /// Failed checks in a row.
    pub fails: u32,
    /// Successful checks in a row.
    pub oks: u32,
    /// When the current run of failures began.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failing_since: Option<u64>,
    /// What channels were last told.
    pub notified: Status,
    /// When the incident channels were told about began.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub down_since: Option<u64>,
    /// Unix milliseconds of recent downs, for flap damping.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub downs: Vec<u64>,
    pub flapping: bool,
    /// The certificate (by its expiry, unix seconds) already warned about.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cert_warned: Option<u64>,
    /// When the first check of a pending monitor ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_check: Option<u64>,
    /// Pending for [`NEVER_UP_MS`] with only failures.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub never_up: bool,
    /// Its app or stack service is scaled to 0, since [`State::since`].
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub stopped: bool,
}

/// What a check changed, and what to tell channels.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    /// The new status, when it changed.
    pub changed: Option<Status>,
    pub notify: Notify,
    /// The check failed while the monitor was waiting for its first
    /// success: kept as pending, not as a failure.
    pub pending: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Notify {
    None,
    /// Down since `since` (the first failed check of the incident).
    Down {
        since: u64,
        flapping: bool,
    },
    /// Still no success `NEVER_UP_MS` after the first check (`since`).
    NeverUp {
        since: u64,
    },
    /// Up again; it was down from `down_since`.
    Up {
        down_since: u64,
        downtime_ms: u64,
    },
}

impl State {
    /// Take one check's result at `now` (unix milliseconds).
    pub fn observe(&mut self, ok: bool, now: u64, t: Thresholds) -> Step {
        // Checked again: its target runs.
        self.stopped = false;
        if self.status == Status::Pending && !ok {
            return self.observe_pending(now);
        }
        let changed = self.count(ok, now, t);
        if let Some(s) = changed {
            self.status = s;
            self.since = now;
            if s == Status::Down {
                self.downs.push(now);
            }
        }
        self.downs
            .retain(|d| now.saturating_sub(*d) < FLAP_WINDOW_MS);
        let was = self.flapping;
        if changed == Some(Status::Down) && self.downs.len() >= FLAP_DOWNS {
            self.flapping = true;
        } else if self.flapping && now.saturating_sub(self.since) >= FLAP_WINDOW_MS {
            self.flapping = false;
        }
        let notify = self.decide(now, was);
        Step {
            changed,
            notify,
            pending: false,
        }
    }

    /// A failed check before the first success: counted, never a down.
    fn observe_pending(&mut self, now: u64) -> Step {
        let first = *self.first_check.get_or_insert(now);
        self.fails = self.fails.saturating_add(1);
        self.oks = 0;
        self.failing_since.get_or_insert(now);
        let mut notify = Notify::None;
        if !self.never_up && now.saturating_sub(first) >= NEVER_UP_MS {
            self.never_up = true;
            self.notified = Status::Down;
            self.down_since = Some(first);
            notify = Notify::NeverUp { since: first };
        }
        Step {
            changed: None,
            notify,
            pending: true,
        }
    }

    /// Count the check; the status it now calls for, when that is a change.
    fn count(&mut self, ok: bool, now: u64, t: Thresholds) -> Option<Status> {
        if ok {
            self.oks = self.oks.saturating_add(1);
            self.fails = 0;
            self.failing_since = None;
            let enough = self.status == Status::Pending || self.oks >= t.recoveries.max(1);
            if enough {
                self.never_up = false;
                self.first_check = None;
            }
            (self.status != Status::Up && enough).then_some(Status::Up)
        } else {
            self.fails = self.fails.saturating_add(1);
            self.oks = 0;
            self.failing_since.get_or_insert(now);
            (self.status != Status::Down && self.fails >= t.failures.max(1)).then_some(Status::Down)
        }
    }

    fn decide(&mut self, now: u64, was_flapping: bool) -> Notify {
        // Held while flapping; the change that started it still goes out.
        if self.flapping && was_flapping {
            return Notify::None;
        }
        match (self.status, self.notified) {
            (Status::Down, n) if n != Status::Down => {
                let since = self.failing_since.unwrap_or(now).min(now);
                self.notified = Status::Down;
                self.down_since = Some(since);
                Notify::Down {
                    since,
                    flapping: self.flapping,
                }
            }
            (Status::Up, Status::Down) => {
                let down_since = self.down_since.take().unwrap_or(now);
                self.notified = Status::Up;
                Notify::Up {
                    down_since,
                    downtime_ms: now.saturating_sub(down_since),
                }
            }
            (Status::Up, Status::Pending) => {
                // The first up is not news.
                self.notified = Status::Up;
                Notify::None
            }
            _ => Notify::None,
        }
    }

    /// Its target was scaled to 0 at `now`: forget the run it was on, so
    /// starting again waits for the first success like a new monitor.
    /// Returns whether it was not stopped already.
    pub fn stop(&mut self, now: u64) -> bool {
        if self.stopped {
            return false;
        }
        *self = State {
            since: now,
            stopped: true,
            ..State::default()
        };
        true
    }

    /// Start counting afresh (a resumed or edited monitor), keeping what
    /// channels were told.
    pub fn reset_counts(&mut self) {
        self.fails = 0;
        self.oks = 0;
        self.failing_since = None;
        if self.status == Status::Pending {
            self.first_check = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Thresholds = Thresholds {
        failures: 2,
        recoveries: 2,
    };

    fn run(s: &mut State, checks: &[(bool, u64)]) -> Vec<Notify> {
        checks
            .iter()
            .map(|(ok, at)| s.observe(*ok, *at, T).notify)
            .filter(|n| *n != Notify::None)
            .collect()
    }

    #[test]
    fn thresholds_and_one_notification_per_incident() {
        let mut s = State::default();
        // The first success makes a new monitor up, quietly.
        assert_eq!(run(&mut s, &[(true, 1000)]), vec![]);
        assert_eq!(s.status, Status::Up);
        // One failure is not enough.
        assert_eq!(run(&mut s, &[(false, 2000), (true, 3000)]), vec![]);
        assert_eq!(s.status, Status::Up);
        // Two are; the incident starts at the first.
        let n = run(&mut s, &[(false, 4000), (false, 5000)]);
        assert_eq!(
            n,
            vec![Notify::Down {
                since: 4000,
                flapping: false
            }]
        );
        assert_eq!(s.status, Status::Down);
        // More failures say nothing more.
        assert_eq!(run(&mut s, &[(false, 6000), (false, 7000)]), vec![]);
        // One success does not recover it (hysteresis); a failure resets it.
        assert_eq!(
            run(&mut s, &[(true, 8000), (false, 9000), (true, 10_000)]),
            vec![]
        );
        assert_eq!(s.status, Status::Down);
        // Two in a row do, with the downtime from the first failure.
        let n = run(&mut s, &[(true, 11_000)]);
        assert_eq!(
            n,
            vec![Notify::Up {
                down_since: 4000,
                downtime_ms: 7000
            }]
        );
        assert_eq!(s.status, Status::Up);
        assert_eq!(s.notified, Status::Up);
    }

    #[test]
    fn a_new_monitor_waits_for_its_first_success() {
        let mut s = State::default();
        // Failures before the first success are pending, never down.
        for at in [10, 20, 30, 40] {
            let st = s.observe(false, at, T);
            assert!(st.pending);
            assert_eq!((st.changed, st.notify), (None, Notify::None));
        }
        assert_eq!(s.status, Status::Pending);
        assert!(s.downs.is_empty());
        // The first success is up, quietly, and clears the waiting.
        let st = s.observe(true, 50, T);
        assert!(!st.pending);
        assert_eq!((st.changed, st.notify), (Some(Status::Up), Notify::None));
        assert_eq!((s.first_check, s.never_up, s.fails), (None, false, 0));
        assert_eq!(s.notified, Status::Up);
    }

    #[test]
    fn pending_failures_do_not_count_toward_the_threshold() {
        let mut s = State::default();
        run(&mut s, &[(false, 10), (false, 20), (false, 30)]);
        // Up on the first success; one failure then is still below 2.
        assert_eq!(run(&mut s, &[(true, 40), (false, 50)]), vec![]);
        assert_eq!(s.status, Status::Up);
        assert_eq!(s.fails, 1);
    }

    #[test]
    fn real_downtime_after_up_still_pages() {
        let mut s = State::default();
        run(&mut s, &[(false, 10), (true, 20)]);
        let n = run(&mut s, &[(false, 30), (false, 40)]);
        assert_eq!(
            n,
            vec![Notify::Down {
                since: 30,
                flapping: false
            }]
        );
        assert_eq!(s.status, Status::Down);
        assert!(!s.observe(false, 50, T).pending);
    }

    #[test]
    fn a_monitor_that_never_comes_up_says_so_once() {
        let mut s = State::default();
        assert_eq!(run(&mut s, &[(false, 1000)]), vec![]);
        // Just short of the window: nothing.
        assert_eq!(run(&mut s, &[(false, 1000 + NEVER_UP_MS - 1)]), vec![]);
        let n = run(
            &mut s,
            &[(false, 1000 + NEVER_UP_MS), (false, 2000 + NEVER_UP_MS)],
        );
        assert_eq!(n, vec![Notify::NeverUp { since: 1000 }]);
        // Still pending, flagged; no second message.
        assert_eq!((s.status, s.never_up), (Status::Pending, true));
        assert_eq!(run(&mut s, &[(false, 3000 + NEVER_UP_MS)]), vec![]);
        // When it finally answers, channels hear it is up.
        let at = 4000 + NEVER_UP_MS;
        let n = run(&mut s, &[(true, at)]);
        assert_eq!(
            n,
            vec![Notify::Up {
                down_since: 1000,
                downtime_ms: at - 1000
            }]
        );
        assert_eq!((s.status, s.never_up), (Status::Up, false));
    }

    #[test]
    fn thresholds_of_one() {
        let one = Thresholds {
            failures: 1,
            recoveries: 1,
        };
        let mut s = State::default();
        assert_eq!(s.observe(true, 1, one).notify, Notify::None);
        assert!(matches!(
            s.observe(false, 2, one).notify,
            Notify::Down { .. }
        ));
        assert!(matches!(s.observe(true, 3, one).notify, Notify::Up { .. }));
        // Zero is treated as one.
        let zero = Thresholds {
            failures: 0,
            recoveries: 0,
        };
        assert!(matches!(
            s.observe(false, 4, zero).notify,
            Notify::Down { .. }
        ));
    }

    #[test]
    fn flapping_is_damped_and_settles() {
        let one = Thresholds {
            failures: 1,
            recoveries: 1,
        };
        let mut s = State::default();
        let mut sent = Vec::new();
        let mut at = 1_000;
        s.observe(true, at, one);
        // Down and up every minute, ten times.
        for _ in 0..10 {
            for ok in [false, true] {
                at += 60_000;
                let n = s.observe(ok, at, one).notify;
                if n != Notify::None {
                    sent.push(n);
                }
            }
        }
        // down, up, down, up, then the third down (flapping), then quiet.
        assert_eq!(sent.len(), 5, "{sent:?}");
        assert!(matches!(sent[4], Notify::Down { flapping: true, .. }));
        assert!(s.flapping);
        // It stays up: nothing until it has held for the window, then the
        // up channels have not heard yet.
        let mut later = Vec::new();
        for _ in 0..40 {
            at += 60_000;
            let n = s.observe(true, at, one).notify;
            if n != Notify::None {
                later.push(n);
            }
        }
        assert_eq!(later.len(), 1, "{later:?}");
        assert!(matches!(later[0], Notify::Up { .. }));
        assert!(!s.flapping);
        assert_eq!(s.notified, Status::Up);
    }

    #[test]
    fn flapping_that_settles_down_is_not_told_twice() {
        let one = Thresholds {
            failures: 1,
            recoveries: 1,
        };
        let mut s = State::default();
        let mut at = 0;
        let mut sent = Vec::new();
        for ok in [true, false, true, false, true, false, true, false] {
            at += 1000;
            let n = s.observe(ok, at, one).notify;
            if n != Notify::None {
                sent.push(n);
            }
        }
        // The last flap left it down, and channels heard the flapping down.
        assert_eq!(s.notified, Status::Down);
        let n_before = sent.len();
        for _ in 0..40 {
            at += 60_000;
            let n = s.observe(false, at, one).notify;
            if n != Notify::None {
                sent.push(n);
            }
        }
        assert_eq!(sent.len(), n_before, "{sent:?}");
        assert!(!s.flapping);
    }

    #[test]
    fn stopping_is_quiet_and_starting_begins_anew() {
        let mut s = State::default();
        run(&mut s, &[(true, 1), (false, 2), (false, 3)]);
        assert_eq!(s.notified, Status::Down);
        assert!(s.stop(4));
        assert!(!s.stop(5), "already stopped");
        assert_eq!((s.status, s.since, s.stopped), (Status::Pending, 4, true));
        // Failures while it comes back up are pending, never a down.
        assert_eq!(
            run(&mut s, &[(false, 10), (false, 11), (false, 12)]),
            vec![]
        );
        assert!(!s.stopped);
        assert_eq!(s.status, Status::Pending);
        // Its first success makes it up, and no up goes out: the down
        // channels heard belonged to the run before the stop.
        assert_eq!(run(&mut s, &[(true, 13)]), vec![]);
        assert_eq!(s.status, Status::Up);
    }

    #[test]
    fn state_survives_a_restart() {
        let mut s = State::default();
        run(&mut s, &[(true, 1), (false, 2), (false, 3)]);
        assert_eq!(s.status, Status::Down);
        let saved = serde_json::to_string(&s).unwrap();
        let mut back: State = serde_json::from_str(&saved).unwrap();
        assert_eq!(back, s);
        // Still down after the restart: no second down.
        assert_eq!(run(&mut back, &[(false, 4), (false, 5)]), vec![]);
        let n = run(&mut back, &[(true, 6), (true, 7)]);
        assert_eq!(
            n,
            vec![Notify::Up {
                down_since: 2,
                downtime_ms: 5
            }]
        );
        // An old state file without the newer fields still loads.
        let old: State = serde_json::from_str(r#"{"status":"up"}"#).unwrap();
        assert_eq!(old.status, Status::Up);
    }
}
