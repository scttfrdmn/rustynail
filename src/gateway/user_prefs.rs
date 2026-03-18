use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Per-user preferred response channel store.
///
/// When a user's preferred channel is set, the gateway routes responses to
/// that channel regardless of which channel the original message arrived on.
pub struct UserPreferences {
    prefs: Arc<RwLock<HashMap<String, String>>>,
}

impl UserPreferences {
    pub fn new() -> Self {
        Self {
            prefs: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn set(&self, user_id: &str, channel_id: &str) {
        let mut prefs = self.prefs.write().await;
        prefs.insert(user_id.to_string(), channel_id.to_string());
    }

    pub async fn get(&self, user_id: &str) -> Option<String> {
        let prefs = self.prefs.read().await;
        prefs.get(user_id).cloned()
    }

    pub async fn clear(&self, user_id: &str) {
        let mut prefs = self.prefs.write().await;
        prefs.remove(user_id);
    }
}

impl Default for UserPreferences {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_set_and_get() {
        let prefs = UserPreferences::new();
        assert!(prefs.get("alice").await.is_none());
        prefs.set("alice", "whatsapp-1").await;
        assert_eq!(prefs.get("alice").await.unwrap(), "whatsapp-1");
    }

    #[tokio::test]
    async fn test_clear() {
        let prefs = UserPreferences::new();
        prefs.set("alice", "whatsapp-1").await;
        prefs.clear("alice").await;
        assert!(prefs.get("alice").await.is_none());
    }
}
