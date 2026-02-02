use crate::channels::Channel;
use crate::types::{ChannelHealth, Message};
use anyhow::Result;
use async_trait::async_trait;
use serenity::all::{ChannelId, Context, EventHandler, GatewayIntents, Ready};
use serenity::Client;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{error, info};

pub struct DiscordChannel {
    id: String,
    token: String,
    health: Arc<RwLock<ChannelHealth>>,
    message_tx: mpsc::UnboundedSender<Message>,
    client: Arc<RwLock<Option<Client>>>,
}

struct Handler {
    message_tx: mpsc::UnboundedSender<Message>,
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, _ctx: Context, ready: Ready) {
        info!("Discord bot connected as {}", ready.user.name);
    }

    async fn message(&self, ctx: Context, msg: serenity::all::Message) {
        // Ignore messages from bots (including ourselves)
        if msg.author.bot {
            return;
        }

        // Create our internal message type
        let message = Message::new(
            msg.channel_id.to_string(),
            msg.author.id.to_string(),
            msg.author.name.clone(),
            msg.content.clone(),
        );

        // Send to gateway for processing
        if let Err(e) = self.message_tx.send(message.clone()) {
            error!("Failed to send message to gateway: {}", e);
            return;
        }

        // For now, respond with a typing indicator
        let _ = msg.channel_id.broadcast_typing(&ctx.http).await;
    }
}

impl DiscordChannel {
    pub fn new(id: String, token: String, message_tx: mpsc::UnboundedSender<Message>) -> Self {
        Self {
            id,
            token,
            health: Arc::new(RwLock::new(ChannelHealth::Healthy)),
            message_tx,
            client: Arc::new(RwLock::new(None)),
        }
    }
}

#[async_trait]
impl Channel for DiscordChannel {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "discord"
    }

    async fn start(&mut self) -> Result<()> {
        info!("Starting Discord channel");

        let intents = GatewayIntents::GUILD_MESSAGES
            | GatewayIntents::DIRECT_MESSAGES
            | GatewayIntents::MESSAGE_CONTENT;

        let handler = Handler {
            message_tx: self.message_tx.clone(),
        };

        let client = Client::builder(&self.token, intents)
            .event_handler(handler)
            .await?;

        // Store client for later use
        *self.client.write().await = Some(client);

        // Start the client in a background task
        let client_clone = self.client.clone();
        let health_clone = self.health.clone();
        tokio::spawn(async move {
            if let Some(mut client) = client_clone.write().await.take() {
                if let Err(e) = client.start().await {
                    error!("Discord client error: {}", e);
                    *health_clone.write().await = ChannelHealth::Unhealthy {
                        reason: format!("Client error: {}", e),
                    };
                }
            }
        });

        info!("Discord channel started");
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        info!("Stopping Discord channel");

        // Serenity doesn't have a clean stop method
        // The client will be dropped when we exit
        *self.health.write().await = ChannelHealth::Unhealthy {
            reason: "Shutting down".to_string(),
        };

        Ok(())
    }

    async fn send_message(&self, message: Message) -> Result<()> {
        let client = self.client.read().await;
        if let Some(client) = client.as_ref() {
            let channel_id = ChannelId::new(message.channel_id.parse()?);
            channel_id
                .say(&client.http, message.content)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to send message: {}", e))?;
            Ok(())
        } else {
            Err(anyhow::anyhow!("Discord client not initialized"))
        }
    }

    fn health(&self) -> ChannelHealth {
        // This is a sync method, so we can't await here
        // In a real implementation, we'd want to restructure this
        ChannelHealth::Healthy
    }

    fn is_running(&self) -> bool {
        matches!(
            *self.health.blocking_read(),
            ChannelHealth::Healthy | ChannelHealth::Degraded { .. }
        )
    }
}
