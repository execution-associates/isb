//! Host-wide pacing of the instance starts and creates the stack controller
//! makes, across every stack and org.
//!
//! Each service has its own worker thread, so after a host boot (or an
//! `isb serve` restart beside stopped replicas) every worker would start its
//! instances in the same second. On a host whose instances share one disk
//! that is an I/O storm: on 2026-10-24 titan sat at io pressure 98% for 85
//! minutes, with sshd and tailscaled starved. The gate bounds how many starts
//! run at once, spaces them, and holds new ones while the host's io pressure
//! (`/proc/pressure/io`) is high. A setup after a boot it did not cause (the
//! instance was already running) waits out the pressure only.
//!
//! `isb serve --start-concurrency` sets how many starts run at once (default
//! 4), and `--start-io-pressure` the `full avg10` percentage at or above
//! which new ones wait (default 40; `0` turns the check off). A start waits
//! at most [`MAX_HOLD`] for pressure to fall, so a host that stays busy
//! still recovers.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// The default starts in flight at once.
pub const DEFAULT_CONCURRENCY: u32 = 4;
/// The default io pressure (`full avg10`, percent) that holds new starts.
pub const DEFAULT_IO_PRESSURE: f64 = 40.0;
/// The least time between two starts.
pub const SPACING: Duration = Duration::from_secs(1);
/// The longest a start waits for io pressure to fall.
pub const MAX_HOLD: Duration = Duration::from_secs(300);
/// How often a held start looks again.
const POLL: Duration = Duration::from_millis(500);

type PressureFn = Box<dyn Fn() -> Option<f64> + Send + Sync>;

/// How the controller paces starts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pacing {
    /// Starts in flight at once.
    pub concurrency: usize,
    /// The io pressure that holds new starts; `None` never holds them.
    pub io_pressure: Option<f64>,
}

impl Default for Pacing {
    fn default() -> Pacing {
        Pacing {
            concurrency: DEFAULT_CONCURRENCY as usize,
            io_pressure: Some(DEFAULT_IO_PRESSURE),
        }
    }
}

/// The gate. One per controller.
pub struct StartGate {
    max: usize,
    spacing: Duration,
    limit: Option<f64>,
    max_hold: Duration,
    pressure: PressureFn,
    state: Mutex<State>,
    cv: Condvar,
}

#[derive(Default)]
struct State {
    in_flight: usize,
    last: Option<Instant>,
}

/// A start in flight: the slot frees when it drops.
pub struct Permit<'a> {
    gate: &'a StartGate,
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        let mut st = self.gate.state.lock().unwrap();
        st.in_flight -= 1;
        self.gate.cv.notify_all();
    }
}

impl std::fmt::Debug for StartGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StartGate")
            .field("max", &self.max)
            .field("limit", &self.limit)
            .finish_non_exhaustive()
    }
}

impl StartGate {
    /// A gate on this host's `/proc/pressure/io`.
    pub fn host(p: Pacing) -> StartGate {
        StartGate::new(
            p.concurrency,
            SPACING,
            p.io_pressure,
            MAX_HOLD,
            Box::new(host_io_pressure),
        )
    }

    pub fn new(
        max: usize,
        spacing: Duration,
        limit: Option<f64>,
        max_hold: Duration,
        pressure: PressureFn,
    ) -> StartGate {
        StartGate {
            max: max.max(1),
            spacing,
            limit,
            max_hold,
            pressure,
            state: Mutex::new(State::default()),
            cv: Condvar::new(),
        }
    }

    /// Whether io pressure is at or above the limit now.
    fn pressured(&self) -> Option<f64> {
        let limit = self.limit?;
        (self.pressure)().filter(|p| *p >= limit)
    }

    /// Wait for a start slot: fewer than the maximum in flight, the spacing
    /// since the last start passed, and io pressure under the limit (or
    /// held for [`MAX_HOLD`] already). `held` is called once, with the
    /// pressure, the first time pressure holds the start. `None` when
    /// `stop` was set while waiting.
    pub fn acquire(&self, stop: &AtomicBool, held: &mut dyn FnMut(f64)) -> Option<Permit<'_>> {
        let since = Instant::now();
        let mut told = false;
        loop {
            if stop.load(Ordering::Relaxed) {
                return None;
            }
            // Read outside the lock: it is a file read.
            let pressure = if since.elapsed() < self.max_hold {
                self.pressured()
            } else {
                None
            };
            if let Some(p) = pressure {
                if !told {
                    told = true;
                    held(p);
                }
                std::thread::sleep(POLL);
                continue;
            }
            let mut st = self.state.lock().unwrap();
            let spaced = st.last.is_none_or(|t| t.elapsed() >= self.spacing);
            if st.in_flight < self.max && spaced {
                st.in_flight += 1;
                st.last = Some(Instant::now());
                return Some(Permit { gate: self });
            }
            let wait = match st.last {
                Some(t) if st.in_flight < self.max => self.spacing.saturating_sub(t.elapsed()),
                _ => POLL,
            };
            let _ = self
                .cv
                .wait_timeout(st, wait.clamp(Duration::from_millis(10), POLL));
        }
    }

    /// Wait (up to [`MAX_HOLD`]) for io pressure to fall under the limit,
    /// without taking a slot. `held` as for [`StartGate::acquire`].
    pub fn wait_pressure(&self, stop: &AtomicBool, held: &mut dyn FnMut(f64)) {
        let since = Instant::now();
        let mut told = false;
        while since.elapsed() < self.max_hold && !stop.load(Ordering::Relaxed) {
            let Some(p) = self.pressured() else { return };
            if !told {
                told = true;
                held(p);
            }
            std::thread::sleep(POLL);
        }
    }
}

/// The host's io pressure: `full avg10` from `/proc/pressure/io`, the share
/// of the last ten seconds in which every runnable task waited on I/O.
/// `None` without PSI (an old kernel, or not Linux).
pub fn host_io_pressure() -> Option<f64> {
    parse_full_avg10(&std::fs::read_to_string("/proc/pressure/io").ok()?)
}

fn parse_full_avg10(s: &str) -> Option<f64> {
    let line = s.lines().find(|l| l.starts_with("full "))?;
    line.split_whitespace()
        .find_map(|f| f.strip_prefix("avg10="))?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    fn gate(max: usize, spacing: Duration, pressure: Arc<Mutex<f64>>) -> StartGate {
        StartGate::new(
            max,
            spacing,
            Some(40.0),
            Duration::from_secs(5),
            Box::new(move || Some(*pressure.lock().unwrap())),
        )
    }

    #[test]
    fn parses_psi() {
        let s = "some avg10=10.61 avg60=5.87 avg300=1.75 total=2985247473\nfull avg10=9.04 avg60=4.92 avg300=1.44 total=2430035415\n";
        assert_eq!(parse_full_avg10(s), Some(9.04));
        assert_eq!(parse_full_avg10("some avg10=1.00\n"), None);
    }

    #[test]
    fn bounds_starts_in_flight() {
        let g = Arc::new(gate(2, Duration::ZERO, Arc::new(Mutex::new(0.0))));
        let stop = Arc::new(AtomicBool::new(false));
        let (now, peak) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let threads: Vec<_> = (0..6)
            .map(|_| {
                let (g, stop, now, peak) = (g.clone(), stop.clone(), now.clone(), peak.clone());
                std::thread::spawn(move || {
                    let _p = g.acquire(&stop, &mut |_| {}).unwrap();
                    let n = now.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(n, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(50));
                    now.fetch_sub(1, Ordering::SeqCst);
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(peak.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn spaces_starts() {
        let g = gate(8, Duration::from_millis(100), Arc::new(Mutex::new(0.0)));
        let stop = AtomicBool::new(false);
        let t = Instant::now();
        for _ in 0..3 {
            drop(g.acquire(&stop, &mut |_| {}).unwrap());
        }
        assert!(
            t.elapsed() >= Duration::from_millis(200),
            "{:?}",
            t.elapsed()
        );
    }

    #[test]
    fn holds_while_io_pressure_is_high() {
        let p = Arc::new(Mutex::new(90.0));
        let g = Arc::new(gate(4, Duration::ZERO, p.clone()));
        let stop = Arc::new(AtomicBool::new(false));
        let held = Arc::new(Mutex::new(Vec::new()));
        let t = {
            let (g, stop, held) = (g.clone(), stop.clone(), held.clone());
            std::thread::spawn(move || {
                let at = Instant::now();
                let ok = g
                    .acquire(&stop, &mut |p| held.lock().unwrap().push(p))
                    .is_some();
                (ok, at.elapsed())
            })
        };
        std::thread::sleep(Duration::from_millis(700));
        *p.lock().unwrap() = 5.0;
        let (ok, waited) = t.join().unwrap();
        assert!(ok);
        assert!(waited >= Duration::from_millis(700), "{waited:?}");
        // Told once, with the pressure that held it.
        assert_eq!(*held.lock().unwrap(), vec![90.0]);
    }

    #[test]
    fn pressure_holds_at_most_max_hold() {
        let g = StartGate::new(
            4,
            Duration::ZERO,
            Some(40.0),
            Duration::from_millis(600),
            Box::new(|| Some(99.0)),
        );
        let t = Instant::now();
        assert!(g.acquire(&AtomicBool::new(false), &mut |_| {}).is_some());
        assert!(t.elapsed() >= Duration::from_millis(600));
        let t = Instant::now();
        g.wait_pressure(&AtomicBool::new(false), &mut |_| {});
        assert!(t.elapsed() >= Duration::from_millis(600));
    }

    #[test]
    fn stop_ends_the_wait() {
        let g = StartGate::new(
            1,
            Duration::ZERO,
            None,
            Duration::from_secs(5),
            Box::new(|| None),
        );
        let stop = AtomicBool::new(false);
        let _held = g.acquire(&stop, &mut |_| {}).unwrap();
        stop.store(true, Ordering::Relaxed);
        assert!(g.acquire(&stop, &mut |_| {}).is_none());
    }

    #[test]
    fn no_limit_never_holds() {
        let g = StartGate::new(
            1,
            Duration::ZERO,
            None,
            Duration::from_secs(5),
            Box::new(|| Some(100.0)),
        );
        let t = Instant::now();
        drop(g.acquire(&AtomicBool::new(false), &mut |_| {}).unwrap());
        assert!(t.elapsed() < Duration::from_millis(400));
    }
}
