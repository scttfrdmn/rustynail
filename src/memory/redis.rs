use crate::memory::MemoryStore;
use redis::Commands;
use tracing::error;

/// Persists conversation history in Redis as a capped LIFO list.
///
/// Each user's history is stored under the key `rustynail:history:<user_id>`.
/// Messages are prepended with `LPUSH` (newest at index 0) and trimmed to
/// `max_history` entries. An optional TTL (`redis_ttl_seconds`) is renewed on
/// every write to keep active conversations alive.
pub struct RedisStore {
    client: redis::Client,
    max_history: usize,
    ttl_seconds: i64,
}

impl RedisStore {
    /// Create a new `RedisStore`.
    ///
    /// - `redis_url`: e.g. `"redis://localhost:6379"`
    /// - `max_history`: maximum messages to retain per user
    /// - `ttl_seconds`: expiry for each history key; `0` disables TTL
    pub fn new(redis_url: &str, max_history: usize, ttl_seconds: u64) -> anyhow::Result<Self> {
        let client = redis::Client::open(redis_url)
            .map_err(|e| anyhow::anyhow!("Redis client error: {}", e))?;
        Ok(Self {
            client,
            max_history,
            ttl_seconds: ttl_seconds as i64,
        })
    }

    fn history_key(user_id: &str) -> String {
        format!("rustynail:history:{}", user_id)
    }
}

impl MemoryStore for RedisStore {
    fn get_history(&self, user_id: &str) -> Vec<String> {
        let mut con = match self.client.get_connection() {
            Ok(c) => c,
            Err(e) => {
                error!("Redis get_connection error: {}", e);
                return Vec::new();
            }
        };
        let key = Self::history_key(user_id);
        // LRANGE 0 -1: returns newest-first (LPUSH order); reverse for chronological
        let values: Vec<String> = match con.lrange(&key, 0_isize, -1_isize) {
            Ok(v) => v,
            Err(e) => {
                error!("Redis LRANGE error: {}", e);
                Vec::new()
            }
        };
        let mut result = values;
        result.reverse();
        result
    }

    fn add_message(&self, user_id: &str, message: String) {
        let mut con = match self.client.get_connection() {
            Ok(c) => c,
            Err(e) => {
                error!("Redis get_connection error: {}", e);
                return;
            }
        };
        let key = Self::history_key(user_id);
        // LPUSH: prepend so newest is at index 0
        let _: redis::RedisResult<i64> = con.lpush(&key, &message);
        // LTRIM: keep only the first max_history entries (newest N)
        let _: redis::RedisResult<()> = con.ltrim(&key, 0_isize, (self.max_history as isize) - 1);
        // Renew TTL on each write
        if self.ttl_seconds > 0 {
            let _: redis::RedisResult<bool> = con.expire(&key, self.ttl_seconds);
        }
    }

    fn clear_history(&self, user_id: &str) {
        let mut con = match self.client.get_connection() {
            Ok(c) => c,
            Err(e) => {
                error!("Redis get_connection error: {}", e);
                return;
            }
        };
        let key = Self::history_key(user_id);
        let _: redis::RedisResult<i64> = con.del(&key);
    }

    fn max_history(&self) -> usize {
        self.max_history
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns a RedisStore pointed at a local Redis instance.
    /// These tests are skipped unless `REDIS_URL` is set.
    fn redis_store() -> Option<RedisStore> {
        let url = std::env::var("REDIS_URL").ok()?;
        RedisStore::new(&url, 5, 60).ok()
    }

    #[test]
    fn test_redis_store_basic() {
        let store = match redis_store() {
            Some(s) => s,
            None => return, // skip if no Redis available
        };

        let uid = format!("test-user-{}", uuid::Uuid::new_v4());

        store.add_message(&uid, "Hello".to_string());
        store.add_message(&uid, "World".to_string());

        let history = store.get_history(&uid);
        assert_eq!(history.len(), 2);
        // Chronological order: oldest first
        assert_eq!(history[0], "Hello");
        assert_eq!(history[1], "World");

        store.clear_history(&uid);
        assert!(store.get_history(&uid).is_empty());
    }

    #[test]
    fn test_redis_store_trim() {
        let store = match redis_store() {
            Some(s) => s,
            None => return,
        };

        let uid = format!("test-trim-{}", uuid::Uuid::new_v4());

        for i in 0..8 {
            store.add_message(&uid, format!("msg-{}", i));
        }

        let history = store.get_history(&uid);
        assert_eq!(history.len(), 5, "should be trimmed to max_history=5");

        store.clear_history(&uid);
    }
}
