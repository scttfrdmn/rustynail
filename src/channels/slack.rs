use crate::channels::Channel;
use crate::config::SlackConfig;
use crate::types::{ChannelHealth, Message};
use anyhow::Result;
use async_trait::async_trait;
use hmac::{Hmac, Mac};
use reqwest::Client;
use sha2::Sha256;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info};

type HmacSha256 = Hmac<Sha256>;

pub struct SlackChannel {
    id: String,
    config: SlackConfig,
    health: Arc<RwLock<ChannelHealth>>,
    http_client: Client,
}

impl SlackChannel {
    pub fn new(id: String, config: SlackConfig) -> Self {
        Self {
            id,
            config,
            health: Arc::new(RwLock::new(ChannelHealth::Unhealthy {
                reason: "not started".to_string(),
            })),
            http_client: Client::new(),
        }
    }
}

#[async_trait]
impl Channel for SlackChannel {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "slack"
    }

    async fn start(&mut self) -> Result<()> {
        info!("Starting Slack channel (webhook mode — routes handled by HTTP server)");
        *self.health.write().await = ChannelHealth::Healthy;
        info!("Slack channel started");
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        info!("Stopping Slack channel");
        *self.health.write().await = ChannelHealth::Unhealthy {
            reason: "shutting down".to_string(),
        };
        Ok(())
    }

    async fn send_message(&self, message: Message) -> Result<()> {
        let body = serde_json::json!({
            "channel": message.channel_id,
            "text": message.content
        });

        let response = self
            .http_client
            .post("https://slack.com/api/chat.postMessage")
            .bearer_auth(&self.config.bot_token)
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Slack send failed: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            error!("Slack API HTTP error {}: {}", status, text);
            return Err(anyhow::anyhow!("Slack API returned {}: {}", status, text));
        }

        let json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Slack response parse error: {}", e))?;

        if json["ok"] != true {
            let err = json["error"].as_str().unwrap_or("unknown");
            error!("Slack chat.postMessage error: {}", err);
            return Err(anyhow::anyhow!("Slack chat.postMessage failed: {}", err));
        }

        Ok(())
    }

    fn health(&self) -> ChannelHealth {
        self.health.blocking_read().clone()
    }

    fn is_running(&self) -> bool {
        self.health().is_operational()
    }
}

// ── Webhook payload types ──────────────────────────────────────────────────────

#[derive(Debug, serde::Deserialize)]
#[serde(tag = "type")]
pub enum SlackWebhookPayload {
    #[serde(rename = "url_verification")]
    UrlVerification { challenge: String },
    #[serde(rename = "event_callback")]
    EventCallback {
        event: SlackEvent,
        event_id: String,
        team_id: String,
    },
}

#[derive(Debug, serde::Deserialize)]
#[serde(tag = "type")]
pub enum SlackEvent {
    #[serde(rename = "message")]
    Message {
        channel: String,
        user: Option<String>,
        text: Option<String>,
        ts: String,
        thread_ts: Option<String>,
        bot_id: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

/// Verify Slack request signature using HMAC-SHA256.
///
/// Slack signs requests as: `v0={hex(hmac_sha256(signing_secret, "v0:{timestamp}:{body}"))}`.
pub fn verify_slack_signature(
    signing_secret: &str,
    timestamp: &str,
    body: &[u8],
    signature: &str,
) -> Result<()> {
    let base = format!("v0:{}:{}", timestamp, String::from_utf8_lossy(body));

    let mut mac = HmacSha256::new_from_slice(signing_secret.as_bytes())
        .map_err(|e| anyhow::anyhow!("HMAC key error: {}", e))?;
    mac.update(base.as_bytes());
    let result = mac.finalize();
    let computed = format!("v0={}", hex::encode(result.into_bytes()));

    if computed != signature {
        return Err(anyhow::anyhow!("Slack signature mismatch"));
    }

    Ok(())
}

/// Convert a Slack webhook payload into a `Message`, returning None for non-message events.
pub fn parse_event(payload: &SlackWebhookPayload) -> Option<Message> {
    match payload {
        SlackWebhookPayload::EventCallback { event, .. } => match event {
            SlackEvent::Message {
                channel,
                user,
                text,
                ts,
                thread_ts,
                bot_id,
            } => {
                // Ignore bot messages
                if bot_id.is_some() {
                    return None;
                }

                let text = text.as_ref()?.clone();
                let user_id = user.as_ref()?.clone();
                let thread_id = thread_ts.clone().or_else(|| Some(ts.clone()));

                let mut msg = Message::new(channel.as_str(), user_id.clone(), user_id, text);
                msg.thread_id = thread_id;
                Some(msg)
            }
            SlackEvent::Unknown => None,
        },
        SlackWebhookPayload::UrlVerification { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_event_message() {
        let json = r#"{
            "type": "event_callback",
            "event_id": "Ev123",
            "team_id": "T123",
            "event": {
                "type": "message",
                "channel": "C123",
                "user": "U456",
                "text": "Hello from Slack",
                "ts": "1700000000.000001"
            }
        }"#;
        let payload: SlackWebhookPayload = serde_json::from_str(json).unwrap();
        let msg = parse_event(&payload).unwrap();
        assert_eq!(msg.channel_id, "C123");
        assert_eq!(msg.user_id, "U456");
        assert_eq!(msg.content, "Hello from Slack");
    }

    #[test]
    fn test_parse_event_url_verification_returns_none() {
        let json = r#"{"type": "url_verification", "challenge": "abc123"}"#;
        let payload: SlackWebhookPayload = serde_json::from_str(json).unwrap();
        assert!(parse_event(&payload).is_none());
    }

    #[test]
    fn test_parse_event_bot_id_ignored() {
        let json = r#"{
            "type": "event_callback",
            "event_id": "Ev999",
            "team_id": "T123",
            "event": {
                "type": "message",
                "channel": "C123",
                "user": "U789",
                "text": "I am a bot",
                "ts": "1700000001.000001",
                "bot_id": "B123"
            }
        }"#;
        let payload: SlackWebhookPayload = serde_json::from_str(json).unwrap();
        assert!(parse_event(&payload).is_none());
    }

    #[test]
    fn test_verify_slack_signature_valid() {
        let secret = "8f742231b10e8888abcd99yyyzzz85a5";
        let timestamp = "1531420618";
        let body = b"token=xyzz0WbapA4vBCDEFasx0q6G&team_id=T1DC2JH3J&team_domain=testteamnow";
        // Compute expected signature
        let base = format!("v0:{}:{}", timestamp, String::from_utf8_lossy(body));
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(base.as_bytes());
        let result = mac.finalize();
        let expected = format!("v0={}", hex::encode(result.into_bytes()));

        assert!(verify_slack_signature(secret, timestamp, body, &expected).is_ok());
    }

    #[test]
    fn test_verify_slack_signature_invalid() {
        let result = verify_slack_signature("secret", "12345", b"body", "v0=badsignature");
        assert!(result.is_err());
    }
}
