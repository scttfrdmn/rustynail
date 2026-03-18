pub mod dashboard;
pub mod http;
pub mod user_prefs;

use crate::agents::AgentManager;
use crate::channels::Channel;
use crate::config::Config;
use crate::gateway::dashboard::MessageStats;
use crate::memory::{
    InMemoryStore, MemorySummarizer, MemoryStore, PostgresStore, RedisStore, SqliteStore,
    VectorMemoryStore,
};
use crate::tools::ToolRegistry;
use crate::types::{GatewayEvent, Message};
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, RwLock};
use tokio::task::JoinHandle;
use tracing::{error, info, Instrument};

use agenkit::protocols::{McpClient, McpHttpClient, McpStdioClient, mcp_tools_from_client};
use agenkit::Tool;
use user_prefs::UserPreferences;

pub struct Gateway {
    config: Config,
    channels: Arc<RwLock<Vec<Box<dyn Channel>>>>,
    memory: Arc<dyn MemoryStore>,
    summarizer: Option<Arc<MemorySummarizer>>,
    agent_manager: Arc<AgentManager>,
    user_prefs: Arc<UserPreferences>,
    stats: Arc<MessageStats>,
    event_tx: broadcast::Sender<GatewayEvent>,
    _event_rx: broadcast::Receiver<GatewayEvent>,
    tasks: Vec<JoinHandle<()>>,
    /// Sender given to webhook-based channels / HTTP server for inbound messages
    message_tx: mpsc::UnboundedSender<Message>,
    message_rx: Option<mpsc::UnboundedReceiver<Message>>,
}

impl Gateway {
    pub fn new(config: Config) -> Self {
        let (event_tx, event_rx) = broadcast::channel(100);
        let (message_tx, message_rx) = mpsc::unbounded_channel();

        // Select memory backend based on config
        let memory: Arc<dyn MemoryStore> = match config.memory.backend.as_str() {
            "redis" => {
                match &config.memory.redis_url {
                    Some(url) => {
                        match RedisStore::new(
                            url,
                            config.agents.max_history,
                            config.memory.redis_ttl_seconds,
                        ) {
                            Ok(store) => {
                                info!("Using Redis memory store (url={})", url);
                                Arc::new(store)
                            }
                            Err(e) => {
                                error!(
                                    "Failed to create Redis store, falling back to in-memory: {}",
                                    e
                                );
                                Arc::new(InMemoryStore::new(config.agents.max_history))
                            }
                        }
                    }
                    None => {
                        error!("memory.backend=redis but REDIS_URL not set; falling back to in-memory");
                        Arc::new(InMemoryStore::new(config.agents.max_history))
                    }
                }
            }
            "sqlite" => {
                let path = config
                    .memory
                    .sqlite_path
                    .as_deref()
                    .unwrap_or("~/.rustynail/memory.db");
                // Expand ~ manually
                let expanded = if let Some(rest) = path.strip_prefix("~/") {
                    dirs_path_expand(rest)
                } else {
                    path.to_string()
                };
                match SqliteStore::new(&expanded, config.agents.max_history) {
                    Ok(store) => {
                        info!("Using SQLite memory store (path={})", expanded);
                        Arc::new(store)
                    }
                    Err(e) => {
                        error!("Failed to create SQLite store, falling back to in-memory: {}", e);
                        Arc::new(InMemoryStore::new(config.agents.max_history))
                    }
                }
            }
            "postgres" => {
                match &config.memory.postgres_url {
                    Some(url) => {
                        match PostgresStore::new(url, config.agents.max_history) {
                            Ok(store) => {
                                info!("Using PostgreSQL memory store");
                                Arc::new(store)
                            }
                            Err(e) => {
                                error!("Failed to create Postgres store, falling back to in-memory: {}", e);
                                Arc::new(InMemoryStore::new(config.agents.max_history))
                            }
                        }
                    }
                    None => {
                        error!("memory.backend=postgres but DATABASE_URL not set; falling back to in-memory");
                        Arc::new(InMemoryStore::new(config.agents.max_history))
                    }
                }
            }
            "vector" => {
                match VectorMemoryStore::new(config.agents.max_history) {
                    Ok(store) => {
                        info!("Using vector memory store (in-process, simple embeddings)");
                        Arc::new(store)
                    }
                    Err(e) => {
                        error!("Failed to create vector store, falling back to in-memory: {}", e);
                        Arc::new(InMemoryStore::new(config.agents.max_history))
                    }
                }
            }
            _ => {
                Arc::new(InMemoryStore::new(config.agents.max_history))
            }
        };

        // Build summarizer if enabled
        let summarizer = if config.memory.summarization.enabled {
            info!(
                "Memory summarization enabled (trigger_at={}, keep_recent={})",
                config.memory.summarization.trigger_at,
                config.memory.summarization.keep_recent
            );
            Some(Arc::new(MemorySummarizer::new(
                config.memory.summarization.clone(),
                config.agents.api_key.clone(),
                config.agents.api_base.clone(),
            )))
        } else {
            None
        };

        // Build tool registry from config
        let mut tool_registry = ToolRegistry::new();

        if config.tools.enabled {
            // Always register calculator
            let calc_tool = crate::tools::calculator::CalculatorTool;
            if let Err(e) = tool_registry.register(Arc::new(calc_tool)) {
                error!("Failed to register calculator tool: {}", e);
            }

            // Register filesystem tool if root is configured
            if let Some(ref fs_root) = config.tools.filesystem_root {
                let root = std::path::PathBuf::from(fs_root);
                let fs_tool = crate::tools::filesystem::FileSystemTool::new(root);
                if let Err(e) = tool_registry.register(Arc::new(fs_tool)) {
                    error!("Failed to register filesystem tool: {}", e);
                }
            }

            // Register web search tool if API key is configured
            if let Some(ref api_key) = config.tools.web_search_api_key {
                let ws_tool = crate::tools::web_search::WebSearchTool::new(api_key.clone());
                if let Err(e) = tool_registry.register(Arc::new(ws_tool)) {
                    error!("Failed to register web search tool: {}", e);
                }
            }

            // Register calendar tool
            let cal_tool = crate::tools::calendar::CalendarTool::with_default_dir();
            if let Err(e) = tool_registry.register(Arc::new(cal_tool)) {
                error!("Failed to register calendar tool: {}", e);
            }

            // Register formatter tool
            let fmt_tool = crate::tools::formatter::FormatterTool;
            if let Err(e) = tool_registry.register(Arc::new(fmt_tool)) {
                error!("Failed to register formatter tool: {}", e);
            }
        }

        let agent_manager = Arc::new(AgentManager::with_tools(
            config.agents.clone(),
            config.tools.clone(),
            tool_registry,
        ));

        Self {
            config,
            channels: Arc::new(RwLock::new(Vec::new())),
            memory,
            summarizer,
            agent_manager,
            user_prefs: Arc::new(UserPreferences::new()),
            stats: MessageStats::new(),
            event_tx,
            _event_rx: event_rx,
            tasks: Vec::new(),
            message_tx,
            message_rx: Some(message_rx),
        }
    }

    /// Returns a sender for delivering inbound messages to this gateway.
    pub fn message_sender(&self) -> mpsc::UnboundedSender<Message> {
        self.message_tx.clone()
    }

    /// Returns a reference to the user preferences store.
    pub fn user_prefs(&self) -> Arc<UserPreferences> {
        self.user_prefs.clone()
    }

    /// Register a channel with the gateway.
    pub async fn register_channel(&mut self, channel: Box<dyn Channel>) {
        info!("Registering channel: {}", channel.name());
        let mut channels = self.channels.write().await;
        channels.push(channel);
    }

    /// Register a tool with the agent manager.
    pub async fn register_tool(&self, tool: Arc<dyn Tool>) -> Result<()> {
        self.agent_manager.register_tool(tool).await
    }

    /// Start the gateway and all registered channels.
    pub async fn start(&mut self) -> Result<()> {
        info!("Starting gateway");

        // Add WhatsApp channel if enabled
        if let Some(wa_config) = self.config.channels.whatsapp.clone() {
            if wa_config.enabled {
                info!("Setting up WhatsApp channel");
                let wa = crate::channels::whatsapp::WhatsAppChannel::new(
                    "whatsapp-main".to_string(),
                    wa_config,
                );
                self.register_channel(Box::new(wa)).await;
            }
        }

        // Add Telegram channel if enabled — choose webhook or long-poll mode
        if let Some(tg_config) = self.config.channels.telegram.clone().filter(|c| c.enabled) {
            if tg_config.mode == "longpoll" {
                info!("Setting up Telegram channel (long-poll mode)");
                let tg = crate::channels::telegram_longpoll::TelegramLongPollChannel::new(
                    "telegram-main".to_string(),
                    tg_config,
                    self.message_tx.clone(),
                );
                self.register_channel(Box::new(tg)).await;
            } else {
                info!("Setting up Telegram channel (webhook mode)");
                let tg = crate::channels::telegram::TelegramChannel::new(
                    "telegram-main".to_string(),
                    tg_config,
                );
                self.register_channel(Box::new(tg)).await;
            }
        }

        // Add Slack channel — webhook mode or socket mode
        if let Some(sl_config) = self.config.channels.slack.clone().filter(|c| c.enabled) {
            if sl_config.mode == "socket" {
                info!("Setting up Slack channel (Socket Mode)");
                let sl = crate::channels::slack_socketmode::SlackSocketModeChannel::new(
                    "slack-main".to_string(),
                    sl_config,
                    self.message_tx.clone(),
                );
                self.register_channel(Box::new(sl)).await;
            } else {
                info!("Setting up Slack channel (webhook mode)");
                let sl =
                    crate::channels::slack::SlackChannel::new("slack-main".to_string(), sl_config);
                self.register_channel(Box::new(sl)).await;
            }
        }

        // Add SMS channel if enabled (webhook-based, routes handled by HTTP)
        if let Some(sms_config) = self.config.channels.sms.clone().filter(|c| c.enabled) {
            info!("Setting up SMS channel (Twilio webhook mode)");
            let sms =
                crate::channels::sms::SmsChannel::new("sms-main".to_string(), sms_config);
            self.register_channel(Box::new(sms)).await;
        }

        // Add generic webhook channel if enabled
        if let Some(wh_config) = self.config.channels.webhook.clone().filter(|c| c.enabled) {
            info!(
                "Setting up generic webhook channel ({} endpoints)",
                wh_config.endpoints.len()
            );
            let wh = crate::channels::webhook::WebhookChannel::new(
                "webhook-main".to_string(),
                wh_config,
            );
            self.register_channel(Box::new(wh)).await;
        }

        // Add Webchat channel if enabled
        let webchat_sessions = if let Some(wc_config) =
            self.config.channels.webchat.clone().filter(|c| c.enabled)
        {
            info!("Setting up webchat channel");
            let wc =
                crate::channels::webchat::WebchatChannel::new("webchat-main".to_string(), wc_config);
            let sessions = wc.sessions_handle();
            self.register_channel(Box::new(wc)).await;
            Some(sessions)
        } else {
            None
        };

        // Add Email channel if enabled
        if let Some(em_config) = self.config.channels.email.clone().filter(|c| c.enabled) {
            info!("Setting up email channel");
            let em = crate::channels::email::EmailChannel::new(
                "email-main".to_string(),
                em_config,
                self.message_tx.clone(),
            );
            self.register_channel(Box::new(em)).await;
        }

        // Connect MCP servers and register their tools
        for server_cfg in &self.config.mcp.servers {
            let tools = match server_cfg.transport.as_str() {
                "http" => {
                    let url = match &server_cfg.url {
                        Some(u) => u.clone(),
                        None => {
                            error!(
                                "MCP server '{}' has transport=http but no url configured",
                                server_cfg.name
                            );
                            continue;
                        }
                    };
                    let mut client = McpHttpClient::new(&url);
                    match client.initialize().await {
                        Ok(()) => {
                            info!(
                                "MCP server '{}' connected ({})",
                                server_cfg.name,
                                client.server_info().name
                            );
                            mcp_tools_from_client(std::sync::Arc::new(client))
                                .await
                                .unwrap_or_else(|e| {
                                    error!("Failed to list tools from MCP server '{}': {}", server_cfg.name, e);
                                    vec![]
                                })
                        }
                        Err(e) => {
                            error!("Failed to initialize MCP server '{}': {}", server_cfg.name, e);
                            continue;
                        }
                    }
                }
                _ => {
                    // stdio (default)
                    let command = match &server_cfg.command {
                        Some(c) => c.clone(),
                        None => {
                            error!(
                                "MCP server '{}' has transport=stdio but no command configured",
                                server_cfg.name
                            );
                            continue;
                        }
                    };
                    let arg_strs: Vec<&str> = server_cfg.args.iter().map(|s| s.as_str()).collect();
                    let mut client = McpStdioClient::new(&command, &arg_strs);
                    for (k, v) in &server_cfg.env {
                        client = client.with_env(k, v);
                    }
                    match client.initialize().await {
                        Ok(()) => {
                            info!(
                                "MCP server '{}' connected ({})",
                                server_cfg.name,
                                client.server_info().name
                            );
                            mcp_tools_from_client(std::sync::Arc::new(client))
                                .await
                                .unwrap_or_else(|e| {
                                    error!("Failed to list tools from MCP server '{}': {}", server_cfg.name, e);
                                    vec![]
                                })
                        }
                        Err(e) => {
                            error!("Failed to initialize MCP server '{}': {}", server_cfg.name, e);
                            continue;
                        }
                    }
                }
            };

            for tool in tools {
                if let Err(e) = self.register_tool(tool).await {
                    error!("Failed to register MCP tool: {}", e);
                }
            }
        }

        // Pre-compute sender and config values for HTTP server
        let whatsapp_tx = self
            .config
            .channels
            .whatsapp
            .as_ref()
            .filter(|c| c.enabled)
            .map(|_| self.message_tx.clone());

        let whatsapp_verify_token = self
            .config
            .channels
            .whatsapp
            .as_ref()
            .map(|c| c.verify_token.clone())
            .unwrap_or_default();

        let telegram_tx = self
            .config
            .channels
            .telegram
            .as_ref()
            .filter(|c| c.enabled && c.mode != "longpoll")
            .map(|_| self.message_tx.clone());

        let telegram_webhook_secret = self
            .config
            .channels
            .telegram
            .as_ref()
            .map(|c| c.webhook_secret.clone())
            .unwrap_or_default();

        let slack_tx = self
            .config
            .channels
            .slack
            .as_ref()
            .filter(|c| c.enabled && c.mode != "socket")
            .map(|_| self.message_tx.clone());

        let slack_signing_secret = self
            .config
            .channels
            .slack
            .as_ref()
            .map(|c| c.signing_secret.clone())
            .unwrap_or_default();

        let sms_tx = self
            .config
            .channels
            .sms
            .as_ref()
            .filter(|c| c.enabled)
            .map(|_| self.message_tx.clone());

        let sms_auth_token = self
            .config
            .channels
            .sms
            .as_ref()
            .map(|c| c.auth.auth_token.clone())
            .unwrap_or_default();

        let webhook_endpoints = self
            .config
            .channels
            .webhook
            .as_ref()
            .filter(|c| c.enabled)
            .map(|c| c.endpoints.clone())
            .unwrap_or_default();

        let webhook_tx = if !webhook_endpoints.is_empty() {
            Some(self.message_tx.clone())
        } else {
            None
        };

        let webchat_tx = webchat_sessions
            .as_ref()
            .map(|_| self.message_tx.clone());

        // Pre-compute dashboard basic-auth header value if password is set
        let dashboard_expected_auth = self.config.dashboard.auth_password.as_deref().map(|pw| {
            use base64::Engine;
            let encoded =
                base64::engine::general_purpose::STANDARD.encode(format!("rustynail:{}", pw));
            format!("Basic {}", encoded)
        });

        // Start HTTP server
        let http_cfg = http::HttpServerConfig {
            port: self.config.gateway.http_port,
            channels: self.channels.clone(),
            agent_manager: self.agent_manager.clone(),
            whatsapp_tx,
            whatsapp_verify_token,
            telegram_tx,
            telegram_webhook_secret,
            slack_tx,
            slack_signing_secret,
            sms_tx,
            sms_auth_token,
            webhook_endpoints,
            webhook_tx,
            webchat_sessions,
            webchat_tx,
            user_prefs: self.user_prefs.clone(),
            stats: self.stats.clone(),
            dashboard_expected_auth,
        };

        let http_task = tokio::spawn(async move {
            if let Err(e) = http::start_http_server(http_cfg).await {
                error!("HTTP server error: {}", e);
            }
        });
        self.tasks.push(http_task);

        // Start all channels
        {
            let mut channels = self.channels.write().await;
            for channel in channels.iter_mut() {
                info!("Starting channel: {}", channel.name());
                channel.start().await?;
            }
        }

        // Spawn internal message processing loop
        if let Some(mut rx) = self.message_rx.take() {
            let memory = self.memory.clone();
            let summarizer = self.summarizer.clone();
            let agent_manager = self.agent_manager.clone();
            let channels = self.channels.clone();
            let user_prefs = self.user_prefs.clone();
            let stats = self.stats.clone();

            let msg_task = tokio::spawn(async move {
                while let Some(message) = rx.recv().await {
                    let span = tracing::info_span!(
                        "gateway.handle_message",
                        user_id = %message.user_id,
                        channel_id = %message.channel_id
                    );
                    if let Err(e) = handle_message_inner(
                        &memory,
                        &summarizer,
                        &agent_manager,
                        &channels,
                        &user_prefs,
                        &stats,
                        message,
                    )
                    .instrument(span)
                    .await
                    {
                        error!("Error handling message: {}", e);
                    }
                }
            });
            self.tasks.push(msg_task);
        }

        info!("Gateway started successfully");
        Ok(())
    }

    /// Stop the gateway and all channels.
    pub async fn stop(&mut self) -> Result<()> {
        info!("Stopping gateway");

        let _ = self.event_tx.send(GatewayEvent::Shutdown);

        {
            let mut channels = self.channels.write().await;
            for channel in channels.iter_mut() {
                info!("Stopping channel: {}", channel.name());
                if let Err(e) = channel.stop().await {
                    error!("Error stopping channel {}: {}", channel.name(), e);
                }
            }
        }

        for task in self.tasks.drain(..) {
            task.abort();
        }

        info!("Gateway stopped");
        Ok(())
    }

    /// Handle an incoming message (kept for external callers / tests).
    pub async fn handle_message(&self, message: Message) -> Result<()> {
        let span = tracing::info_span!(
            "gateway.handle_message",
            user_id = %message.user_id,
            channel_id = %message.channel_id
        );
        handle_message_inner(
            &self.memory,
            &self.summarizer,
            &self.agent_manager,
            &self.channels,
            &self.user_prefs,
            &self.stats,
            message,
        )
        .instrument(span)
        .await
    }

    pub fn event_sender(&self) -> broadcast::Sender<GatewayEvent> {
        self.event_tx.clone()
    }

    pub fn memory(&self) -> Arc<dyn MemoryStore> {
        self.memory.clone()
    }
}

/// Public entry point for integration tests — delegates to `handle_message_inner`.
pub async fn handle_message_for_test(
    memory: &Arc<dyn MemoryStore>,
    agent_manager: &Arc<AgentManager>,
    channels: &Arc<RwLock<Vec<Box<dyn Channel>>>>,
    user_prefs: &Arc<user_prefs::UserPreferences>,
    stats: &Arc<MessageStats>,
    message: Message,
) -> Result<()> {
    handle_message_inner(memory, &None, agent_manager, channels, user_prefs, stats, message).await
}

/// Core message-handling logic shared between the internal loop and public method.
async fn handle_message_inner(
    memory: &Arc<dyn MemoryStore>,
    summarizer: &Option<Arc<MemorySummarizer>>,
    agent_manager: &Arc<AgentManager>,
    channels: &Arc<RwLock<Vec<Box<dyn Channel>>>>,
    user_prefs: &Arc<UserPreferences>,
    stats: &Arc<MessageStats>,
    message: Message,
) -> Result<()> {
    info!(
        "Handling message from {} in channel {}",
        message.username, message.channel_id
    );

    // Resolve the channel to route the response to
    let response_channel_id = if let Some(ref preferred) = message.preferred_channel_id {
        preferred.clone()
    } else if let Some(pref) = user_prefs.get(&message.user_id).await {
        pref
    } else {
        message.channel_id.clone()
    };

    // Track in memory store + stats
    memory.add_message(&message.user_id, format!("User: {}", message.content));

    // Maybe summarise history (fire-and-forget)
    if let Some(ref s) = summarizer {
        s.maybe_summarize(memory.clone(), &message.user_id);
    }

    stats.record_inbound_async(&message).await;

    // Process with agent (record duration for Prometheus histogram)
    let processing_start = std::time::Instant::now();
    let response_content = agent_manager
        .process_message(&message.user_id, &message.content)
        .instrument(tracing::info_span!("agent.process", user_id = %message.user_id))
        .await?;
    stats.observe_message_duration(processing_start.elapsed().as_secs_f64());

    memory.add_message(&message.user_id, format!("Assistant: {}", response_content));

    // Send response to the resolved channel
    let response = Message::new(
        response_channel_id.clone(),
        "assistant".to_string(),
        "RustyNail".to_string(),
        response_content,
    );

    let channels = channels.read().await;
    for channel in channels.iter() {
        if channel.id() == response_channel_id {
            channel.send_message(response.clone()).await?;
            stats.record_outbound_async(&response).await;
            return Ok(());
        }
    }

    error!(
        "No channel found with id '{}' to send response",
        response_channel_id
    );
    Ok(())
}

/// Expand `~/...` path using the HOME environment variable.
fn dirs_path_expand(rest: &str) -> String {
    std::env::var("HOME")
        .map(|home| format!("{}/{}", home, rest))
        .unwrap_or_else(|_| format!("/tmp/{}", rest))
}
