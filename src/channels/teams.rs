use crate::channels::Channel;
use crate::config::TeamsConfig;
use std::collections::HashMap;
use crate::types::{ChannelHealth, Message};
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::{error, info, warn};

// ── Token cache ───────────────────────────────────────────────────────────────

struct TeamsTokenCache {
    token: String,
    expires_at: Instant,
}

/// Obtains and caches OAuth2 client-credentials tokens from the Bot Framework
/// login endpoint. The cached token is refreshed 60 seconds before expiry.
struct TokenManager {
    app_id: String,
    app_password: String,
    cache: Mutex<Option<TeamsTokenCache>>,
}

impl TokenManager {
    fn new(app_id: impl Into<String>, app_password: impl Into<String>) -> Self {
        Self {
            app_id: app_id.into(),
            app_password: app_password.into(),
            cache: Mutex::new(None),
        }
    }

    async fn get_token(&self) -> Result<String> {
        let mut guard = self.cache.lock().await;

        // Return cached token if still valid (with 60s buffer)
        if let Some(ref cached) = *guard {
            if cached.expires_at > Instant::now() + Duration::from_secs(60) {
                return Ok(cached.token.clone());
            }
        }

        // Fetch a new token
        let token_url =
            "https://login.microsoftonline.com/botframework.com/oauth2/v2.0/token";

        let client = reqwest::Client::new();
        let mut form: HashMap<&str, &str> = HashMap::new();
        form.insert("grant_type", "client_credentials");
        form.insert("client_id", &self.app_id);
        form.insert("client_secret", &self.app_password);
        form.insert("scope", "https://api.botframework.com/.default");

        let resp = client
            .post(token_url)
            .form(&form)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("token request failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "token endpoint returned {}: {}",
                status,
                body
            ));
        }

        #[derive(Deserialize)]
        struct TokenResponse {
            access_token: String,
            expires_in: u64,
        }

        let token_resp: TokenResponse = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("failed to parse token response: {}", e))?;

        let expires_at =
            Instant::now() + Duration::from_secs(token_resp.expires_in);

        let token = token_resp.access_token.clone();
        *guard = Some(TeamsTokenCache {
            token: token_resp.access_token,
            expires_at,
        });

        Ok(token)
    }
}

// ── Bot Framework Activity types ──────────────────────────────────────────────

#[derive(Debug, Deserialize, Clone)]
pub struct TeamsFrom {
    pub id: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct TeamsConversation {
    pub id: String,
}

/// Bot Framework Activity (subset of fields we care about).
#[derive(Debug, Deserialize)]
pub struct TeamsActivity {
    #[serde(rename = "type")]
    pub activity_type: String,
    pub from: Option<TeamsFrom>,
    pub text: Option<String>,
    #[serde(rename = "serviceUrl")]
    pub service_url: Option<String>,
    pub conversation: Option<TeamsConversation>,
    pub id: Option<String>,
}

/// Extract a `Message` from a Bot Framework Activity, or return `None` if
/// the activity isn't a user message.
pub fn parse_activity(channel_id: &str, activity: &TeamsActivity) -> Option<Message> {
    if activity.activity_type != "message" {
        return None;
    }
    let text = activity.text.as_deref()?.trim().to_string();
    if text.is_empty() {
        return None;
    }

    let from = activity.from.as_ref()?;
    let mut msg = Message::new(
        channel_id.to_string(),
        from.id.clone(),
        from.name.clone(),
        text,
    );

    // Stash Teams-specific routing metadata so the outbound send knows where
    // to deliver the reply.
    msg.metadata = serde_json::json!({
        "service_url": activity.service_url.as_deref().unwrap_or(""),
        "conversation_id": activity.conversation.as_ref().map(|c| c.id.as_str()).unwrap_or(""),
        "activity_id": activity.id.as_deref().unwrap_or("")
    });

    Some(msg)
}

// ── Channel implementation ────────────────────────────────────────────────────

pub struct TeamsChannel {
    id: String,
    config: TeamsConfig,
    token_manager: Arc<TokenManager>,
    running: bool,
}

impl TeamsChannel {
    pub fn new(id: impl Into<String>, config: TeamsConfig) -> Self {
        let token_manager = Arc::new(TokenManager::new(
            config.auth.app_id.clone(),
            config.auth.app_password.clone(),
        ));
        Self {
            id: id.into(),
            config,
            token_manager,
            running: false,
        }
    }
}

#[async_trait]
impl Channel for TeamsChannel {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "teams"
    }

    async fn start(&mut self) -> Result<()> {
        info!(
            "Teams channel '{}' started (webhook mode — POST /channels/teams/messages)",
            self.id
        );
        self.running = true;
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        self.running = false;
        Ok(())
    }

    /// Send a reply to Teams via the Bot Framework REST API.
    ///
    /// The outbound `Message` must carry Teams routing metadata set by
    /// `parse_activity()` above (service_url, conversation_id, activity_id).
    async fn send_message(&self, msg: Message) -> Result<()> {
        let service_url = match msg.metadata["service_url"].as_str() {
            Some(u) if !u.is_empty() => u.to_string(),
            _ => {
                warn!(
                    "Teams channel: send_message missing service_url in metadata for user {}",
                    msg.user_id
                );
                return Ok(()); // best-effort; no crash
            }
        };
        let conversation_id = msg.metadata["conversation_id"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let activity_id = msg.metadata["activity_id"]
            .as_str()
            .unwrap_or("")
            .to_string();

        let token = self.token_manager.get_token().await.map_err(|e| {
            error!("Teams: failed to get bearer token: {}", e);
            e
        })?;

        let url = format!(
            "{}/v3/conversations/{}/activities/{}",
            service_url.trim_end_matches('/'),
            conversation_id,
            activity_id
        );

        #[derive(Serialize)]
        struct ReplyBody<'a> {
            #[serde(rename = "type")]
            activity_type: &'a str,
            text: &'a str,
        }

        let client = reqwest::Client::new();
        let resp = client
            .post(&url)
            .bearer_auth(&token)
            .json(&ReplyBody {
                activity_type: "message",
                text: &msg.content,
            })
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Teams send failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            error!("Teams: reply returned {}: {}", status, body);
        }

        Ok(())
    }

    fn health(&self) -> ChannelHealth {
        if self.running {
            ChannelHealth::Healthy
        } else {
            ChannelHealth::Unhealthy {
                reason: "channel not started".to_string(),
            }
        }
    }

    fn is_running(&self) -> bool {
        self.running
    }
}
