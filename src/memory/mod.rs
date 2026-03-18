pub mod postgres;
pub mod redis;
pub mod sqlite;
pub mod summarizer;
pub mod vector;

pub use postgres::PostgresStore;
pub use redis::RedisStore;
pub use sqlite::SqliteStore;
pub use summarizer::MemorySummarizer;
pub use vector::VectorMemoryStore;

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// MemoryStore manages conversation history for users
pub trait MemoryStore: Send + Sync {
    /// Get conversation history for a user
    fn get_history(&self, user_id: &str) -> Vec<String>;

    /// Add a message to user's history
    fn add_message(&self, user_id: &str, message: String);

    /// Clear history for a user
    fn clear_history(&self, user_id: &str);

    /// Get the maximum number of messages to keep
    fn max_history(&self) -> usize;
}

/// InMemoryStore is a simple in-memory implementation of MemoryStore
#[derive(Clone)]
pub struct InMemoryStore {
    histories: Arc<RwLock<HashMap<String, Vec<String>>>>,
    max_history: usize,
}

impl InMemoryStore {
    pub fn new(max_history: usize) -> Self {
        Self {
            histories: Arc::new(RwLock::new(HashMap::new())),
            max_history,
        }
    }
}

impl MemoryStore for InMemoryStore {
    fn get_history(&self, user_id: &str) -> Vec<String> {
        let histories = self.histories.read().unwrap();
        histories.get(user_id).cloned().unwrap_or_default()
    }

    fn add_message(&self, user_id: &str, message: String) {
        let mut histories = self.histories.write().unwrap();
        let history = histories.entry(user_id.to_string()).or_default();

        history.push(message);

        // Keep only the last N messages
        if history.len() > self.max_history {
            *history = history.split_off(history.len() - self.max_history);
        }
    }

    fn clear_history(&self, user_id: &str) {
        let mut histories = self.histories.write().unwrap();
        histories.remove(user_id);
    }

    fn max_history(&self) -> usize {
        self.max_history
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_in_memory_store() {
        let store = InMemoryStore::new(3);

        // Add messages
        store.add_message("user1", "Message 1".to_string());
        store.add_message("user1", "Message 2".to_string());
        store.add_message("user1", "Message 3".to_string());

        // Check history
        let history = store.get_history("user1");
        assert_eq!(history.len(), 3);

        // Add one more - should trim to max_history
        store.add_message("user1", "Message 4".to_string());
        let history = store.get_history("user1");
        assert_eq!(history.len(), 3);
        assert_eq!(history[0], "Message 2");
        assert_eq!(history[2], "Message 4");

        // Clear history
        store.clear_history("user1");
        let history = store.get_history("user1");
        assert_eq!(history.len(), 0);
    }

    #[test]
    fn test_multiple_users() {
        let store = InMemoryStore::new(10);

        store.add_message("user1", "User1 Message 1".to_string());
        store.add_message("user2", "User2 Message 1".to_string());
        store.add_message("user1", "User1 Message 2".to_string());

        let history1 = store.get_history("user1");
        let history2 = store.get_history("user2");

        assert_eq!(history1.len(), 2);
        assert_eq!(history2.len(), 1);
    }
}
