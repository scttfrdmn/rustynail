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
