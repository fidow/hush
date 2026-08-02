//! In-memory sliding-window rate limiter.
//!
//! Without this, the 6-digit verification code and the login password are
//! brute-forceable, and Argon2 verification turns every login endpoint into a
//! CPU exhaustion vector. Single-process and in-memory on purpose: the server
//! is one binary, and a restart resetting the counters is acceptable.

use std::collections::HashMap;
use std::sync::Mutex;

/// How many stale keys we tolerate before sweeping the map.
const SWEEP_THRESHOLD: usize = 10_000;

#[derive(Default)]
pub struct RateLimiter {
    hits: Mutex<HashMap<String, Vec<i64>>>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records an attempt for `key`, returning false when more than `max`
    /// attempts happened within `window_ms`.
    pub fn allow(&self, key: &str, max: usize, window_ms: i64, now: i64) -> bool {
        let mut hits = self.hits.lock().expect("rate limiter poisoned");

        if hits.len() > SWEEP_THRESHOLD {
            hits.retain(|_, times| times.iter().any(|t| now - t < window_ms));
        }

        let times = hits.entry(key.to_string()).or_default();
        times.retain(|t| now - *t < window_ms);
        if times.len() >= max {
            return false;
        }
        times.push(now);
        true
    }

    /// Clears the counters for `key` (e.g. after a successful login).
    pub fn reset(&self, key: &str) {
        self.hits.lock().expect("rate limiter poisoned").remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_after_max_attempts_and_recovers_after_the_window() {
        let rl = RateLimiter::new();
        for i in 0..3 {
            assert!(rl.allow("k", 3, 1000, 100 + i), "attempt {i} should pass");
        }
        assert!(!rl.allow("k", 3, 1000, 103), "fourth attempt must be blocked");
        assert!(rl.allow("k", 3, 1000, 2000), "window elapsed, allowed again");
    }

    #[test]
    fn keys_are_independent_and_reset_works() {
        let rl = RateLimiter::new();
        assert!(!rl.allow("a", 0, 1000, 0));
        assert!(rl.allow("b", 1, 1000, 0));
        assert!(!rl.allow("b", 1, 1000, 0));
        rl.reset("b");
        assert!(rl.allow("b", 1, 1000, 0));
    }
}
