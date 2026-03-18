use crate::channels::Channel;
use crate::config::TelegramConfig;
use crate::types::{ChannelHealth, Message};
use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info};

pub struct TelegramChannel {
    id: String,
    config: TelegramConfig,
    health: Arc<RwLock<ChannelHealth>>,
    http_client: Client,
}

impl TelegramChannel {
    pub fn new(id: String, config: TelegramConfig) -> Self {
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
impl Channel for TelegramChannel {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "telegram"
    }

    async fn start(&mut self) -> Result<()> {
        info!("Starting Telegram channel (webhook mode — routes handled by HTTP server)");
        *self.health.write().await = ChannelHealth::Healthy;
        info!("Telegram channel started");
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        info!("Stopping Telegram channel");
        *self.health.write().await = ChannelHealth::Unhealthy {
            reason: "shutting down".to_string(),
        };
        Ok(())
    }

    async fn send_message(&self, message: Message) -> Result<()> {
        let url = format!(
            "https://api.telegram.org/bot{}/sendMessage",
            self.config.bot_token
        );

        let chat_id: i64 = message
            .user_id
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid Telegram chat_id: {}", message.user_id))?;

        let body = serde_json::json!({
            "chat_id": chat_id,
            "text": message.content,
            "parse_mode": "Markdown"
        });

        let response = self
            .http_client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Telegram send failed: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            error!("Telegram API error {}: {}", status, text);
            return Err(anyhow::anyhow!(
                "Telegram API returned {}: {}",
                status,
                text
            ));
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
pub struct TelegramUpdate {
    pub update_id: i64,
    pub message: Option<TelegramMessage>,
}

#[derive(Debug, serde::Deserialize)]
pub struct TelegramMessage {
    pub message_id: i64,
    pub from: Option<TelegramUser>,
    pub chat: TelegramChat,
    pub date: i64,
    pub text: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
pub struct TelegramUser {
    pub id: i64,
    pub first_name: String,
    pub last_name: Option<String>,
    pub username: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
pub struct TelegramChat {
    pub id: i64,
    #[serde(rename = "type")]
    pub chat_type: String,
}

/// Convert a Telegram update into a `Message`, returning None for non-text or missing updates.
pub fn parse_update(update: &TelegramUpdate) -> Option<Message> {
    let tg_msg = update.message.as_ref()?;
    let text = tg_msg.text.as_ref()?.clone();

    let chat_id = tg_msg.chat.id.to_string();

    let username = tg_msg
        .from
        .as_ref()
        .map(|u| {
            u.username.clone().unwrap_or_else(|| {
                let mut name = u.first_name.clone();
                if let Some(ref last) = u.last_name {
                    name.push(' ');
                    name.push_str(last);
                }
                name
            })
        })
        .unwrap_or_else(|| chat_id.clone());

    Some(Message::new("telegram-main", chat_id, username, text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_update_json() -> &'static str {
        r#"{
            "update_id": 100,
            "message": {
                "message_id": 1,
                "from": {"id": 42, "first_name": "Alice", "last_name": "Smith", "username": "alicesmith"},
                "chat": {"id": 42, "type": "private"},
                "date": 1700000000,
                "text": "Hello, bot!"
            }
        }"#
    }

    #[test]
    fn test_parse_update_text_message() {
        let update: TelegramUpdate = serde_json::from_str(sample_update_json()).unwrap();
        let msg = parse_update(&update).unwrap();

        assert_eq!(msg.channel_id, "telegram-main");
        assert_eq!(msg.user_id, "42");
        assert_eq!(msg.username, "alicesmith");
        assert_eq!(msg.content, "Hello, bot!");
    }

    #[test]
    fn test_parse_update_no_text_returns_none() {
        let json = r#"{
            "update_id": 101,
            "message": {
                "message_id": 2,
                "from": {"id": 42, "first_name": "Alice"},
                "chat": {"id": 42, "type": "private"},
                "date": 1700000001
            }
        }"#;
        let update: TelegramUpdate = serde_json::from_str(json).unwrap();
        assert!(parse_update(&update).is_none());
    }

    #[test]
    fn test_parse_update_no_message_returns_none() {
        let json = r#"{"update_id": 102}"#;
        let update: TelegramUpdate = serde_json::from_str(json).unwrap();
        assert!(parse_update(&update).is_none());
    }

    #[test]
    fn test_parse_update_username_fallback_to_full_name() {
        let json = r#"{
            "update_id": 103,
            "message": {
                "message_id": 3,
                "from": {"id": 99, "first_name": "Bob", "last_name": "Jones"},
                "chat": {"id": 99, "type": "private"},
                "date": 1700000002,
                "text": "Hi"
            }
        }"#;
        let update: TelegramUpdate = serde_json::from_str(json).unwrap();
        let msg = parse_update(&update).unwrap();
        assert_eq!(msg.username, "Bob Jones");
    }
}
