use crate::types::{ChannelHealth, Message};
use anyhow::Result;
use async_trait::async_trait;

/// Channel trait that all messaging platform adapters must implement
#[async_trait]
pub trait Channel: Send + Sync {
    /// Returns the unique identifier for this channel
    fn id(&self) -> &str;

    /// Returns the name of this channel (e.g., "discord", "telegram")
    fn name(&self) -> &str;

    /// Starts the channel and begins listening for messages
    async fn start(&mut self) -> Result<()>;

    /// Stops the channel gracefully
    async fn stop(&mut self) -> Result<()>;

    /// Sends a message through this channel
    async fn send_message(&self, message: Message) -> Result<()>;

    /// Returns the current health status of the channel
    fn health(&self) -> ChannelHealth;

    /// Returns whether the channel is currently running
    fn is_running(&self) -> bool;
}
