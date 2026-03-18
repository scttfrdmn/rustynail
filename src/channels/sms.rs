use crate::channels::Channel;
use crate::config::SmsConfig;
use crate::types::{ChannelHealth, Message};
use anyhow::Result;
use async_trait::async_trait;
use hmac::{Hmac, Mac};
use reqwest::Client;
use sha2::Sha256;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

type HmacSha256 = Hmac<Sha256>;

const NAME: &str = "sms";

pub struct SmsChannel {
    id: String,
    config: SmsConfig,
    health: Arc<RwLock<ChannelHealth>>,
    http_client: Client,
}

impl SmsChannel {
    pub fn new(id: String, config: SmsConfig) -> Self {
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
impl Channel for SmsChannel {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        NAME
    }

    async fn start(&mut self) -> Result<()> {
        info!("Starting SMS channel (Twilio webhook mode)");
        *self.health.write().await = ChannelHealth::Healthy;
        info!("SMS channel started");
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        info!("Stopping SMS channel");
        *self.health.write().await = ChannelHealth::Unhealthy {
            reason: "stopped".to_string(),
        };
        Ok(())
    }

    async fn send_message(&self, message: Message) -> Result<()> {
        let url = format!(
            "https://api.twilio.com/2010-04-01/Accounts/{}/Messages.json",
            self.config.auth.account_sid
        );

        let params = [
            ("To", message.user_id.as_str()),
            ("From", self.config.auth.from_number.as_str()),
            ("Body", message.content.as_str()),
        ];

        let resp = self
            .http_client
            .post(&url)
            .basic_auth(
                &self.config.auth.account_sid,
                Some(&self.config.auth.auth_token),
            )
            .form(&params)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Twilio send error: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "Twilio API error {}: {}",
                status,
                body
            ));
        }

        Ok(())
    }

    fn health(&self) -> ChannelHealth {
        self.health.blocking_read().clone()
    }

    fn is_running(&self) -> bool {
        matches!(self.health.blocking_read().clone(), ChannelHealth::Healthy)
    }
}

// ── Webhook parsing helpers ───────────────────────────────────────────────────

/// Verify Twilio's HMAC-SHA256 webhook signature.
///
/// `url` — the full URL Twilio posted to (must match exactly).
/// `params` — sorted form parameters from the request body.
/// `signature` — value of the `X-Twilio-Signature` header.
/// `auth_token` — Twilio account auth token.
pub fn verify_twilio_signature(
    url: &str,
    params: &[(String, String)],
    signature: &str,
    auth_token: &str,
) -> Result<()> {
    // Build the string to sign: url + sorted params (key+value, no separator)
    let mut sorted = params.to_vec();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let mut to_sign = url.to_string();
    for (k, v) in &sorted {
        to_sign.push_str(k);
        to_sign.push_str(v);
    }

    let mut mac = HmacSha256::new_from_slice(auth_token.as_bytes())
        .map_err(|e| anyhow::anyhow!("HMAC key error: {}", e))?;
    mac.update(to_sign.as_bytes());
    let computed = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        mac.finalize().into_bytes(),
    );

    if computed != signature {
        return Err(anyhow::anyhow!("Twilio signature verification failed"));
    }
    Ok(())
}

/// Parse a Twilio SMS webhook form body into a [`Message`].
///
/// Twilio sends `From`, `To`, `Body`, `MessageSid`, etc. as URL-encoded form fields.
pub fn parse_sms_webhook(channel_id: &str, form: &[(String, String)]) -> Option<Message> {
    let get = |key: &str| -> Option<String> {
        form.iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    };

    let from = get("From")?;
    let body = get("Body")?;

    if body.is_empty() {
        warn!("Received empty SMS body from {}", from);
        return None;
    }

    info!("SMS received from {}: {} chars", from, body.len());
    Some(Message::new(
        channel_id.to_string(),
        from.clone(),
        from,
        body,
    ))
}
