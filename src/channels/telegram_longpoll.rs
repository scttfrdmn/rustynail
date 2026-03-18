use crate::channels::telegram::{parse_update, TelegramUpdate};
use crate::channels::Channel;
use crate::config::TelegramConfig;
use crate::types::{ChannelHealth, Message};
use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, RwLock};
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

const NAME: &str = "telegram-longpoll";
const POLL_TIMEOUT_SECS: u64 = 30;
const BACKOFF_SECS: u64 = 5;

/// Telegram channel adapter that uses long-polling (`getUpdates`) instead of
/// webhooks. Use this when the gateway runs without a public HTTPS endpoint.
///
/// `start()` spawns a `tokio::spawn` loop that calls `getUpdates?timeout=30&offset=<n>`
/// continuously. Send is identical to the webhook channel adapter.
pub struct TelegramLongPollChannel {
    id: String,
    config: TelegramConfig,
    health: Arc<RwLock<ChannelHealth>>,
    http_client: Client,
    message_tx: mpsc::UnboundedSender<Message>,
    poll_task: Option<JoinHandle<()>>,
}

impl TelegramLongPollChannel {
    pub fn new(
        id: String,
        config: TelegramConfig,
        message_tx: mpsc::UnboundedSender<Message>,
    ) -> Self {
        Self {
            id,
            config,
            health: Arc::new(RwLock::new(ChannelHealth::Unhealthy {
                reason: "not started".to_string(),
            })),
            http_client: Client::builder()
                .timeout(Duration::from_secs(POLL_TIMEOUT_SECS + 10))
                .build()
                .unwrap_or_default(),
            message_tx,
            poll_task: None,
        }
    }
}

#[async_trait]
impl Channel for TelegramLongPollChannel {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        NAME
    }

    async fn start(&mut self) -> Result<()> {
        info!("Starting Telegram long-poll channel (id={})", self.id);
        *self.health.write().await = ChannelHealth::Healthy;

        let bot_token = self.config.bot_token.clone();
        let message_tx = self.message_tx.clone();
        let health = self.health.clone();

        let handle = tokio::spawn(async move {
            let client = Client::builder()
                .timeout(Duration::from_secs(POLL_TIMEOUT_SECS + 10))
                .build()
                .unwrap_or_default();
            let mut offset: i64 = 0;

            info!("Telegram long-poll loop started");
            loop {
                let url = format!(
                    "https://api.telegram.org/bot{}/getUpdates?timeout={}&offset={}",
                    bot_token, POLL_TIMEOUT_SECS, offset
                );

                match client.get(&url).send().await {
                    Ok(resp) => {
                        // Mark healthy on first successful response
                        *health.write().await = ChannelHealth::Healthy;

                        match resp.json::<serde_json::Value>().await {
                            Ok(body) => {
                                if !body["ok"].as_bool().unwrap_or(false) {
                                    let desc =
                                        body["description"].as_str().unwrap_or("unknown error");
                                    error!("Telegram getUpdates error: {}", desc);
                                    tokio::time::sleep(Duration::from_secs(BACKOFF_SECS)).await;
                                    continue;
                                }

                                if let Some(updates) = body["result"].as_array() {
                                    for update_val in updates {
                                        match serde_json::from_value::<TelegramUpdate>(
                                            update_val.clone(),
                                        ) {
                                            Ok(update) => {
                                                // Advance offset past this update
                                                offset = update.update_id + 1;
                                                if let Some(msg) = parse_update(&update) {
                                                    if let Err(e) = message_tx.send(msg) {
                                                        warn!(
                                                            "Failed to enqueue Telegram update: {}",
                                                            e
                                                        );
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                warn!("Failed to parse Telegram update: {}", e);
                                                // Still advance offset to avoid reprocessing
                                                if let Some(id) = update_val["update_id"].as_i64() {
                                                    offset = id + 1;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                error!("Failed to parse Telegram getUpdates response: {}", e);
                                tokio::time::sleep(Duration::from_secs(BACKOFF_SECS)).await;
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Telegram long-poll request failed: {}", e);
                        *health.write().await = ChannelHealth::Degraded {
                            reason: format!("poll error: {}", e),
                        };
                        tokio::time::sleep(Duration::from_secs(BACKOFF_SECS)).await;
                    }
                }
            }
        });

        self.poll_task = Some(handle);
        info!("Telegram long-poll channel started");
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        info!("Stopping Telegram long-poll channel");
        if let Some(task) = self.poll_task.take() {
            task.abort();
        }
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
