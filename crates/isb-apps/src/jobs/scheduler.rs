//! The daemon's scheduler: one thread that fires jobs and backups when
//! their cron schedules come due.
//!
//! Each schedule keeps an anchor: the last slot it fired for (its creation
//! time before the first). A schedule is due when its next slot after the
//! anchor has passed. It then fires once, for the latest slot that has
//! passed, so a burst of missed slots is one run, not many.
//!
//! Missed while the daemon was down: when the daemon starts, a schedule
//! whose latest passed slot is within its grace window (default one hour)
//! runs once, late (`trigger: missed`); older slots are skipped and the
//! anchor moves to now.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::cron::Schedule;
use crate::org::OrgId;

/// Missed slots this recent still run at startup.
pub const DEFAULT_GRACE: i64 = 3600;

/// The longest the scheduler sleeps before looking again.
const MAX_SLEEP: Duration = Duration::from_secs(15);

/// One schedule, as its owner reports it.
#[derive(Debug, Clone)]
pub struct Entry {
    pub org: OrgId,
    pub name: String,
    pub schedule: Schedule,
    /// Unix seconds of the last slot fired (or of creation).
    pub anchor: i64,
    /// Seconds a missed slot may be late and still run.
    pub grace: i64,
}

/// What to do with an entry now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Not due; the next slot is at this time (`None`: never again).
    Wait(Option<i64>),
    /// Run for `slot`; `late` when the slot passed more than a minute ago.
    Run { slot: i64, late: bool },
    /// Every passed slot is older than the grace window: move the anchor
    /// to `to` without running.
    Skip { to: i64 },
}

/// Decide for one schedule at `now`.
pub fn decide(s: &Schedule, anchor: i64, now: i64, grace: i64) -> Decision {
    let Some(next) = s.next_after(anchor) else {
        return Decision::Wait(None);
    };
    if next > now {
        return Decision::Wait(Some(next));
    }
    // The first slot worth running: the next one, or the first inside the
    // grace window when the next is older than that.
    let first = if now - next <= grace {
        next
    } else {
        match s.next_after(now - grace - 1) {
            Some(f) if f <= now => f,
            _ => return Decision::Skip { to: now },
        }
    };
    // The latest passed slot, walking forward (at most the grace window's
    // worth of slots).
    let mut slot = first;
    while let Some(n) = s.next_after(slot) {
        if n > now {
            break;
        }
        slot = n;
    }
    Decision::Run {
        slot,
        late: now - slot > 60,
    }
}

/// Something with schedules: jobs, backups.
pub trait Scheduled: Send + Sync {
    /// Every enabled schedule.
    fn entries(&self) -> Vec<Entry>;
    /// Record `slot` as the entry's anchor, then start its run (without
    /// blocking the scheduler). The anchor must be saved before this
    /// returns, so a slot never fires twice.
    fn fire(&self, e: &Entry, slot: i64, late: bool);
    /// Move the anchor without running (missed beyond the grace window).
    fn advance(&self, e: &Entry, to: i64);
}

struct Shared {
    stop: AtomicBool,
    wake: Condvar,
    lock: Mutex<bool>,
}

/// The scheduler thread's handle.
#[derive(Clone)]
pub struct Scheduler {
    shared: Arc<Shared>,
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl Scheduler {
    /// Start the thread over `sources`.
    pub fn start(sources: Vec<Arc<dyn Scheduled>>) -> Scheduler {
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            wake: Condvar::new(),
            lock: Mutex::new(false),
        });
        let s2 = shared.clone();
        std::thread::Builder::new()
            .name("isb-scheduler".into())
            .spawn(move || run(s2, sources))
            .expect("spawn the scheduler");
        Scheduler { shared }
    }

    /// A scheduler that runs nothing (tests, and before the daemon starts
    /// one).
    pub fn idle() -> Scheduler {
        Scheduler {
            shared: Arc::new(Shared {
                stop: AtomicBool::new(true),
                wake: Condvar::new(),
                lock: Mutex::new(false),
            }),
        }
    }

    /// Look at the schedules again now (one was created or changed).
    pub fn wake(&self) {
        *self.shared.lock.lock().unwrap() = true;
        self.shared.wake.notify_all();
    }

    pub fn shutdown(&self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.wake();
    }
}

fn run(shared: Arc<Shared>, sources: Vec<Arc<dyn Scheduled>>) {
    while !shared.stop.load(Ordering::SeqCst) {
        let now = now_secs();
        let mut next_due: Option<i64> = None;
        for src in &sources {
            for e in src.entries() {
                match decide(&e.schedule, e.anchor, now, e.grace) {
                    Decision::Wait(Some(t)) => {
                        next_due = Some(next_due.map_or(t, |n| n.min(t)));
                    }
                    Decision::Wait(None) => {}
                    Decision::Run { slot, late } => src.fire(&e, slot, late),
                    Decision::Skip { to } => src.advance(&e, to),
                }
            }
        }
        let sleep = match next_due {
            Some(t) => Duration::from_millis(((t - now_secs()).max(0) as u64) * 1000 + 50),
            None => MAX_SLEEP,
        }
        .min(MAX_SLEEP)
        .max(Duration::from_millis(200));
        let g = shared.lock.lock().unwrap();
        let (mut g, _) = shared
            .wake
            .wait_timeout_while(g, sleep, |woken| !*woken)
            .unwrap();
        *g = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_790_000_000 - 1_790_000_000 % 3600; // on the hour

    #[test]
    fn decisions() {
        let every = Schedule::parse("* * * * *").unwrap();
        let hourly = Schedule::parse("@hourly").unwrap();
        // Not yet due.
        assert_eq!(
            decide(&every, T0, T0 + 30, 3600),
            Decision::Wait(Some(T0 + 60))
        );
        // Due: the slot that just passed.
        assert_eq!(
            decide(&every, T0, T0 + 61, 3600),
            Decision::Run {
                slot: T0 + 60,
                late: false
            }
        );
        // Several missed minutes run once, for the latest.
        assert_eq!(
            decide(&every, T0, T0 + 600, 3600),
            Decision::Run {
                slot: T0 + 600,
                late: false
            }
        );
        assert_eq!(
            decide(&every, T0, T0 + 630, 3600),
            Decision::Run {
                slot: T0 + 600,
                late: false
            }
        );
        // An hourly slot missed by 20 minutes, inside the grace window: late.
        assert_eq!(
            decide(&hourly, T0 - 1, T0 + 1200, 3600),
            Decision::Run {
                slot: T0,
                late: true
            }
        );
        // Missed by more than the grace window: skipped.
        assert_eq!(
            decide(&hourly, T0 - 1, T0 + 1200, 600),
            Decision::Skip { to: T0 + 1200 }
        );
        // Down for a day, every minute, short grace: the latest in the window.
        assert_eq!(
            decide(&every, T0, T0 + 86_400 + 5, 300),
            Decision::Run {
                slot: T0 + 86_400,
                late: false
            }
        );
        // A daily schedule down for two days, with a day's grace: once.
        let daily = Schedule::parse("@daily").unwrap();
        let midnight = T0 - T0 % 86_400;
        assert_eq!(
            decide(&daily, midnight - 10, midnight + 2 * 86_400 + 3600, 86_400),
            Decision::Run {
                slot: midnight + 2 * 86_400,
                late: true
            }
        );
    }

    /// The fake fires record their slots and move the anchor.
    struct Fake {
        entries: Mutex<Vec<Entry>>,
        fired: Mutex<Vec<(String, i64, bool)>>,
    }

    impl Scheduled for Fake {
        fn entries(&self) -> Vec<Entry> {
            self.entries.lock().unwrap().clone()
        }
        fn fire(&self, e: &Entry, slot: i64, late: bool) {
            self.fired
                .lock()
                .unwrap()
                .push((e.name.clone(), slot, late));
            for x in self.entries.lock().unwrap().iter_mut() {
                if x.name == e.name {
                    x.anchor = slot;
                }
            }
        }
        fn advance(&self, e: &Entry, to: i64) {
            for x in self.entries.lock().unwrap().iter_mut() {
                if x.name == e.name {
                    x.anchor = to;
                }
            }
        }
    }

    #[test]
    fn the_thread_fires_missed_runs_once() {
        let now = now_secs();
        let e = |name: &str, expr: &str, anchor: i64, grace: i64| Entry {
            org: OrgId::default_org(),
            name: name.into(),
            schedule: Schedule::parse(expr).unwrap(),
            anchor,
            grace,
        };
        let fake = Arc::new(Fake {
            entries: Mutex::new(vec![
                // Down for three hours: within a day's grace, runs once.
                e("hourly", "@hourly", now - 3 * 3600 - 60, 86_400),
                // Down for a day, grace an hour: skipped.
                e("daily", "0 0 1 1 *", now - 400 * 86_400, 3600),
            ]),
            fired: Mutex::new(vec![]),
        });
        let s = Scheduler::start(vec![fake.clone() as Arc<dyn Scheduled>]);
        let started = std::time::Instant::now();
        while fake.fired.lock().unwrap().is_empty() {
            assert!(started.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(400));
        s.wake();
        std::thread::sleep(Duration::from_millis(300));
        s.shutdown();
        let fired = fake.fired.lock().unwrap().clone();
        assert_eq!(fired.len(), 1, "{fired:?}");
        assert_eq!(fired[0].0, "hourly");
        assert!(fired[0].1 > now - 3600 && fired[0].1 <= now);
        let daily = &fake.entries.lock().unwrap()[1];
        assert!(daily.anchor >= now, "moved past the missed slot");
    }
}
