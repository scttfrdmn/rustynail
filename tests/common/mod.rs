//! Shared test helpers.
//!
//! Cargo compiles this module separately into every integration test binary
//! that declares `mod common;`. No single binary uses every helper, so unused
//! items here are expected rather than dead — hence the crate-wide allow.
#![allow(dead_code)]

use anyhow::Result;
use async_trait::async_trait;
use rustynail::agents::AgentManager;
use rustynail::channels::Channel;
use rustynail::config::{AgentsConfig, RateLimitConfig, SkillsConfig};
use rustynail::gateway::dashboard::MessageStats;
use rustynail::gateway::http::AppState;
use rustynail::gateway::rate_limiter::RateLimiter;
use rustynail::gateway::user_prefs::UserPreferences;
use rustynail::gateway::HotConfig;
use rustynail::types::{ChannelHealth, Message};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::sync::{mpsc, Mutex};

#[allow(dead_code)]
pub fn make_test_state_stub() -> AppState {
    AppState {
        channels: Arc::new(RwLock::new(Vec::new())),
        agent_manager: Arc::new(AgentManager::new(AgentsConfig {
            llm_provider: "stub".to_string(),
            ..Default::default()
        })),
        whatsapp_tx: None,
        whatsapp_verify_token: "test-verify-token".to_string(),
        telegram_tx: None,
        telegram_webhook_secret: String::new(),
        slack_tx: None,
        slack_signing_secret: String::new(),
        sms_tx: None,
        sms_auth_token: String::new(),
        webhook_endpoints: Vec::new(),
        webhook_tx: None,
        webchat_sessions: None,
        webchat_tx: None,
        teams_tx: None,
        teams_hmac_secret: String::new(),
        user_prefs: Arc::new(UserPreferences::new()),
        stats: MessageStats::new(),
        dashboard_expected_auth: None,
        api_token: None,
        test_channel: None,
        test_tx: None,
        rate_limiter: RateLimiter::new(),
        audit: None,
        hot_config: Arc::new(RwLock::new(HotConfig {
            log_level: "error".to_string(),
            api_token: None,
            rate_limit: RateLimitConfig::default(),
            audit_enabled: false,
            audit_path: String::new(),
        })),
        skills_config: SkillsConfig::default(),
        cron_jobs: Vec::new(),
        allowed_ws_origins: Vec::new(),
    }
}

fn default_hot_config() -> Arc<RwLock<HotConfig>> {
    Arc::new(RwLock::new(HotConfig {
        log_level: "error".to_string(),
        api_token: None,
        rate_limit: RateLimitConfig::default(),
        audit_enabled: false,
        audit_path: String::new(),
    }))
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
        sms_tx: None,
        sms_auth_token: String::new(),
        webhook_endpoints: Vec::new(),
        webhook_tx: None,
        webchat_sessions: None,
        webchat_tx: None,
        teams_tx: None,
        teams_hmac_secret: String::new(),
        user_prefs: Arc::new(UserPreferences::new()),
        stats: MessageStats::new(),
        dashboard_expected_auth: None,
        api_token: None,
        test_channel: None,
        test_tx: None,
        rate_limiter: RateLimiter::new(),
        audit: None,
        hot_config: default_hot_config(),
        skills_config: SkillsConfig::default(),
        cron_jobs: Vec::new(),
        allowed_ws_origins: Vec::new(),
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
        sms_tx: None,
        sms_auth_token: String::new(),
        webhook_endpoints: Vec::new(),
        webhook_tx: None,
        webchat_sessions: None,
        webchat_tx: None,
        teams_tx: None,
        teams_hmac_secret: String::new(),
        user_prefs: Arc::new(UserPreferences::new()),
        stats: MessageStats::new(),
        dashboard_expected_auth: None,
        api_token: None,
        test_channel: None,
        test_tx: None,
        rate_limiter: RateLimiter::new(),
        audit: None,
        hot_config: default_hot_config(),
        skills_config: SkillsConfig::default(),
        cron_jobs: Vec::new(),
        allowed_ws_origins: Vec::new(),
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
