//! Heartbeats: the control plane asks each agent how it is every
//! [`INTERVAL`]; [`FAILURES`] misses in a row make the server unreachable
//! (`server.unreachable`), and the first answer after that recovers it
//! (`server.recovered`).

use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

pub const INTERVAL: Duration = Duration::from_secs(10);
pub const TIMEOUT: Duration = Duration::from_secs(8);
/// Misses in a row before a server is unreachable (30 s at the interval).
pub const FAILURES: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Not heard from yet in this run.
    Unknown,
    Up,
    Unreachable,
}

/// A change worth an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transition {
    Unreachable,
    Recovered,
}

/// What the control plane knows of one server's health.
#[derive(Debug, Clone, Serialize)]
pub struct Health {
    pub state: State,
    /// Misses in a row.
    pub failures: u32,
    /// Unix seconds of the last answer.
    pub last_ok: Option<u64>,
    pub last_checked: Option<u64>,
    pub last_error: Option<String>,
    /// The last heartbeat: versions, CPU, memory, disk, orgs, its own last
    /// error.
    pub heartbeat: Value,
}

impl Default for Health {
    fn default() -> Self {
        Health {
            state: State::Unknown,
            failures: 0,
            last_ok: None,
            last_checked: None,
            last_error: None,
            heartbeat: Value::Null,
        }
    }
}

impl Health {
    /// Take one heartbeat's outcome at `now`; the transition it causes.
    pub fn observe(&mut self, r: Result<Value, String>, now: u64) -> Option<Transition> {
        self.last_checked = Some(now);
        match r {
            Ok(v) => {
                let was = self.state;
                self.state = State::Up;
                self.failures = 0;
                self.last_ok = Some(now);
                self.last_error = None;
                self.heartbeat = v;
                (was == State::Unreachable).then_some(Transition::Recovered)
            }
            Err(e) => {
                self.failures = self.failures.saturating_add(1);
                self.last_error = Some(e);
                if self.failures >= FAILURES && self.state != State::Unreachable {
                    self.state = State::Unreachable;
                    return Some(Transition::Unreachable);
                }
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn misses_make_it_unreachable_once_and_an_answer_recovers_it() {
        let mut h = Health::default();
        assert_eq!(
            h.observe(Ok(json!({"isb": "x"})), 1),
            None,
            "first contact is quiet"
        );
        assert_eq!(h.state, State::Up);
        assert_eq!(h.observe(Err("timeout".into()), 2), None);
        assert_eq!(h.observe(Err("timeout".into()), 3), None);
        assert_eq!(h.state, State::Up, "two misses are a blip");
        assert_eq!(
            h.observe(Err("refused".into()), 4),
            Some(Transition::Unreachable)
        );
        assert_eq!(h.state, State::Unreachable);
        assert_eq!(h.observe(Err("refused".into()), 5), None, "said once");
        assert_eq!(h.last_error.as_deref(), Some("refused"));
        assert_eq!(h.last_ok, Some(1));
        assert_eq!(h.observe(Ok(json!({})), 6), Some(Transition::Recovered));
        assert_eq!(
            (h.state, h.failures, h.last_error.clone()),
            (State::Up, 0, None)
        );
        assert_eq!(h.observe(Ok(json!({})), 7), None);
    }

    #[test]
    fn a_server_never_heard_from_still_goes_unreachable() {
        let mut h = Health::default();
        for t in 0..FAILURES - 1 {
            assert_eq!(h.observe(Err("down".into()), t as u64), None);
        }
        assert_eq!(
            h.observe(Err("down".into()), 9),
            Some(Transition::Unreachable)
        );
        assert_eq!(h.observe(Ok(json!({})), 10), Some(Transition::Recovered));
    }
}
