use crate::channels::Channel;
use crate::config::SlackConfig;
use crate::types::{ChannelHealth, Message};
use anyhow::Result;
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio::task::JoinHandle;
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMsg};
use tracing::{error, info, warn};

const NAME: &str = "slack-socket";

pub struct SlackSocketModeChannel {
    id: String,
    config: SlackConfig,
    health: Arc<RwLock<ChannelHealth>>,
    message_tx: mpsc::UnboundedSender<Message>,
    ws_task: Option<JoinHandle<()>>,
    http_client: Client,
}

impl SlackSocketModeChannel {
    pub fn new(
        id: String,
        config: SlackConfig,
        message_tx: mpsc::UnboundedSender<Message>,
    ) -> Self {
        Self {
            id,
            config,
            health: Arc::new(RwLock::new(ChannelHealth::Unhealthy {
                reason: "not started".to_string(),
            })),
            message_tx,
            ws_task: None,
            http_client: Client::new(),
        }
    }
}

#[async_trait]
impl Channel for SlackSocketModeChannel {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        NAME
    }

    async fn start(&mut self) -> Result<()> {
        let app_token = match &self.config.app_token {
            Some(t) => t.clone(),
            None => {
                return Err(anyhow::anyhow!(
                    "Slack Socket Mode requires app_token (xapp-...)"
                ))
            }
        };

        info!("Starting Slack Socket Mode channel");

        let bot_token = self.config.bot_token.clone();
        let tx = self.message_tx.clone();
        let channel_id = self.id.clone();
        let health = self.health.clone();
        let http_client = self.http_client.clone();

        *health.write().await = ChannelHealth::Healthy;

        let task = tokio::spawn(async move {
            socket_mode_loop(app_token, bot_token, http_client, tx, channel_id, health).await;
        });

        self.ws_task = Some(task);
        info!("Slack Socket Mode channel started");
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        info!("Stopping Slack Socket Mode channel");
        if let Some(task) = self.ws_task.take() {
            task.abort();
        }
        *self.health.write().await = ChannelHealth::Unhealthy {
            reason: "stopped".to_string(),
        };
        Ok(())
    }

    async fn send_message(&self, message: Message) -> Result<()> {
        // Send via Slack Web API chat.postMessage
        let url = "https://slack.com/api/chat.postMessage";

        let body = serde_json::json!({
            "channel": message.channel_id,
            "text": message.content,
        });

        let resp = self
            .http_client
            .post(url)
            .bearer_auth(&self.config.bot_token)
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Slack send error: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("Slack API error {}: {}", status, body_text));
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

// ── Socket Mode background loop ───────────────────────────────────────────────

/// Calls `apps.connections.open` and connects to the returned WSS URL.
/// Handles `hello`, `events_api`, and `disconnect` frames.
async fn socket_mode_loop(
    app_token: String,
    bot_token: String,
    http_client: Client,
    tx: mpsc::UnboundedSender<Message>,
    channel_id: String,
    health: Arc<RwLock<ChannelHealth>>,
) {
    let mut reconnect_delay = std::time::Duration::from_secs(1);
    let max_delay = std::time::Duration::from_secs(60);

    loop {
        info!("Slack Socket Mode: connecting…");

        let wss_url = match open_connection(&http_client, &app_token).await {
            Ok(url) => url,
            Err(e) => {
                error!("Slack apps.connections.open error: {}", e);
                *health.write().await = ChannelHealth::Unhealthy {
                    reason: format!("connection open error: {}", e),
                };
                tokio::time::sleep(reconnect_delay).await;
                reconnect_delay = std::cmp::min(reconnect_delay * 2, max_delay);
                continue;
            }
        };

        match connect_async(&wss_url).await {
            Ok((ws_stream, _)) => {
                reconnect_delay = std::time::Duration::from_secs(1);
                *health.write().await = ChannelHealth::Healthy;
                info!("Slack Socket Mode: WebSocket connected");

                let (mut write, mut read) = ws_stream.split();
                let mut should_reconnect = false;

                while let Some(frame) = read.next().await {
                    match frame {
                        Ok(WsMsg::Text(text)) => {
                            let val: Value = match serde_json::from_str(&text) {
                                Ok(v) => v,
                                Err(e) => {
                                    warn!("Slack WS: failed to parse JSON: {}", e);
                                    continue;
                                }
                            };

                            let envelope_id = val["envelope_id"].as_str().unwrap_or("").to_string();
                            let msg_type = val["type"].as_str().unwrap_or("");

                            match msg_type {
                                "hello" => {
                                    info!("Slack Socket Mode: hello received");
                                }
                                "disconnect" => {
                                    info!("Slack Socket Mode: disconnect frame; reconnecting");
                                    should_reconnect = true;
                                    break;
                                }
                                "events_api" => {
                                    // Acknowledge immediately
                                    if !envelope_id.is_empty() {
                                        let ack =
                                            serde_json::json!({ "envelope_id": envelope_id });
                                        let _ = write
                                            .send(WsMsg::Text(ack.to_string()))
                                            .await;
                                    }

                                    // Parse the inner event
                                    if let Some(msg) = parse_socket_event(&val, &channel_id, &bot_token) {
                                        if let Err(e) = tx.send(msg) {
                                            error!("Slack Socket Mode: failed to enqueue: {}", e);
                                        }
                                    }
                                }
                                other => {
                                    // Ack any unknown envelope
                                    if !envelope_id.is_empty() {
                                        let ack =
                                            serde_json::json!({ "envelope_id": envelope_id });
                                        let _ = write
                                            .send(WsMsg::Text(ack.to_string()))
                                            .await;
                                    }
                                    tracing::debug!("Slack WS: unhandled type '{}'", other);
                                }
                            }
                        }
                        Ok(WsMsg::Close(_)) => {
                            info!("Slack Socket Mode: WebSocket closed; reconnecting");
                            should_reconnect = true;
                            break;
                        }
                        Ok(WsMsg::Ping(data)) => {
                            let _ = write.send(WsMsg::Pong(data)).await;
                        }
                        Ok(_) => {}
                        Err(e) => {
                            error!("Slack Socket Mode WS error: {}", e);
                            should_reconnect = true;
                            break;
                        }
                    }
                }

                if should_reconnect {
                    *health.write().await = ChannelHealth::Unhealthy {
                        reason: "reconnecting".to_string(),
                    };
                    tokio::time::sleep(reconnect_delay).await;
                    reconnect_delay = std::cmp::min(reconnect_delay * 2, max_delay);
                }
            }
            Err(e) => {
                error!("Slack Socket Mode: WebSocket connect error: {}", e);
                *health.write().await = ChannelHealth::Unhealthy {
                    reason: format!("WS connect error: {}", e),
                };
                tokio::time::sleep(reconnect_delay).await;
                reconnect_delay = std::cmp::min(reconnect_delay * 2, max_delay);
            }
        }
    }
}

/// Call `apps.connections.open` and return the WSS URL.
async fn open_connection(http_client: &Client, app_token: &str) -> Result<String> {
    #[derive(Deserialize)]
    struct ConnResp {
        ok: bool,
        url: Option<String>,
        error: Option<String>,
    }

    let resp: ConnResp = http_client
        .post("https://slack.com/api/apps.connections.open")
        .bearer_auth(app_token)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .send()
        .await?
        .json()
        .await?;

    if !resp.ok {
        return Err(anyhow::anyhow!(
            "apps.connections.open failed: {}",
            resp.error.unwrap_or_default()
        ));
    }

    resp.url
        .ok_or_else(|| anyhow::anyhow!("apps.connections.open returned no URL"))
}

/// Parse an `events_api` Socket Mode envelope into a [`Message`].
fn parse_socket_event(val: &Value, channel_id: &str, _bot_token: &str) -> Option<Message> {
    let payload = &val["payload"];
    let event = &payload["event"];

    let event_type = event["type"].as_str()?;
    if event_type != "message" {
        return None;
    }

    // Ignore bot messages
    if event.get("bot_id").is_some() {
        return None;
    }

    let text = event["text"].as_str()?.to_string();
    let user_id = event["user"].as_str().unwrap_or("unknown").to_string();
    let slack_channel = event["channel"].as_str().unwrap_or(channel_id).to_string();

    info!(
        "Slack Socket Mode: message from {} in {}: {} chars",
        user_id,
        slack_channel,
        text.len()
    );

    Some(Message::new(
        slack_channel,
        user_id.clone(),
        user_id,
        text,
    ))
}
