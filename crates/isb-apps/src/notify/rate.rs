//! Retry backoff and per-channel rate limiting.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// The longest wait between attempts (Retry-After included).
pub const BACKOFF_MAX: Duration = Duration::from_secs(300);

/// The wait before attempt `attempt + 1` (attempt counts from 1): the base
/// doubling per attempt, at least what the server asked for, capped.
pub fn backoff(base: Duration, attempt: u32, retry_after: Option<Duration>) -> Duration {
    let exp = base.saturating_mul(1u32 << attempt.saturating_sub(1).min(16));
    exp.max(retry_after.unwrap_or_default()).min(BACKOFF_MAX)
}

/// Sliding one-minute window of sends.
#[derive(Debug, Default)]
pub struct RateLimit {
    sent: VecDeque<Instant>,
}

impl RateLimit {
    /// How long to wait before the next send may go, at `now`.
    pub fn wait(&mut self, now: Instant, per_minute: usize) -> Duration {
        while self
            .sent
            .front()
            .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(60))
        {
            self.sent.pop_front();
        }
        if self.sent.len() < per_minute {
            return Duration::ZERO;
        }
        (self.sent[0] + Duration::from_secs(60)).saturating_duration_since(now)
    }

    pub fn record(&mut self, now: Instant) {
        self.sent.push_back(now);
    }
}
