use crate::channels::Channel;
use crate::config::WhatsAppConfig;
use crate::types::{ChannelHealth, Message};
use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info};

pub struct WhatsAppChannel {
    id: String,
    config: WhatsAppConfig,
    health: Arc<RwLock<ChannelHealth>>,
    http_client: Client,
}

impl WhatsAppChannel {
    pub fn new(id: String, config: WhatsAppConfig) -> Self {
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
impl Channel for WhatsAppChannel {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "whatsapp"
    }

    async fn start(&mut self) -> Result<()> {
        info!("Starting WhatsApp channel (webhook mode — routes handled by HTTP server)");
        *self.health.write().await = ChannelHealth::Healthy;
        info!("WhatsApp channel started");
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        info!("Stopping WhatsApp channel");
        *self.health.write().await = ChannelHealth::Unhealthy {
            reason: "shutting down".to_string(),
        };
        Ok(())
    }

    async fn send_message(&self, message: Message) -> Result<()> {
        let url = format!(
            "https://graph.facebook.com/v18.0/{}/messages",
            self.config.phone_number_id
        );

        let body = serde_json::json!({
            "messaging_product": "whatsapp",
            "to": message.user_id,
            "type": "text",
            "text": { "body": message.content }
        });

        let response = self
            .http_client
            .post(&url)
            .bearer_auth(&self.config.access_token)
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("WhatsApp send failed: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            error!("WhatsApp API error {}: {}", status, text);
            return Err(anyhow::anyhow!(
                "WhatsApp API returned {}: {}",
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

/// Parsed representation of a WhatsApp Cloud API webhook payload.
#[derive(Debug, serde::Deserialize)]
pub struct WebhookBody {
    pub object: String,
    pub entry: Vec<WebhookEntry>,
}

#[derive(Debug, serde::Deserialize)]
pub struct WebhookEntry {
    pub id: String,
    pub changes: Vec<WebhookChange>,
}

#[derive(Debug, serde::Deserialize)]
pub struct WebhookChange {
    pub value: WebhookChangeValue,
    pub field: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct WebhookChangeValue {
    pub messaging_product: Option<String>,
    pub contacts: Option<Vec<WebhookContact>>,
    pub messages: Option<Vec<WebhookMessage>>,
}

#[derive(Debug, serde::Deserialize)]
pub struct WebhookContact {
    pub profile: WebhookProfile,
    pub wa_id: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct WebhookProfile {
    pub name: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct WebhookMessage {
    pub from: String,
    pub id: String,
    pub timestamp: String,
    #[serde(rename = "type")]
    pub msg_type: String,
    pub text: Option<WebhookText>,
}

#[derive(Debug, serde::Deserialize)]
pub struct WebhookText {
    pub body: String,
}

/// Extract zero or more `Message` values from a raw WhatsApp webhook payload.
pub fn parse_webhook(channel_id: &str, body: &WebhookBody) -> Vec<Message> {
    let mut messages = Vec::new();

    for entry in &body.entry {
        for change in &entry.changes {
            if change.field != "messages" {
                continue;
            }
            let value = &change.value;
            if let Some(wmsgs) = &value.messages {
                for wmsg in wmsgs {
                    if wmsg.msg_type != "text" {
                        continue;
                    }
                    let text = match &wmsg.text {
                        Some(t) => t.body.clone(),
                        None => continue,
                    };
                    let username = value
                        .contacts
                        .as_ref()
                        .and_then(|cs| cs.iter().find(|c| c.wa_id == wmsg.from))
                        .map(|c| c.profile.name.clone())
                        .unwrap_or_else(|| wmsg.from.clone());

                    messages.push(Message::new(channel_id, wmsg.from.clone(), username, text));
                }
            }
        }
    }

    messages
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_webhook_json() -> &'static str {
        r#"{
            "object": "whatsapp_business_account",
            "entry": [{
                "id": "123",
                "changes": [{
                    "value": {
                        "messaging_product": "whatsapp",
                        "contacts": [{"profile": {"name": "Alice"}, "wa_id": "14155551234"}],
                        "messages": [{
                            "from": "14155551234",
                            "id": "wamid.xxx",
                            "timestamp": "1700000000",
                            "type": "text",
                            "text": {"body": "Hello, bot!"}
                        }]
                    },
                    "field": "messages"
                }]
            }]
        }"#
    }

    #[test]
    fn test_parse_webhook_text_message() {
        let body: WebhookBody = serde_json::from_str(sample_webhook_json()).unwrap();
        let messages = parse_webhook("whatsapp-main", &body);

        assert_eq!(messages.len(), 1);
        let msg = &messages[0];
        assert_eq!(msg.user_id, "14155551234");
        assert_eq!(msg.username, "Alice");
        assert_eq!(msg.content, "Hello, bot!");
        assert_eq!(msg.channel_id, "whatsapp-main");
    }

    #[test]
    fn test_parse_webhook_non_message_field_ignored() {
        let json = r#"{
            "object": "whatsapp_business_account",
            "entry": [{
                "id": "123",
                "changes": [{"value": {}, "field": "account_review_update"}]
            }]
        }"#;
        let body: WebhookBody = serde_json::from_str(json).unwrap();
        let messages = parse_webhook("whatsapp-main", &body);
        assert!(messages.is_empty());
    }

    #[test]
    fn test_parse_webhook_non_text_type_ignored() {
        let json = r#"{
            "object": "whatsapp_business_account",
            "entry": [{
                "id": "123",
                "changes": [{
                    "value": {
                        "messaging_product": "whatsapp",
                        "messages": [{
                            "from": "14155551234",
                            "id": "wamid.yyy",
                            "timestamp": "1700000001",
                            "type": "image"
                        }]
                    },
                    "field": "messages"
                }]
            }]
        }"#;
        let body: WebhookBody = serde_json::from_str(json).unwrap();
        let messages = parse_webhook("whatsapp-main", &body);
        assert!(messages.is_empty());
    }

    #[test]
    fn test_message_construction() {
        let body: WebhookBody = serde_json::from_str(sample_webhook_json()).unwrap();
        let messages = parse_webhook("whatsapp-main", &body);
        let msg = &messages[0];
        assert!(!msg.id.is_empty());
        assert!(msg.preferred_channel_id.is_none());
    }
}
