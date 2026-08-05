pub mod chunker;
pub mod dashboard;
pub mod deduplicator;
pub mod formatter;
pub mod http;
pub mod openai_compat;
pub mod rate_limiter;
pub mod user_prefs;

use crate::agents::AgentManager;
use crate::audit::{AuditEvent, AuditLogger};
use crate::channels::Channel;
use crate::config::{Config, QuarryPolicyConfig, RateLimitConfig, SkillsConfig};
use crate::cron::CronScheduler;
use crate::gateway::chunker::MessageChunker;
use crate::gateway::dashboard::MessageStats;
use crate::gateway::deduplicator::MessageDeduplicator;
use crate::gateway::formatter::ResponseFormatter;
use crate::gateway::rate_limiter::RateLimiter;
use crate::memory::{
    InMemoryStore, MemoryStore, MemorySummarizer, PostgresStore, RedisStore, SqliteStore,
    VectorMemoryStore,
};
use crate::quarry::{ApprovalRegistry, ReplyOutcome, Supervisor as QuarrySupervisor};
use crate::skills::SkillRegistry;
use crate::tools::ToolRegistry;
use crate::types::{GatewayEvent, Message};
use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, Mutex, RwLock};
use tokio::task::JoinHandle;
use tracing::{error, info, warn, Instrument};

use agenkit::protocols::{mcp_tools_from_client, McpClient, McpHttpClient, McpStdioClient};
use agenkit::Tool;
use user_prefs::UserPreferences;

// ── Hot-reloadable config subset ──────────────────────────────────────────────

/// Fields that can be updated at runtime via SIGHUP without restarting.
#[derive(Debug, Clone)]
pub struct HotConfig {
    pub log_level: String,
    pub api_token: Option<String>,
    pub rate_limit: RateLimitConfig,
    pub audit_enabled: bool,
    pub audit_path: String,
    /// Who may run quarry and under what caps.
    ///
    /// Hot-reloadable because the alternative is worse: an operator who has to
    /// restart the gateway to tighten a cap will not tighten the cap. Policy is read
    /// per run, so a reload takes effect on the next run and leaves runs already in
    /// flight under the caps they started with — those caps were already handed to a
    /// child process and cannot be revised from here.
    pub quarry_policy: QuarryPolicyConfig,
}

impl HotConfig {
    pub fn from_config(config: &Config) -> Self {
        Self {
            log_level: config.gateway.log_level.clone(),
            api_token: config.gateway.api_token.clone(),
            rate_limit: config.gateway.rate_limit.clone(),
            audit_enabled: config.audit.enabled,
            audit_path: config.audit.path.clone(),
            quarry_policy: config.quarry.policy.clone(),
        }
    }

    /// Apply new config values, returning the names of changed fields.
    /// Non-hot-reloadable fields (ports, memory backend, LLM provider) are ignored
    /// with a warning when they differ.
    pub fn apply(&mut self, new: &Config) -> Vec<String> {
        let mut changed = Vec::new();

        if self.log_level != new.gateway.log_level {
            self.log_level = new.gateway.log_level.clone();
            changed.push("log_level".to_string());
        }
        if self.api_token != new.gateway.api_token {
            self.api_token = new.gateway.api_token.clone();
            changed.push("api_token".to_string());
        }
        if self.rate_limit.enabled != new.gateway.rate_limit.enabled {
            self.rate_limit.enabled = new.gateway.rate_limit.enabled;
            changed.push("rate_limit.enabled".to_string());
        }
        if self.rate_limit.messages_per_window != new.gateway.rate_limit.messages_per_window {
            self.rate_limit.messages_per_window = new.gateway.rate_limit.messages_per_window;
            changed.push("rate_limit.messages_per_window".to_string());
        }
        if self.rate_limit.window_seconds != new.gateway.rate_limit.window_seconds {
            self.rate_limit.window_seconds = new.gateway.rate_limit.window_seconds;
            changed.push("rate_limit.window_seconds".to_string());
        }
        if self.audit_enabled != new.audit.enabled {
            self.audit_enabled = new.audit.enabled;
            changed.push("audit.enabled".to_string());
        }
        if self.audit_path != new.audit.path {
            self.audit_path = new.audit.path.clone();
            changed.push("audit.path".to_string());
        }
        if self.quarry_policy != new.quarry.policy {
            self.quarry_policy = new.quarry.policy.clone();
            changed.push("quarry.policy".to_string());
        }

        changed
    }
}

// ── Gateway ───────────────────────────────────────────────────────────────────

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
    /// Per-user sliding-window rate limiter.
    rate_limiter: Arc<RateLimiter>,
    /// Structured audit logger (None when audit is disabled).
    audit: Option<Arc<AuditLogger>>,
    /// Hot-reloadable config subset shared with the HTTP layer.
    hot_config: Arc<RwLock<HotConfig>>,
    /// Cron scheduler (None when no jobs configured).
    cron_scheduler: Option<CronScheduler>,
    /// Skills config snapshot (for admin /skills/reload).
    skills_config: SkillsConfig,
    /// Message deduplicator (None when disabled).
    deduplicator: Option<Arc<Mutex<MessageDeduplicator>>>,
    /// Message chunker (None when disabled).
    chunker: Option<Arc<MessageChunker>>,
    /// Response formatter.
    formatter: Arc<ResponseFormatter>,
    /// Auto-route attachments to PDF/image tool prompts.
    auto_route_attachments: bool,
    /// quarry run supervisor. Present even when `quarry.enabled` is false, so a
    /// caller gets [`crate::quarry::SpawnError::Disabled`] — a reason — rather than
    /// a missing capability it has to guess about.
    quarry: Arc<QuarrySupervisor>,
    /// Pending plan approvals, keyed by (channel, sender).
    ///
    /// Held on the gateway rather than per-run because a reply arrives as an
    /// ordinary inbound message, so the pipeline has to be able to look up "does
    /// this sender owe me an answer" before it processes anything.
    quarry_approvals: Arc<ApprovalRegistry>,
}

impl Gateway {
    pub fn new(config: Config) -> Self {
        let (event_tx, event_rx) = broadcast::channel(100);
        let (message_tx, message_rx) = mpsc::unbounded_channel();

        // Select memory backend based on config
        let memory: Arc<dyn MemoryStore> = match config.memory.backend.as_str() {
            "redis" => match &config.memory.redis_url {
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
            },
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
                        error!(
                            "Failed to create SQLite store, falling back to in-memory: {}",
                            e
                        );
                        Arc::new(InMemoryStore::new(config.agents.max_history))
                    }
                }
            }
            "postgres" => match &config.memory.postgres_url {
                Some(url) => match PostgresStore::new(url, config.agents.max_history) {
                    Ok(store) => {
                        info!("Using PostgreSQL memory store");
                        Arc::new(store)
                    }
                    Err(e) => {
                        error!(
                            "Failed to create Postgres store, falling back to in-memory: {}",
                            e
                        );
                        Arc::new(InMemoryStore::new(config.agents.max_history))
                    }
                },
                None => {
                    error!("memory.backend=postgres but DATABASE_URL not set; falling back to in-memory");
                    Arc::new(InMemoryStore::new(config.agents.max_history))
                }
            },
            "vector" => {
                match VectorMemoryStore::with_decay(
                    config.agents.max_history,
                    config.memory.vector_decay_half_life_seconds,
                ) {
                    Ok(store) => {
                        info!("Using vector memory store (in-process, simple embeddings)");
                        Arc::new(store)
                    }
                    Err(e) => {
                        error!(
                            "Failed to create vector store, falling back to in-memory: {}",
                            e
                        );
                        Arc::new(InMemoryStore::new(config.agents.max_history))
                    }
                }
            }
            _ => Arc::new(InMemoryStore::new(config.agents.max_history)),
        };

        // Build summarizer if enabled
        let summarizer = if config.memory.summarization.enabled {
            info!(
                "Memory summarization enabled (trigger_at={}, keep_recent={})",
                config.memory.summarization.trigger_at, config.memory.summarization.keep_recent
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

            // Register web fetch tool (always enabled with tools)
            let wf_tool = crate::tools::web_fetch::WebFetchTool;
            if let Err(e) = tool_registry.register(Arc::new(wf_tool)) {
                error!("Failed to register web fetch tool: {}", e);
            }

            // Register shell tool if enabled in config
            if config.tools.shell.enabled {
                let shell_cfg = crate::tools::shell::ShellToolConfig {
                    require_approval: config.tools.shell.require_approval,
                    allowed_commands: config.tools.shell.allowed_commands.clone(),
                };
                let sh_tool = crate::tools::shell::ShellTool::new(shell_cfg);
                if let Err(e) = tool_registry.register(Arc::new(sh_tool)) {
                    error!("Failed to register shell tool: {}", e);
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

            // Register PDF analysis tool if enabled
            if config.tools.pdf_enabled {
                let api_base = config
                    .agents
                    .api_base
                    .clone()
                    .unwrap_or_else(|| "https://api.anthropic.com".to_string());
                let pdf_tool = crate::tools::pdf::PdfAnalysisTool::new(
                    config.agents.api_key.clone(),
                    api_base,
                    config.agents.llm_model.clone(),
                );
                if let Err(e) = tool_registry.register(Arc::new(pdf_tool)) {
                    error!("Failed to register pdf_analysis tool: {}", e);
                }
            }

            // Register image analysis tool if enabled
            if config.tools.image_enabled {
                let api_base = config
                    .agents
                    .api_base
                    .clone()
                    .unwrap_or_else(|| "https://api.anthropic.com".to_string());
                let img_tool = crate::tools::image::ImageAnalysisTool::new(
                    config.agents.api_key.clone(),
                    api_base,
                    config.agents.llm_model.clone(),
                );
                if let Err(e) = tool_registry.register(Arc::new(img_tool)) {
                    error!("Failed to register image_analysis tool: {}", e);
                }
            }
        }

        // Discover skills if enabled
        let skills_context = if config.skills.enabled {
            let mut registry = SkillRegistry::new();
            let n = registry.discover_skills(&config.skills.paths);
            info!(
                "Skills: enabled ({} paths, {} skills loaded)",
                config.skills.paths.len(),
                n
            );
            registry.build_skill_context(config.skills.max_active)
        } else {
            None
        };

        let stats = MessageStats::new();

        let agent_manager = Arc::new(AgentManager::with_tools_skills_and_stats(
            config.agents.clone(),
            config.tools.clone(),
            tool_registry,
            skills_context,
            Some(stats.clone()),
        ));

        // Build rate limiter
        let rate_limiter = RateLimiter::new();
        if config.gateway.rate_limit.enabled {
            info!(
                "Rate limiting enabled ({} msgs / {}s per user)",
                config.gateway.rate_limit.messages_per_window,
                config.gateway.rate_limit.window_seconds,
            );
        }

        // Build audit logger
        let audit = if config.audit.enabled {
            let dest = if config.audit.path.is_empty() {
                "stderr".to_string()
            } else {
                config.audit.path.clone()
            };
            info!("Audit logging enabled (dest={})", dest);
            Some(AuditLogger::new(&config.audit))
        } else {
            None
        };

        // Build hot config
        let hot_config = Arc::new(RwLock::new(HotConfig::from_config(&config)));

        // Build cron scheduler if jobs are configured
        let cron_scheduler = if !config.cron.jobs.is_empty() {
            Some(CronScheduler::new(
                config.cron.jobs.clone(),
                message_tx.clone(),
            ))
        } else {
            None
        };

        let skills_config = config.skills.clone();

        // Build deduplicator
        let deduplicator = if config.gateway.deduplication.enabled {
            info!(
                "Message deduplication enabled (window={})",
                config.gateway.deduplication.window_size
            );
            Some(Arc::new(Mutex::new(MessageDeduplicator::new(
                config.gateway.deduplication.window_size,
            ))))
        } else {
            None
        };

        // Build chunker
        let chunker = if config.gateway.chunking_enabled {
            info!("Message chunking enabled");
            Some(Arc::new(MessageChunker::new(
                config.gateway.chunking_limits.clone(),
            )))
        } else {
            None
        };

        // Build formatter
        let formatter = Arc::new(ResponseFormatter::new(config.gateway.formatting_enabled));

        let auto_route_attachments = config.gateway.auto_route_attachments;

        let quarry =
            Arc::new(QuarrySupervisor::new(config.quarry.clone()).with_audit(audit.clone()));
        let quarry_approvals = Arc::new(ApprovalRegistry::new().with_audit(audit.clone()));
        if config.quarry.enabled {
            info!(
                "quarry runs enabled (binary={}, max_concurrent={})",
                config.quarry.binary_path, config.quarry.max_concurrent_runs
            );
        }

        Self {
            config,
            channels: Arc::new(RwLock::new(Vec::new())),
            memory,
            summarizer,
            agent_manager,
            user_prefs: Arc::new(UserPreferences::new()),
            stats,
            event_tx,
            _event_rx: event_rx,
            tasks: Vec::new(),
            message_tx,
            message_rx: Some(message_rx),
            rate_limiter,
            audit,
            hot_config,
            cron_scheduler,
            skills_config,
            deduplicator,
            chunker,
            formatter,
            auto_route_attachments,
            quarry,
            quarry_approvals,
        }
    }

    /// The pending plan-approval registry.
    ///
    /// A quarry caller registers here and the message pipeline offers every inbound
    /// message to it, which is how a chat reply reaches a suspended run. Nothing is
    /// persisted: a restart drops pending approvals rather than resuming them, since
    /// an approval given before a restart was given against a policy that may since
    /// have changed.
    pub fn quarry_approvals(&self) -> Arc<ApprovalRegistry> {
        Arc::clone(&self.quarry_approvals)
    }

    /// A responder that delivers to `channel_id` through the normal outbound path.
    pub fn responder(&self, channel_id: &str) -> GatewayResponder {
        GatewayResponder {
            channels: Arc::clone(&self.channels),
            stats: Arc::clone(&self.stats),
            channel_id: channel_id.to_string(),
            formatter: Arc::clone(&self.formatter),
            chunker: self.chunker.clone(),
        }
    }

    /// The quarry run supervisor.
    pub fn quarry(&self) -> Arc<QuarrySupervisor> {
        Arc::clone(&self.quarry)
    }

    /// Resolve what a sender may spend, and the scope their run carries.
    ///
    /// Built per call from the **hot config** rather than held as a field, so a
    /// SIGHUP that tightens a cap applies to the next run without a restart. The
    /// cost is a small clone per run, against a policy that would otherwise be
    /// frozen at startup — a bad trade in the other direction, since an operator who
    /// must restart to tighten a cap will not tighten it.
    pub async fn quarry_policy(&self) -> crate::quarry::ConfigCapsPolicy {
        let policy = self.hot_config.read().await.quarry_policy.clone();
        crate::quarry::ConfigCapsPolicy::new(policy).with_audit(self.audit.clone())
    }

    /// The environment a quarry child receives: the localhost `/v1` endpoint and its
    /// bearer token, and nothing else.
    ///
    /// The token is read from the hot config for the same reason the middleware
    /// reads it there — a SIGHUP-rotated token must not leave the child holding the
    /// old one and failing every call.
    pub async fn quarry_child_env(
        &self,
    ) -> Result<std::collections::BTreeMap<String, String>, crate::quarry::PolicyRefusal> {
        let token = self.hot_config.read().await.api_token.clone();
        let url = format!("http://127.0.0.1:{}/v1", self.config.gateway.http_port);
        crate::quarry::policy::mint_child_env(token.as_deref(), &url)
    }

    /// The timezone a sender's deadlines resolve in, with its provenance.
    ///
    /// Walks the documented chain — stored preference, then `quarry.default_timezone`,
    /// then UTC — and reports which step supplied the answer so the resolved instant
    /// can be echoed back with an honest source. One implementation on purpose: a
    /// second copy of this chain would eventually disagree with the disclosure the
    /// sender is shown, which is worse than either fallback.
    pub async fn sender_timezone(&self, user_id: &str) -> crate::quarry::SenderTimezone {
        let stored = self.user_prefs.timezone(user_id).await;
        let default = &self.config.quarry.default_timezone;
        crate::quarry::SenderTimezone::resolve(
            stored.as_deref(),
            if default.is_empty() {
                None
            } else {
                Some(default.as_str())
            },
        )
    }

    /// Returns a sender for delivering inbound messages to this gateway.
    pub fn message_sender(&self) -> mpsc::UnboundedSender<Message> {
        self.message_tx.clone()
    }

    /// Returns a reference to the user preferences store.
    pub fn user_prefs(&self) -> Arc<UserPreferences> {
        self.user_prefs.clone()
    }

    /// Returns a handle to the hot-reloadable config (for SIGHUP handler in main).
    pub fn hot_config_handle(&self) -> Arc<RwLock<HotConfig>> {
        self.hot_config.clone()
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
            let sms = crate::channels::sms::SmsChannel::new("sms-main".to_string(), sms_config);
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
        let webchat_sessions =
            if let Some(wc_config) = self.config.channels.webchat.clone().filter(|c| c.enabled) {
                info!("Setting up webchat channel");
                let wc = crate::channels::webchat::WebchatChannel::new(
                    "webchat-main".to_string(),
                    wc_config,
                );
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

        // Add Microsoft Teams channel if enabled
        if let Some(teams_config) = self.config.channels.teams.clone().filter(|c| c.enabled) {
            info!("Setting up Microsoft Teams channel (webhook mode)");
            let teams =
                crate::channels::teams::TeamsChannel::new("teams-main".to_string(), teams_config);
            self.register_channel(Box::new(teams)).await;
        }

        // Add test channel if enabled.
        //
        // The gateway owns the channel; the HTTP layer gets a handle to the same
        // captured-message buffer. Constructing a second `TestChannel` here would
        // give HTTP a buffer nothing ever writes to.
        let test_channel_handle = if self.config.channels.test_channel {
            info!("Setting up zero-credential test channel (POST /test/send, GET /test/responses)");
            let channel = crate::channels::testchan::TestChannel::new("testchan-main".to_string());
            let captured = channel.captured_handle();
            self.register_channel(Box::new(channel)).await;
            Some(captured)
        } else {
            None
        };

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
                                    error!(
                                        "Failed to list tools from MCP server '{}': {}",
                                        server_cfg.name, e
                                    );
                                    vec![]
                                })
                        }
                        Err(e) => {
                            error!(
                                "Failed to initialize MCP server '{}': {}",
                                server_cfg.name, e
                            );
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
                                    error!(
                                        "Failed to list tools from MCP server '{}': {}",
                                        server_cfg.name, e
                                    );
                                    vec![]
                                })
                        }
                        Err(e) => {
                            error!(
                                "Failed to initialize MCP server '{}': {}",
                                server_cfg.name, e
                            );
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

        let webchat_tx = webchat_sessions.as_ref().map(|_| self.message_tx.clone());

        let teams_tx = self
            .config
            .channels
            .teams
            .as_ref()
            .filter(|c| c.enabled)
            .map(|_| self.message_tx.clone());

        // Pre-compute dashboard basic-auth header value if password is set
        let dashboard_expected_auth = self.config.dashboard.auth_password.as_deref().map(|pw| {
            use base64::Engine;
            let encoded =
                base64::engine::general_purpose::STANDARD.encode(format!("rustynail:{}", pw));
            format!("Basic {}", encoded)
        });

        // Snapshot cron job statuses for HTTP layer
        let cron_jobs = self
            .cron_scheduler
            .as_ref()
            .map(|s| s.job_status())
            .unwrap_or_default();

        // Start HTTP server
        let http_cfg = http::HttpServerConfig {
            port: self.config.gateway.http_port,
            max_body_bytes: self.config.gateway.max_body_bytes,
            request_timeout_seconds: self.config.gateway.request_timeout_seconds,
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
            teams_tx,
            teams_hmac_secret: self
                .config
                .channels
                .teams
                .as_ref()
                .map(|t| t.auth.hmac_secret.clone())
                .unwrap_or_default(),
            user_prefs: self.user_prefs.clone(),
            stats: self.stats.clone(),
            dashboard_expected_auth,
            api_token: self.config.gateway.api_token.clone(),
            test_tx: test_channel_handle
                .as_ref()
                .map(|_| self.message_tx.clone()),
            test_channel: test_channel_handle,
            rate_limiter: self.rate_limiter.clone(),
            audit: self.audit.clone(),
            hot_config: self.hot_config.clone(),
            skills_config: self.skills_config.clone(),
            cron_jobs,
            allowed_ws_origins: self.config.gateway.allowed_ws_origins.clone(),
        };

        let http_task = tokio::spawn(async move {
            if let Err(e) = http::start_http_server(http_cfg).await {
                error!("HTTP server error: {}", e);
            }
        });
        self.tasks.push(http_task);

        // Start cron scheduler
        if let Some(ref mut scheduler) = self.cron_scheduler {
            scheduler.start();
        }

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
            let rate_limiter = self.rate_limiter.clone();
            let audit = self.audit.clone();
            let hot_config = self.hot_config.clone();
            let deduplicator = self.deduplicator.clone();
            let chunker = self.chunker.clone();
            let formatter = self.formatter.clone();
            let auto_route_attachments = self.auto_route_attachments;
            let quarry_approvals = self.quarry_approvals.clone();

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
                        Some(rate_limiter.clone()),
                        audit.clone(),
                        Some(hot_config.clone()),
                        deduplicator.clone(),
                        chunker.clone(),
                        formatter.clone(),
                        auto_route_attachments,
                        Some(quarry_approvals.clone()),
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

        // quarry run-record retention. Only spawned when quarry is enabled and at
        // least one limit is set — an unconditional reaper would wake hourly forever
        // on an installation that does not use quarry.
        //
        // Reaping on a timer rather than after each run, because the thing being
        // bounded is disk over time, and a run that has just finished is the one a
        // caller is most likely about to read.
        if self.config.quarry.enabled
            && (self.config.quarry.retention_max_runs > 0
                || self.config.quarry.retention_max_age_seconds > 0)
        {
            let quarry = Arc::clone(&self.quarry);
            let reaper = tokio::spawn(async move {
                let mut ticker = tokio::time::interval(Duration::from_secs(3600));
                // The first tick fires immediately, which is wanted: a restart is
                // when records left by a previous process are most likely overdue.
                loop {
                    ticker.tick().await;
                    match quarry.reap_run_dirs().await {
                        Ok(0) => {}
                        Ok(n) => info!(
                            "Reaped {n} expired quarry run director{}",
                            if n == 1 { "y" } else { "ies" }
                        ),
                        Err(e) => warn!("quarry run-record retention failed: {e}"),
                    }
                }
            });
            self.tasks.push(reaper);
        }

        info!("Gateway started successfully");
        Ok(())
    }

    /// Stop the gateway and all channels.
    pub async fn stop(&mut self) -> Result<()> {
        info!("Stopping gateway");

        // Stop cron scheduler first
        if let Some(ref mut scheduler) = self.cron_scheduler {
            scheduler.stop();
        }

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

        // quarry runs are given a bounded chance to finish before the tasks that
        // own them are aborted. Aborting a run mid-flight kills the child and
        // discards the record it was about to write — a run that already spent real
        // money and had nothing to show for it. `shutdown_timeout_seconds` is the
        // same budget the rest of shutdown works to, so a slow drain here cannot
        // stall a deploy indefinitely.
        //
        // A run still in flight when the budget runs out is aborted, not waited for:
        // `kill_on_drop` closes it out, and the supervisor reports it as
        // cancellation — time truncation, which is exactly what a shutdown is.
        let active = self.quarry.active_runs();
        if active > 0 {
            let budget = Duration::from_secs(self.config.gateway.shutdown_timeout_seconds);
            info!("Waiting up to {budget:?} for {active} quarry run(s) to finish");
            let deadline = tokio::time::Instant::now() + budget;
            while self.quarry.active_runs() > 0 && tokio::time::Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            let remaining = self.quarry.active_runs();
            if remaining > 0 {
                warn!(
                    "{remaining} quarry run(s) still in flight at shutdown; \
                     their children will be killed and their records lost"
                );
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
            Some(self.rate_limiter.clone()),
            self.audit.clone(),
            Some(self.hot_config.clone()),
            self.deduplicator.clone(),
            self.chunker.clone(),
            self.formatter.clone(),
            self.auto_route_attachments,
            Some(self.quarry_approvals.clone()),
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

    /// Return a clone of the agent manager (for admin API handlers).
    pub fn agent_manager(&self) -> Arc<AgentManager> {
        self.agent_manager.clone()
    }

    /// Return the skills config (for admin skills reload).
    pub fn skills_config(&self) -> &SkillsConfig {
        &self.skills_config
    }
}

/// Public entry point for integration tests — delegates to `handle_message_inner`.
///
/// Pipeline extras (rate-limiter, audit, dedup, chunker, formatter) default to
/// disabled/absent so that existing test call-sites do not need to change.
pub async fn handle_message_for_test(
    memory: &Arc<dyn MemoryStore>,
    agent_manager: &Arc<AgentManager>,
    channels: &Arc<RwLock<Vec<Box<dyn Channel>>>>,
    user_prefs: &Arc<user_prefs::UserPreferences>,
    stats: &Arc<MessageStats>,
    message: Message,
) -> Result<()> {
    let formatter = Arc::new(ResponseFormatter::new(false));
    handle_message_inner(
        memory,
        &None,
        agent_manager,
        channels,
        user_prefs,
        stats,
        message,
        None,
        None,
        None,
        None,
        None,
        formatter,
        false,
        None,
    )
    .await
}

/// Core message-handling logic shared between the internal loop and public method.
#[allow(clippy::too_many_arguments)]
async fn handle_message_inner(
    memory: &Arc<dyn MemoryStore>,
    summarizer: &Option<Arc<MemorySummarizer>>,
    agent_manager: &Arc<AgentManager>,
    channels: &Arc<RwLock<Vec<Box<dyn Channel>>>>,
    user_prefs: &Arc<UserPreferences>,
    stats: &Arc<MessageStats>,
    mut message: Message,
    rate_limiter: Option<Arc<RateLimiter>>,
    audit: Option<Arc<AuditLogger>>,
    hot_config: Option<Arc<RwLock<HotConfig>>>,
    deduplicator: Option<Arc<Mutex<MessageDeduplicator>>>,
    chunker: Option<Arc<MessageChunker>>,
    formatter: Arc<ResponseFormatter>,
    auto_route_attachments: bool,
    quarry_approvals: Option<Arc<ApprovalRegistry>>,
) -> Result<()> {
    info!(
        "Handling message from {} in channel {}",
        message.username, message.channel_id
    );

    // Whether this sender owes an answer to a plan gate. Resolved up front because
    // it changes what two later stages are allowed to do with the message.
    let awaiting_approval = match &quarry_approvals {
        Some(reg) => reg.has_pending(&message.user_id, &message.channel_id).await,
        None => false,
    };

    // ── Deduplication ─────────────────────────────────────────────────────────
    //
    // An approval reply is exempt. It is one word, so a sender who approves two runs
    // in a session sends byte-identical content twice — which the ring buffer reads
    // as a repeat and drops, leaving the second run to expire unapproved with
    // nothing to explain it. The exemption is narrow: it applies only while that
    // sender has an approval outstanding on that channel, of which there is at most
    // one.
    if let Some(ref dedup) = deduplicator {
        if !awaiting_approval && dedup.lock().await.seen(&message.user_id, &message.content) {
            tracing::debug!("Duplicate message from '{}', dropping", message.user_id);
            return Ok(());
        }
    }

    // ── Audit: message received ───────────────────────────────────────────────
    if let Some(ref al) = audit {
        al.log(AuditEvent::MessageReceived {
            user_id: message.user_id.clone(),
            channel_id: message.channel_id.clone(),
            bytes: message.content.len(),
        });
    }

    // ── quarry plan-gate reply ────────────────────────────────────────────────
    //
    // Offered before the rate limiter on purpose. A `no` that gets rate-limited is a
    // run the sender tried to cancel and could not, and cancelling must never be the
    // thing that fails. The exemption cannot be used as a bypass: settling clears
    // the pending entry, so the next message from this sender takes the normal path
    // with the limiter in it, and there is at most one pending approval per sender
    // per channel.
    if awaiting_approval {
        // `awaiting_approval` implies the registry is present.
        let reg = quarry_approvals
            .as_ref()
            .expect("pending implies a registry");
        match reg
            .submit_reply(&message.user_id, &message.channel_id, &message.content)
            .await
        {
            ReplyOutcome::Settled {
                request_id,
                decision,
            } => {
                // The waiting run sends its own acknowledgement through the same
                // path this function would, so saying anything here would double it.
                info!(
                    "quarry plan gate {request_id} settled as {} by {}",
                    decision.code(),
                    message.user_id
                );
                return Ok(());
            }
            ReplyOutcome::NeedsClarification { request_id } => {
                // Re-prompt and swallow. Falling through to the agent would answer
                // "maybe" with a chat completion while the sender is mid-decision,
                // and the run is still waiting either way.
                tracing::debug!("quarry plan gate {request_id}: reply not understood");
                send_text(
                    channels,
                    stats,
                    &message.channel_id,
                    &crate::quarry::render_clarification(),
                    &formatter,
                    &chunker,
                )
                .await?;
                return Ok(());
            }
            // Raced: the approval settled or expired between the check above and
            // here. Not a reply to anything, so it is an ordinary message.
            ReplyOutcome::NothingPending => {}
        }
    }

    // ── Per-user rate limiting ────────────────────────────────────────────────
    if let (Some(ref rl), Some(ref hc)) = (&rate_limiter, &hot_config) {
        let config = hc.read().await;
        if !rl.check_and_record(&message.user_id, &config.rate_limit) {
            warn!(
                "Rate limit exceeded for user '{}' in channel '{}'",
                message.user_id, message.channel_id
            );
            stats.record_rate_limit_hit();
            if let Some(ref al) = audit {
                al.log(AuditEvent::RateLimitHit {
                    user_id: message.user_id.clone(),
                    channel_id: message.channel_id.clone(),
                });
            }
            // Send friendly rate-limit message back through the originating channel
            let deny = Message::new(
                message.channel_id.clone(),
                "assistant".to_string(),
                "RustyNail".to_string(),
                "⚠️ Rate limit exceeded. Please wait before sending another message.".to_string(),
            );
            let channels = channels.read().await;
            for channel in channels.iter() {
                if channel.id() == message.channel_id {
                    let _ = channel.send_message(deny).await;
                    break;
                }
            }
            return Ok(());
        }
    }

    // Resolve the channel to route the response to
    let response_channel_id = if let Some(ref preferred) = message.preferred_channel_id {
        preferred.clone()
    } else if let Some(pref) = user_prefs.get(&message.user_id).await {
        pref
    } else {
        message.channel_id.clone()
    };

    // ── Attachment auto-routing ───────────────────────────────────────────────
    if auto_route_attachments && !message.attachments.is_empty() {
        let mut prefix = String::new();
        for attachment in &message.attachments {
            match attachment.media_type.as_str() {
                "pdf" => {
                    prefix.push_str(&format!("Please analyze this PDF: {}\n\n", attachment.url));
                }
                "image" => {
                    prefix.push_str(&format!(
                        "Please describe this image: {}\n\n",
                        attachment.url
                    ));
                }
                _ => {}
            }
        }
        if !prefix.is_empty() {
            message.content = format!("{}{}", prefix, message.content);
        }
    }

    // Track in memory store + stats
    memory.add_message(&message.user_id, format!("User: {}", message.content));

    // Maybe summarise history (fire-and-forget)
    if let Some(ref s) = summarizer {
        s.maybe_summarize(memory.clone(), &message.user_id);
    }

    stats.record_inbound_async(&message).await;

    // ── Process with agent ────────────────────────────────────────────────────
    let processing_start = std::time::Instant::now();
    let response_content = match agent_manager
        .process_message(&message.user_id, &message.content)
        .instrument(tracing::info_span!("agent.process", user_id = %message.user_id))
        .await
    {
        Ok(text) => text,
        Err(e) => {
            error!("LLM error for user '{}': {}", message.user_id, e);
            stats.record_llm_error();
            if let Some(ref al) = audit {
                al.log(AuditEvent::LlmError {
                    user_id: message.user_id.clone(),
                    error: e.to_string(),
                });
            }
            "I'm having trouble responding right now. Please try again in a moment.".to_string()
        }
    };
    stats.observe_message_duration(processing_start.elapsed().as_secs_f64());

    memory.add_message(&message.user_id, format!("Assistant: {}", response_content));

    // ── Format, chunk and send ───────────────────────────────────────────────
    send_text(
        channels,
        stats,
        &response_channel_id,
        &response_content,
        &formatter,
        &chunker,
    )
    .await
}

/// A [`crate::quarry::Responder`] that sends through the gateway's own outbound path.
///
/// The plan gate needs the formatter and the chunker applied, and it needs them
/// applied by the *same* code that formats an agent reply — a second copy would
/// eventually drift, and the drift would show up as a plan message truncated on the
/// one platform nobody tests by hand.
pub struct GatewayResponder {
    channels: Arc<RwLock<Vec<Box<dyn Channel>>>>,
    stats: Arc<MessageStats>,
    channel_id: String,
    formatter: Arc<ResponseFormatter>,
    chunker: Option<Arc<MessageChunker>>,
}

#[async_trait::async_trait]
impl crate::quarry::Responder for GatewayResponder {
    async fn reply(&self, text: &str) -> Result<()> {
        send_text(
            &self.channels,
            &self.stats,
            &self.channel_id,
            text,
            &self.formatter,
            &self.chunker,
        )
        .await
    }
}

/// Format, chunk and deliver `text` on `channel_id`.
///
/// The single outbound path. Extracted so the plan gate's messages take exactly the
/// same route as an agent reply — a plan sent around the chunker arrives truncated on
/// Teams' 1024-byte limit, which is how a sender ends up approving a plan whose
/// limits were cut off the bottom.
async fn send_text(
    channels: &Arc<RwLock<Vec<Box<dyn Channel>>>>,
    stats: &Arc<MessageStats>,
    channel_id: &str,
    text: &str,
    formatter: &Arc<ResponseFormatter>,
    chunker: &Option<Arc<MessageChunker>>,
) -> Result<()> {
    let formatted = formatter.format(text, channel_id);
    let chunks = match chunker {
        Some(c) => c.chunk(channel_id, &formatted),
        None => vec![formatted],
    };

    let channels = channels.read().await;
    for channel in channels.iter() {
        if channel.id() == channel_id {
            for chunk in &chunks {
                let response = Message::new(
                    channel_id.to_string(),
                    "assistant".to_string(),
                    "RustyNail".to_string(),
                    chunk.clone(),
                );
                channel.send_message(response.clone()).await?;
                stats.record_outbound_async(&response).await;
            }
            return Ok(());
        }
    }

    error!("No channel found with id '{}' to send response", channel_id);
    Ok(())
}

/// Expand `~/...` path using the HOME environment variable.
fn dirs_path_expand(rest: &str) -> String {
    std::env::var("HOME")
        .map(|home| format!("{}/{}", home, rest))
        .unwrap_or_else(|_| format!("/tmp/{}", rest))
}

/// Extended test entry point that exposes optional pipeline components.
///
/// Unlike `handle_message_for_test`, this variant lets tests control rate-limiting,
/// deduplication, chunking, and formatting, enabling full-pipeline integration tests.
///
/// When `rate_limiter` and `rate_limit_config` are both `Some`, rate limiting is
/// enforced using the supplied config.
#[allow(clippy::too_many_arguments)]
pub async fn handle_message_for_test_full(
    memory: &Arc<dyn MemoryStore>,
    agent_manager: &Arc<AgentManager>,
    channels: &Arc<RwLock<Vec<Box<dyn Channel>>>>,
    user_prefs: &Arc<user_prefs::UserPreferences>,
    stats: &Arc<MessageStats>,
    message: Message,
    rate_limiter: Option<Arc<RateLimiter>>,
    rate_limit_config: Option<crate::config::RateLimitConfig>,
    deduplicator: Option<Arc<Mutex<MessageDeduplicator>>>,
    chunker: Option<Arc<MessageChunker>>,
    formatting_enabled: bool,
) -> Result<()> {
    let formatter = Arc::new(ResponseFormatter::new(formatting_enabled));

    // Build a HotConfig when a rate_limit_config is provided so the rate limiter fires.
    let hot_config = rate_limit_config.map(|rlc| {
        Arc::new(RwLock::new(HotConfig {
            log_level: "info".to_string(),
            api_token: None,
            rate_limit: rlc,
            audit_enabled: false,
            audit_path: String::new(),
            // Default-deny: these tests are not about quarry, and an empty policy
            // grants nobody a run.
            quarry_policy: crate::config::QuarryPolicyConfig::default(),
        }))
    });

    handle_message_inner(
        memory,
        &None,
        agent_manager,
        channels,
        user_prefs,
        stats,
        message,
        rate_limiter,
        None,
        hot_config,
        deduplicator,
        chunker,
        formatter,
        false,
        // No plan gate: this entry point exists for the pipeline tests, and
        // `tests/quarry_plan_gate.rs` drives the gate through the registry directly.
        None,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AgentsConfig, AuditConfig, ChannelsConfig, Config, CronConfig, DeduplicationConfig,
        GatewayConfig, McpConfig, MemoryConfig, OtelConfig, QuarryConfig, RateLimitConfig,
        SkillsConfig, ToolsConfig,
    };

    fn test_config(log_level: &str, api_token: Option<&str>, audit_enabled: bool) -> Config {
        Config {
            gateway: GatewayConfig {
                log_level: log_level.to_string(),
                api_token: api_token.map(|s| s.to_string()),
                http_port: 8080,
                websocket_port: 18789,
                rate_limit: RateLimitConfig::default(),
                max_body_bytes: 1_048_576,
                request_timeout_seconds: 30,
                allowed_ws_origins: Vec::new(),
                shutdown_timeout_seconds: 30,
                chunking_enabled: false,
                chunking_limits: std::collections::HashMap::new(),
                formatting_enabled: false,
                auto_route_attachments: false,
                deduplication: DeduplicationConfig::default(),
            },
            channels: ChannelsConfig {
                discord: None,
                whatsapp: None,
                telegram: None,
                slack: None,
                sms: None,
                webhook: None,
                webchat: None,
                email: None,
                teams: None,
                test_channel: false,
            },
            agents: AgentsConfig::default(),
            tools: ToolsConfig::default(),
            otel: OtelConfig::default(),
            dashboard: Default::default(),
            memory: MemoryConfig::default(),
            mcp: McpConfig::default(),
            skills: SkillsConfig::default(),
            audit: AuditConfig {
                enabled: audit_enabled,
                path: String::new(),
            },
            cron: CronConfig::default(),
            quarry: QuarryConfig::default(),
        }
    }

    #[test]
    fn test_hotconfig_from_config() {
        let cfg = test_config("debug", Some("tok123"), false);
        let hc = HotConfig::from_config(&cfg);
        assert_eq!(hc.log_level, "debug");
        assert_eq!(hc.api_token, Some("tok123".to_string()));
        assert!(!hc.audit_enabled);
    }

    #[test]
    fn test_hotconfig_apply_detects_changes() {
        let cfg = test_config("info", None, false);
        let mut hc = HotConfig::from_config(&cfg);
        let mut new_cfg = cfg.clone();
        new_cfg.gateway.log_level = "debug".to_string();
        let changed = hc.apply(&new_cfg);
        assert!(changed.contains(&"log_level".to_string()));
        assert_eq!(hc.log_level, "debug");
    }

    #[test]
    fn a_sighup_can_tighten_a_quarry_cap_without_a_restart() {
        // Policy has to be reloadable, because an operator who must restart the
        // gateway to lower a spend cap will not lower it. The reload is asserted on
        // the value, not just on the changed-field name — a `changed` entry with a
        // stale value would look like a working reload in the logs.
        let cfg = test_config("info", None, false);
        let mut hc = HotConfig::from_config(&cfg);
        assert!(
            hc.quarry_policy.default.is_none(),
            "a default config must grant nobody a run"
        );

        let mut new_cfg = cfg.clone();
        new_cfg.quarry.policy.default = Some(crate::config::QuarryPolicyEntry {
            allowed_denominations: vec!["spend".into()],
            max_spend_micro_usd: Some(500_000),
            ..Default::default()
        });
        let changed = hc.apply(&new_cfg);
        assert!(changed.contains(&"quarry.policy".to_string()));
        assert_eq!(
            hc.quarry_policy
                .default
                .as_ref()
                .unwrap()
                .max_spend_micro_usd,
            Some(500_000)
        );

        // And a reload that revokes the entry restores default-deny rather than
        // leaving the last permissive policy in place.
        let revoked = test_config("info", None, false);
        let changed = hc.apply(&revoked);
        assert!(changed.contains(&"quarry.policy".to_string()));
        assert!(hc.quarry_policy.default.is_none());
    }

    #[test]
    fn test_hotconfig_apply_ignores_no_change() {
        let cfg = test_config("info", None, false);
        let mut hc = HotConfig::from_config(&cfg);
        let changed = hc.apply(&cfg);
        assert!(changed.is_empty());
    }

    #[tokio::test]
    async fn quarry_supervisor_exists_even_when_disabled() {
        // Disabled must mean "refuses with a reason", not "capability absent". A
        // caller that had to distinguish `None` from a refusal would have to guess
        // whether the operator turned it off or the build lacks it.
        let cfg = test_config("info", None, false);
        assert!(
            !cfg.quarry.enabled,
            "disabled is the default: quarry spends money"
        );
        let gw = Gateway::new(cfg);
        let q = gw.quarry();
        assert!(!q.enabled());
        assert_eq!(q.active_runs(), 0);
    }

    #[tokio::test]
    async fn the_sender_timezone_chain_prefers_the_sender_then_the_operator_then_utc() {
        use crate::quarry::TimezoneSource;

        let mut cfg = test_config("info", None, false);
        cfg.quarry.default_timezone = "America/Denver".to_string();
        let gw = Gateway::new(cfg);

        // Nothing stored: the operator default, reported as such.
        let tz = gw.sender_timezone("alice").await;
        assert_eq!(tz.tz, chrono_tz::America::Denver);
        assert_eq!(tz.source, TimezoneSource::ConfigDefault);

        // Stored preference wins, and the source says so — a deadline resolved in
        // the sender's own zone is a different claim from one resolved in the
        // operator's, and only the sender can tell which is right.
        gw.user_prefs.set_timezone("alice", "Asia/Tokyo").await;
        let tz = gw.sender_timezone("alice").await;
        assert_eq!(tz.tz, chrono_tz::Asia::Tokyo);
        assert_eq!(tz.source, TimezoneSource::SenderPreference);

        // Another sender is unaffected.
        let tz = gw.sender_timezone("bob").await;
        assert_eq!(tz.tz, chrono_tz::America::Denver);
    }

    #[tokio::test]
    async fn an_empty_default_timezone_falls_all_the_way_to_utc_and_says_so() {
        use crate::quarry::TimezoneSource;

        // An empty config string must not be handed to the parser as a zone name:
        // it would fail, land on UTC anyway, and report ConfigDefault — claiming an
        // operator setting that does not exist.
        let cfg = test_config("info", None, false);
        assert!(cfg.quarry.default_timezone.is_empty());
        let gw = Gateway::new(cfg);
        let tz = gw.sender_timezone("alice").await;
        assert_eq!(tz.tz, chrono_tz::UTC);
        assert_eq!(tz.source, TimezoneSource::UtcFallback);
    }

    #[tokio::test]
    async fn the_quarry_policy_is_read_from_the_hot_config_not_frozen_at_startup() {
        use crate::quarry::CapsPolicy;

        // Built from `hot_config` per call, so a SIGHUP that grants or tightens a
        // policy applies to the next run. Held as a startup field it would be frozen
        // until a restart, which is how a cap nobody can lower comes about.
        let cfg = test_config("info", None, false);
        let gw = Gateway::new(cfg.clone());

        let asked = crate::quarry::RequestedCaps {
            spend_micro_usd: Some(400_000),
            ..Default::default()
        };
        assert_eq!(
            gw.quarry_policy()
                .await
                .resolve("alice", "discord-1", &asked)
                .unwrap_err()
                .code(),
            "no_policy",
            "an unconfigured gateway must deny by default"
        );

        let mut granted = cfg.clone();
        granted.quarry.policy.default = Some(crate::config::QuarryPolicyEntry {
            allowed_denominations: vec!["spend".into()],
            max_spend_micro_usd: Some(200_000),
            on_over_limit: "reduce".into(),
            ..Default::default()
        });
        gw.hot_config.write().await.apply(&granted);

        let grant = gw
            .quarry_policy()
            .await
            .resolve("alice", "discord-1", &asked)
            .expect("the reloaded policy should permit this");
        assert_eq!(grant.spend_micro_usd, Some(200_000));
        assert_eq!(grant.scope.key(), "channel=discord-1;user=alice;");
    }

    #[tokio::test]
    async fn the_child_env_is_refused_without_a_token_and_carries_only_it_with_one() {
        let cfg = test_config("info", None, false);
        let gw = Gateway::new(cfg.clone());

        // No token configured means /v1 is unauthenticated; there is no credential
        // to mint and no point pretending otherwise.
        assert_eq!(
            gw.quarry_child_env().await.unwrap_err().code(),
            "no_provider_token"
        );

        // A SIGHUP-rotated token must reach the next child, or every call it makes
        // fails with the old one.
        let mut with_token = cfg.clone();
        with_token.gateway.api_token = Some("rotated".to_string());
        gw.hot_config.write().await.apply(&with_token);

        let env = gw.quarry_child_env().await.expect("token minted");
        assert_eq!(env["QUARRY_PROVIDER_TOKEN"], "rotated");
        assert!(env["QUARRY_PROVIDER_URL"].ends_with("/v1"));
        assert_eq!(env.len(), 2, "the child gets nothing else: {env:?}");
    }

    #[tokio::test]
    async fn shutdown_returns_promptly_when_no_quarry_run_is_in_flight() {
        // The drain loop must not cost a deploy anything when there is nothing to
        // drain — a 30-second budget polled unconditionally would add 30 seconds to
        // every shutdown.
        let mut cfg = test_config("info", None, false);
        cfg.gateway.shutdown_timeout_seconds = 30;
        let mut gw = Gateway::new(cfg);
        let started = std::time::Instant::now();
        gw.stop().await.expect("stop succeeds");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "shutdown waited on an empty run set: {:?}",
            started.elapsed()
        );
    }
}
