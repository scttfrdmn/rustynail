use anyhow::Result;
use async_trait::async_trait;
use rustynail::agents::AgentManager;
use rustynail::channels::Channel;
use rustynail::config::{AgentsConfig, ChannelsConfig, Config, GatewayConfig, MemoryConfig};
use rustynail::gateway::dashboard::MessageStats;
use rustynail::gateway::http::AppState;
use rustynail::gateway::user_prefs::UserPreferences;
use rustynail::types::{ChannelHealth, Message};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::sync::{mpsc, Mutex};

/// Minimal test config — no real channels, dummy api_key.
pub fn make_test_config() -> Config {
    Config {
        gateway: GatewayConfig {
            websocket_port: 18789,
            http_port: 18081,
            log_level: "error".to_string(),
        },
        channels: ChannelsConfig {
            discord: None,
            whatsapp: None,
            telegram: None,
            slack: None,
        },
        agents: AgentsConfig {
            api_key: "test_key_unused".to_string(),
            ..Default::default()
        },
        tools: Default::default(),
        otel: Default::default(),
        dashboard: Default::default(),
        memory: MemoryConfig::default(),
    }
}

/// AppState wired up for tests — no channels, no webhook senders.
pub fn make_test_state() -> AppState {
    AppState {
        channels: Arc::new(RwLock::new(Vec::new())),
        agent_manager: Arc::new(AgentManager::new(Default::default())),
        whatsapp_tx: None,
        whatsapp_verify_token: "test-verify-token".to_string(),
        telegram_tx: None,
        telegram_webhook_secret: String::new(),
        slack_tx: None,
        slack_signing_secret: String::new(),
        user_prefs: Arc::new(UserPreferences::new()),
        stats: MessageStats::new(),
        dashboard_expected_auth: None,
    }
}

/// AppState with a real mpsc sender wired to each webhook handler.
pub fn make_test_state_with_webhooks() -> (
    AppState,
    mpsc::UnboundedReceiver<Message>,
    mpsc::UnboundedReceiver<Message>,
    mpsc::UnboundedReceiver<Message>,
) {
    let (wa_tx, wa_rx) = mpsc::unbounded_channel();
    let (tg_tx, tg_rx) = mpsc::unbounded_channel();
    let (sl_tx, sl_rx) = mpsc::unbounded_channel();
    let state = AppState {
        channels: Arc::new(RwLock::new(Vec::new())),
        agent_manager: Arc::new(AgentManager::new(Default::default())),
        whatsapp_tx: Some(wa_tx),
        whatsapp_verify_token: "test-verify-token".to_string(),
        telegram_tx: Some(tg_tx),
        telegram_webhook_secret: String::new(),
        slack_tx: Some(sl_tx),
        slack_signing_secret: String::new(),
        user_prefs: Arc::new(UserPreferences::new()),
        stats: MessageStats::new(),
        dashboard_expected_auth: None,
    };
    (state, wa_rx, tg_rx, sl_rx)
}

/// A channel implementation that records every outbound `send_message` call.
pub struct RecordingChannel {
    pub id: String,
    pub sent: Arc<Mutex<Vec<Message>>>,
}

impl RecordingChannel {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            sent: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn sent_handle(&self) -> Arc<Mutex<Vec<Message>>> {
        self.sent.clone()
    }
}

#[async_trait]
impl Channel for RecordingChannel {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "recording"
    }

    async fn start(&mut self) -> Result<()> {
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        Ok(())
    }

    async fn send_message(&self, msg: Message) -> Result<()> {
        self.sent.lock().await.push(msg);
        Ok(())
    }

    fn health(&self) -> ChannelHealth {
        ChannelHealth::Healthy
    }

    fn is_running(&self) -> bool {
        true
    }
}
