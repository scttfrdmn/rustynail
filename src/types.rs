use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Attachment represents a file or media item attached to a message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    /// URL to the attachment (remote) or file path (local).
    pub url: String,

    /// Media type: `"pdf"`, `"image"`, or `"unknown"`.
    pub media_type: String,

    /// Optional filename hint.
    pub filename: Option<String>,
}

/// Message represents a message flowing through the system
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// Unique message identifier
    pub id: String,

    /// Channel this message originated from
    pub channel_id: String,

    /// User who sent the message
    pub user_id: String,

    /// Username for display
    pub username: String,

    /// Message content
    pub content: String,

    /// Message timestamp
    pub timestamp: DateTime<Utc>,

    /// Optional attachments
    #[serde(default)]
    pub attachments: Vec<Attachment>,

    /// Optional thread/conversation ID
    pub thread_id: Option<String>,

    /// Message metadata
    #[serde(default)]
    pub metadata: serde_json::Value,

    /// Override the channel to send the response to (cross-channel routing)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_channel_id: Option<String>,
}

impl Message {
    pub fn new(
        channel_id: impl Into<String>,
        user_id: impl Into<String>,
        username: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            channel_id: channel_id.into(),
            user_id: user_id.into(),
            username: username.into(),
            content: content.into(),
            timestamp: Utc::now(),
            attachments: Vec::new(),
            thread_id: None,
            metadata: serde_json::Value::Null,
            preferred_channel_id: None,
        }
    }

    pub fn with_attachments(mut self, attachments: Vec<Attachment>) -> Self {
        self.attachments = attachments;
        self
    }

    pub fn with_thread_id(mut self, thread_id: impl Into<String>) -> Self {
        self.thread_id = Some(thread_id.into());
        self
    }
}

/// ChannelHealth represents the health status of a channel
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChannelHealth {
    /// Channel is operating normally
    Healthy,

    /// Channel is degraded but operational
    Degraded { reason: String },

    /// Channel is not operational
    Unhealthy { reason: String },
}

impl ChannelHealth {
    pub fn is_healthy(&self) -> bool {
        matches!(self, ChannelHealth::Healthy)
    }

    pub fn is_operational(&self) -> bool {
        matches!(
            self,
            ChannelHealth::Healthy | ChannelHealth::Degraded { .. }
        )
    }
}

/// GatewayEvent represents events that flow through the gateway
#[derive(Debug, Clone)]
pub enum GatewayEvent {
    /// New message received
    MessageReceived(Message),

    /// Message sent successfully
    MessageSent(Message),

    /// Channel health changed
    ChannelHealthChanged {
        channel_id: String,
        health: ChannelHealth,
    },

    /// Gateway shutting down
    Shutdown,
}
