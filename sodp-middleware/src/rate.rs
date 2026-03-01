//! Sliding-window per-session rate limiter.
//!
//! Intentionally simple: one counter per 1-second window.  Suitable for
//! protecting individual connections from write bursts.  For cluster-wide
//! rate limiting (e.g. per-user across nodes) a Redis-backed token bucket
//! is the right tool — that belongs in a future `sodp-cluster` crate.
//!
//! # Example
//!
//! ```rust
//! use sodp_middleware::rate::RateLimiter;
//!
//! let mut limiter = RateLimiter::new(10); // 10 ops/sec
//! assert!(limiter.allow());              // first op — allowed
//! ```

use std::time::Instant;

/// Sliding 1-second window rate limiter.
///
/// Call [`allow`](RateLimiter::allow) before each guarded operation.
/// Returns `false` — without closing the connection — when the per-second
/// budget is exhausted.
#[derive(Debug)]
pub struct RateLimiter {
    max_per_sec: u32,
    count:       u32,
    window:      Instant,
}

impl RateLimiter {
    /// Create a limiter allowing at most `max_per_sec` operations per second.
    pub fn new(max_per_sec: u32) -> Self {
        RateLimiter { max_per_sec, count: 0, window: Instant::now() }
    }

    /// Returns `true` if the operation is within budget, `false` if it should
    /// be rejected with a 429 response.
    pub fn allow(&mut self) -> bool {
        if self.window.elapsed().as_secs() >= 1 {
            self.window = Instant::now();
            self.count  = 0;
        }
        self.count += 1;
        self.count <= self.max_per_sec
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_up_to_limit() {
        let mut r = RateLimiter::new(3);
        assert!(r.allow());
        assert!(r.allow());
        assert!(r.allow());
        assert!(!r.allow()); // 4th → over budget
    }
}
