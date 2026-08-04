use crate::channels::Channel;
use crate::types::{ChannelHealth, Message};
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::Mutex;

/// A no-credential test channel that captures outbound messages and exposes
/// HTTP endpoints to inject messages and retrieve responses.
///
/// Activated when `channels.test_channel = true` in config (or `TEST_CHANNEL=true`).
///
/// Endpoints (registered in `http.rs` when test mode is active):
///   - `POST /test/send`      — inject `{"user_id":"...","content":"..."}` into the gateway
///   - `GET  /test/responses` — return and clear all captured outbound messages
pub struct TestChannel {
    id: String,
    /// All outbound messages captured by `send_message`.
    pub captured: Arc<Mutex<Vec<Message>>>,
    running: bool,
}

/// Shared buffer of messages captured by a [`TestChannel`].
///
/// The gateway owns the channel itself (as a `Box<dyn Channel>`), so the HTTP
/// layer holds one of these handles instead — both point at the same buffer.
pub type CapturedMessages = Arc<Mutex<Vec<Message>>>;

impl TestChannel {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            captured: Arc::new(Mutex::new(Vec::new())),
            running: false,
        }
    }

    /// Return a cloneable handle to the captured messages store.
    ///
    /// Callers must use this rather than constructing a second `TestChannel`:
    /// separate instances have separate buffers, so responses written by one
    /// are invisible to the other.
    pub fn captured_handle(&self) -> CapturedMessages {
        self.captured.clone()
    }

    /// Drain and return all messages captured so far.
    pub async fn drain_captured(captured: &CapturedMessages) -> Vec<Message> {
        let mut guard = captured.lock().await;
        guard.drain(..).collect()
    }
}

#[async_trait]
impl Channel for TestChannel {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "testchan"
    }

    async fn start(&mut self) -> Result<()> {
        self.running = true;
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        self.running = false;
        Ok(())
    }

    async fn send_message(&self, msg: Message) -> Result<()> {
        self.captured.lock().await.push(msg);
        Ok(())
    }

    fn health(&self) -> ChannelHealth {
        ChannelHealth::Healthy
    }

    fn is_running(&self) -> bool {
        self.running
    }
}
