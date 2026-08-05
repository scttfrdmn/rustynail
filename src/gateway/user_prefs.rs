use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// One user's stored preferences.
///
/// Every field is optional and independent: setting a timezone must not clear a
/// preferred channel, and vice versa, so the store reads-modifies-writes a struct
/// rather than replacing a single value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Preferences {
    /// Channel responses are routed to, regardless of where the message arrived.
    pub preferred_channel_id: Option<String>,
    /// IANA timezone name deadlines resolve in — `"America/New_York"`.
    ///
    /// Deadlines are a **price control** in quarry's model: `Deferrable()` turns
    /// slack into cheaper inference, so "by tonight" resolved in the wrong zone
    /// buys the wrong amount of compute. Unset falls back to the operator default
    /// and then UTC, and which step supplied it is disclosed to the sender — see
    /// [`crate::quarry::SenderTimezone`].
    pub timezone: Option<String>,
}

/// Per-user preference store.
///
/// When a user's preferred channel is set, the gateway routes responses to that
/// channel regardless of which channel the original message arrived on.
pub struct UserPreferences {
    prefs: Arc<RwLock<HashMap<String, Preferences>>>,
}

impl UserPreferences {
    pub fn new() -> Self {
        Self {
            prefs: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Set the preferred response channel, leaving other preferences intact.
    pub async fn set(&self, user_id: &str, channel_id: &str) {
        let mut prefs = self.prefs.write().await;
        prefs
            .entry(user_id.to_string())
            .or_default()
            .preferred_channel_id = Some(channel_id.to_string());
    }

    /// The preferred response channel, if set.
    pub async fn get(&self, user_id: &str) -> Option<String> {
        let prefs = self.prefs.read().await;
        prefs.get(user_id)?.preferred_channel_id.clone()
    }

    /// Set the timezone deadlines resolve in, leaving other preferences intact.
    pub async fn set_timezone(&self, user_id: &str, timezone: &str) {
        let mut prefs = self.prefs.write().await;
        prefs.entry(user_id.to_string()).or_default().timezone = Some(timezone.to_string());
    }

    /// The stored timezone, if set.
    pub async fn timezone(&self, user_id: &str) -> Option<String> {
        let prefs = self.prefs.read().await;
        prefs.get(user_id)?.timezone.clone()
    }

    /// Everything stored for a user.
    pub async fn all(&self, user_id: &str) -> Preferences {
        let prefs = self.prefs.read().await;
        prefs.get(user_id).cloned().unwrap_or_default()
    }

    /// Drop every preference for a user.
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

    #[tokio::test]
    async fn timezone_is_stored_and_read_back() {
        let prefs = UserPreferences::new();
        assert!(prefs.timezone("alice").await.is_none());
        prefs.set_timezone("alice", "America/New_York").await;
        assert_eq!(prefs.timezone("alice").await.unwrap(), "America/New_York");
    }

    #[tokio::test]
    async fn setting_one_preference_does_not_clear_the_other() {
        // The bug this guards: a store holding a single value per user loses the
        // channel preference when a timezone is set, silently re-routing a user's
        // replies to wherever their next message happens to come from.
        let prefs = UserPreferences::new();
        prefs.set("alice", "whatsapp-1").await;
        prefs.set_timezone("alice", "Asia/Tokyo").await;
        assert_eq!(prefs.get("alice").await.unwrap(), "whatsapp-1");

        prefs.set("alice", "discord-2").await;
        assert_eq!(prefs.timezone("alice").await.unwrap(), "Asia/Tokyo");

        assert_eq!(
            prefs.all("alice").await,
            Preferences {
                preferred_channel_id: Some("discord-2".to_string()),
                timezone: Some("Asia/Tokyo".to_string()),
            }
        );
    }

    #[tokio::test]
    async fn clear_drops_every_preference() {
        let prefs = UserPreferences::new();
        prefs.set("alice", "whatsapp-1").await;
        prefs.set_timezone("alice", "Asia/Tokyo").await;
        prefs.clear("alice").await;
        assert_eq!(prefs.all("alice").await, Preferences::default());
    }

    #[tokio::test]
    async fn users_do_not_share_preferences() {
        let prefs = UserPreferences::new();
        prefs.set_timezone("alice", "Asia/Tokyo").await;
        assert!(prefs.timezone("bob").await.is_none());
    }
}
