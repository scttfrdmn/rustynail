use crate::channels::Channel;
use crate::config::{WebhookConfig, WebhookEndpoint};
use crate::types::{ChannelHealth, Message};
use anyhow::Result;
use async_trait::async_trait;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

type HmacSha256 = Hmac<Sha256>;

const NAME: &str = "webhook";

pub struct WebhookChannel {
    id: String,
    config: WebhookConfig,
    health: Arc<RwLock<ChannelHealth>>,
}

impl WebhookChannel {
    pub fn new(id: String, config: WebhookConfig) -> Self {
        Self {
            id,
            config,
            health: Arc::new(RwLock::new(ChannelHealth::Unhealthy {
                reason: "not started".to_string(),
            })),
        }
    }

    pub fn endpoints(&self) -> &[WebhookEndpoint] {
        &self.config.endpoints
    }
}

#[async_trait]
impl Channel for WebhookChannel {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        NAME
    }

    async fn start(&mut self) -> Result<()> {
        info!(
            "Starting generic webhook channel ({} endpoint(s))",
            self.config.endpoints.len()
        );
        *self.health.write().await = ChannelHealth::Healthy;
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        *self.health.write().await = ChannelHealth::Unhealthy {
            reason: "stopped".to_string(),
        };
        Ok(())
    }

    /// Webhook channels are receive-only; outbound is not supported.
    async fn send_message(&self, _message: Message) -> Result<()> {
        warn!("WebhookChannel: send_message is not supported (receive-only channel)");
        Ok(())
    }

    fn health(&self) -> ChannelHealth {
        self.health.blocking_read().clone()
    }

    fn is_running(&self) -> bool {
        matches!(self.health.blocking_read().clone(), ChannelHealth::Healthy)
    }
}

// ── Parsing helpers ───────────────────────────────────────────────────────────

/// Verify HMAC-SHA256 signature sent in `X-Webhook-Signature`.
pub fn verify_webhook_signature(secret: &str, body: &[u8], signature: &str) -> Result<()> {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|e| anyhow::anyhow!("HMAC key error: {}", e))?;
    mac.update(body);
    let computed = hex::encode(mac.finalize().into_bytes());
    if computed != signature {
        return Err(anyhow::anyhow!("Webhook signature verification failed"));
    }
    Ok(())
}

/// Extract a text value from a JSON body using a JSONPath expression.
///
/// Returns `None` if the path doesn't match or the value is not a string.
pub fn extract_jsonpath(body: &[u8], path: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_slice(body).ok()?;
    let json_str = serde_json::to_string(&json).ok()?;
    let finder = jsonpath_rust::JsonPathFinder::from_str(&json_str, path).ok()?;
    let result = finder.find();
    // find() returns a JsonValue (serde_json::Value); extract first array element string
    result.as_array()?.first()?.as_str().map(|s| s.to_string())
}

/// Parse a generic inbound webhook body into a [`Message`].
///
/// If `endpoint.extract_text` is set, attempts JSONPath extraction first;
/// falls back to the raw body text.
pub fn parse_webhook_body(
    channel_id: &str,
    endpoint: &WebhookEndpoint,
    body: &[u8],
) -> Option<Message> {
    let text = if let Some(ref jpath) = endpoint.extract_text {
        extract_jsonpath(body, jpath)
            .or_else(|| String::from_utf8(body.to_vec()).ok())
    } else {
        String::from_utf8(body.to_vec()).ok()
    }?;

    if text.is_empty() {
        return None;
    }

    Some(Message::new(
        channel_id.to_string(),
        endpoint.user_id.clone(),
        endpoint.user_id.clone(),
        text,
    ))
}
