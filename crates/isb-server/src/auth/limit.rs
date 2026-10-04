//! An in-memory token bucket per key (an email, an IP), to blunt password
//! guessing. It resets when the daemon restarts, which is fine: the point is
//! to make online guessing slow, and a restart is rare and visible.

use std::collections::HashMap;
use std::sync::Mutex;

/// `burst` attempts at once, then one every `per_secs`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rate {
    pub burst: f64,
    pub per_secs: f64,
}

impl Rate {
    pub const fn new(burst: u32, per_secs: u32) -> Self {
        Rate {
            burst: burst as f64,
            per_secs: per_secs as f64,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    at: f64,
}

/// Past this many keys, full buckets (idle keys) are dropped.
const MAX_KEYS: usize = 10_000;

#[derive(Debug)]
pub struct RateLimiter {
    rate: Rate,
    buckets: Mutex<HashMap<String, Bucket>>,
}

impl RateLimiter {
    pub fn new(rate: Rate) -> Self {
        RateLimiter {
            rate,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Take one token for `key` at time `now` (seconds). `Err(secs)` says how
    /// long until the next one.
    pub fn take(&self, key: &str, now: f64) -> Result<(), u64> {
        let mut m = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        if m.len() >= MAX_KEYS && !m.contains_key(key) {
            let r = self.rate;
            m.retain(|_, b| refill(r, *b, now).tokens < r.burst);
            if m.len() >= MAX_KEYS {
                // Still full of active keys: someone is spraying. Refuse new
                // keys rather than grow without bound.
                return Err(r.per_secs.ceil() as u64);
            }
        }
        let b = m.entry(key.to_string()).or_insert(Bucket {
            tokens: self.rate.burst,
            at: now,
        });
        *b = refill(self.rate, *b, now);
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            Ok(())
        } else {
            Err(((1.0 - b.tokens) * self.rate.per_secs).ceil().max(1.0) as u64)
        }
    }
}

fn refill(r: Rate, b: Bucket, now: f64) -> Bucket {
    let dt = (now - b.at).max(0.0);
    Bucket {
        tokens: (b.tokens + dt / r.per_secs).min(r.burst),
        at: now,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket() {
        let l = RateLimiter::new(Rate::new(3, 10));
        for _ in 0..3 {
            assert!(l.take("a", 100.0).is_ok());
        }
        assert_eq!(l.take("a", 100.0), Err(10));
        // Other keys are independent.
        assert!(l.take("b", 100.0).is_ok());
        // Refills at one per 10s.
        assert_eq!(l.take("a", 105.0), Err(5));
        assert!(l.take("a", 110.0).is_ok());
        assert!(l.take("a", 110.0).is_err());
        // Never above the burst.
        for _ in 0..3 {
            assert!(l.take("a", 10_000.0).is_ok());
        }
        assert!(l.take("a", 10_000.0).is_err());
    }

    #[test]
    fn bounded() {
        let l = RateLimiter::new(Rate::new(1, 60));
        for i in 0..MAX_KEYS {
            assert!(l.take(&format!("k{i}"), 0.0).is_ok());
        }
        // Every bucket is empty (active): a new key is refused.
        assert!(l.take("new", 1.0).is_err());
        // Once they refill, idle keys are dropped and new ones fit.
        assert!(l.take("new", 120.0).is_ok());
        assert!(l.buckets.lock().unwrap().len() < 10);
    }
}
