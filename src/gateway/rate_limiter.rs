//! Per-user sliding-window rate limiter.
//!
//! Tracks message counts per user in a DashMap. Each entry stores `(count, window_start)`.
//! The window resets when `elapsed >= window_seconds`.

use crate::config::RateLimitConfig;
use dashmap::DashMap;
use std::sync::Arc;
use std::time::Instant;

/// Sliding-window per-user rate limiter.
pub struct RateLimiter {
    /// `user_id → (count, window_start)`
    limits: DashMap<String, (u32, Instant)>,
}

impl RateLimiter {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            limits: DashMap::new(),
        })
    }

    /// Returns `true` if the message is allowed, `false` if the rate limit is exceeded.
    ///
    /// When `config.enabled` is `false`, always returns `true`.
    pub fn check_and_record(&self, user_id: &str, config: &RateLimitConfig) -> bool {
        if !config.enabled {
            return true;
        }

        let window = std::time::Duration::from_secs(config.window_seconds);

        let mut entry = self
            .limits
            .entry(user_id.to_string())
            .or_insert((0, Instant::now()));

        let (ref mut count, ref mut window_start) = *entry;

        if window_start.elapsed() >= window {
            // Window expired — reset
            *count = 1;
            *window_start = Instant::now();
            true
        } else if *count >= config.messages_per_window {
            // Window still active and limit reached
            false
        } else {
            *count += 1;
            true
        }
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self {
            limits: DashMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(enabled: bool, messages_per_window: u32, window_seconds: u64) -> RateLimitConfig {
        RateLimitConfig {
            enabled,
            messages_per_window,
            window_seconds,
        }
    }

    #[test]
    fn test_disabled_always_allows() {
        let rl = RateLimiter::default();
        let c = cfg(false, 1, 60);
        // Even after many calls, disabled limiter always returns true
        for _ in 0..10 {
            assert!(rl.check_and_record("user1", &c));
        }
    }

    #[test]
    fn test_within_window_allows() {
        let rl = RateLimiter::default();
        let c = cfg(true, 5, 60);
        // 3 messages under a limit of 5 → all pass
        assert!(rl.check_and_record("user1", &c));
        assert!(rl.check_and_record("user1", &c));
        assert!(rl.check_and_record("user1", &c));
    }

    #[test]
    fn test_exceeds_limit_blocks() {
        let rl = RateLimiter::default();
        let c = cfg(true, 3, 60);
        assert!(rl.check_and_record("user1", &c)); // 1
        assert!(rl.check_and_record("user1", &c)); // 2
        assert!(rl.check_and_record("user1", &c)); // 3 — hits the limit
        // 4th message should be blocked
        assert!(!rl.check_and_record("user1", &c));
    }

    #[test]
    fn test_window_reset_allows_again() {
        let rl = RateLimiter::default();
        // window_seconds = 0 means the window expires immediately after each call
        let c = cfg(true, 2, 0);
        assert!(rl.check_and_record("user1", &c)); // 1st call — resets window, count = 1
        // With window_seconds=0, elapsed() >= Duration::ZERO is always true,
        // so the next call resets and allows.
        assert!(rl.check_and_record("user1", &c)); // window expired → reset, count = 1
        assert!(rl.check_and_record("user1", &c)); // window expired → reset, count = 1
    }

    #[test]
    fn test_independent_users() {
        let rl = RateLimiter::default();
        let c = cfg(true, 1, 60);
        assert!(rl.check_and_record("alice", &c));
        assert!(!rl.check_and_record("alice", &c)); // alice blocked
        assert!(rl.check_and_record("bob", &c)); // bob unaffected
    }
}
