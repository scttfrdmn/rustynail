use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub gateway: GatewayConfig,
    pub channels: ChannelsConfig,
    pub agents: AgentsConfig,
    #[serde(default)]
    pub tools: ToolsConfig,
    #[serde(default)]
    pub otel: OtelConfig,
    #[serde(default)]
    pub dashboard: DashboardConfig,
    #[serde(default)]
    pub memory: MemoryConfig,
    #[serde(default)]
    pub mcp: McpConfig,
    #[serde(default)]
    pub skills: SkillsConfig,
    /// Structured audit log configuration.
    #[serde(default)]
    pub audit: AuditConfig,
    /// Cron job scheduler configuration.
    #[serde(default)]
    pub cron: CronConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayConfig {
    #[serde(default = "default_websocket_port")]
    pub websocket_port: u16,

    #[serde(default = "default_http_port")]
    pub http_port: u16,

    #[serde(default = "default_log_level")]
    pub log_level: String,

    /// Optional bearer token for API authentication (env: `GATEWAY_API_TOKEN`).
    /// When `None` or empty, auth is disabled (backward compatible).
    pub api_token: Option<String>,

    /// Per-user sliding-window rate limiting.
    #[serde(default)]
    pub rate_limit: RateLimitConfig,

    /// Maximum request body size in bytes (default 1 MiB). Env: `GATEWAY_MAX_BODY_BYTES`.
    #[serde(default = "default_max_body_bytes")]
    pub max_body_bytes: usize,

    /// Handler timeout in seconds (default 30). Env: `GATEWAY_REQUEST_TIMEOUT_SECONDS`.
    #[serde(default = "default_request_timeout_seconds")]
    pub request_timeout_seconds: u64,

    /// Allowed WebSocket upgrade origins. Empty = allow all. Env: `GATEWAY_ALLOWED_WS_ORIGINS` (comma-separated).
    #[serde(default)]
    pub allowed_ws_origins: Vec<String>,

    /// Graceful shutdown timeout in seconds (default 30). Env: `GATEWAY_SHUTDOWN_TIMEOUT_SECONDS`.
    #[serde(default = "default_shutdown_timeout_seconds")]
    pub shutdown_timeout_seconds: u64,
}

// ── Rate limiting ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfig {
    /// Enable per-user rate limiting (env: `RATE_LIMIT_ENABLED`).
    #[serde(default)]
    pub enabled: bool,

    /// Maximum messages allowed per window (env: `RATE_LIMIT_MESSAGES`).
    #[serde(default = "default_rate_limit_messages")]
    pub messages_per_window: u32,

    /// Sliding window size in seconds (env: `RATE_LIMIT_WINDOW_SECONDS`).
    #[serde(default = "default_rate_limit_window_seconds")]
    pub window_seconds: u64,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            messages_per_window: default_rate_limit_messages(),
            window_seconds: default_rate_limit_window_seconds(),
        }
    }
}

// ── Audit logging ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditConfig {
    /// Enable structured audit logging (env: `AUDIT_ENABLED`).
    #[serde(default)]
    pub enabled: bool,

    /// File path for the NDJSON audit log. Empty = write to stderr.
    /// Env: `AUDIT_PATH`.
    #[serde(default)]
    pub path: String,
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            path: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelsConfig {
    pub discord: Option<DiscordConfig>,
    pub whatsapp: Option<WhatsAppConfig>,
    pub telegram: Option<TelegramConfig>,
    pub slack: Option<SlackConfig>,
    pub sms: Option<SmsConfig>,
    pub webhook: Option<WebhookConfig>,
    pub webchat: Option<WebchatConfig>,
    pub email: Option<EmailConfig>,
    pub teams: Option<TeamsConfig>,
    /// Enable the zero-credential test channel (`POST /test/send`, `GET /test/responses`).
    #[serde(default)]
    pub test_channel: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,

    pub auth: DiscordAuthConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordAuthConfig {
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhatsAppConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default)]
    pub phone_number_id: String,

    #[serde(default)]
    pub access_token: String,

    #[serde(default)]
    pub verify_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default)]
    pub bot_token: String,

    #[serde(default)]
    pub webhook_secret: String,

    /// Receive mode: `"webhook"` (default) or `"longpoll"`.
    #[serde(default = "default_telegram_mode")]
    pub mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlackConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default)]
    pub bot_token: String,

    #[serde(default)]
    pub signing_secret: String,

    /// Socket Mode app-level token (starts with `xapp-`). Required when `mode = "socket"`.
    pub app_token: Option<String>,

    /// Receive mode: `"webhook"` (default) or `"socket"`.
    #[serde(default = "default_slack_mode")]
    pub mode: String,
}

// ── SMS / Twilio ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmsConfig {
    #[serde(default)]
    pub enabled: bool,

    pub auth: SmsAuthConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmsAuthConfig {
    #[serde(default)]
    pub account_sid: String,

    #[serde(default)]
    pub auth_token: String,

    #[serde(default)]
    pub from_number: String,
}

// ── Generic inbound webhook ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default)]
    pub endpoints: Vec<WebhookEndpoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookEndpoint {
    /// Path segment to match, e.g. `"my-system"` → `POST /webhooks/my-system`
    pub path: String,

    /// Optional HMAC-SHA256 secret for signature verification.
    pub secret: Option<String>,

    /// Route all messages from this endpoint as this user_id.
    pub user_id: String,

    /// JSONPath expression to extract the message text from the body.
    /// Falls back to the full body when absent.
    pub extract_text: Option<String>,
}

// ── Web chat widget ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebchatConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default)]
    pub allowed_origins: Vec<String>,

    pub welcome_message: Option<String>,
}

// ── Email (IMAP receive + SMTP send) ─────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailConfig {
    #[serde(default)]
    pub enabled: bool,

    pub imap: ImapConfig,
    pub smtp: SmtpConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImapConfig {
    #[serde(default)]
    pub host: String,

    #[serde(default = "default_imap_port")]
    pub port: u16,

    #[serde(default)]
    pub username: String,

    #[serde(default)]
    pub password: String,

    #[serde(default = "default_inbox")]
    pub inbox: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmtpConfig {
    #[serde(default)]
    pub host: String,

    #[serde(default = "default_smtp_port")]
    pub port: u16,

    #[serde(default)]
    pub username: String,

    #[serde(default)]
    pub password: String,

    #[serde(default)]
    pub from_address: String,
}

// ── Microsoft Teams ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamsConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,

    pub auth: TeamsAuthConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamsAuthConfig {
    #[serde(default)]
    pub app_id: String,

    #[serde(default)]
    pub app_password: String,
}

// ── Skills ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillsConfig {
    #[serde(default)]
    pub enabled: bool,

    /// Directories to search for skill directories (each must contain a SKILL.md).
    #[serde(default = "default_skills_paths")]
    pub paths: Vec<String>,

    /// Maximum number of skills injected into each agent's system prompt.
    #[serde(default = "default_skills_max_active")]
    pub max_active: usize,
}

impl Default for SkillsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            paths: default_skills_paths(),
            max_active: default_skills_max_active(),
        }
    }
}

fn default_skills_paths() -> Vec<String> {
    vec!["skills/".to_string()]
}

fn default_skills_max_active() -> usize {
    3
}

// ── Shell tool ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellToolConfig {
    #[serde(default)]
    pub enabled: bool,

    /// Require the caller to pass `approved=true` before executing (default true).
    #[serde(default = "default_true")]
    pub require_approval: bool,

    /// If non-empty, the command must start with one of these prefixes.
    #[serde(default)]
    pub allowed_commands: Vec<String>,
}

impl Default for ShellToolConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            require_approval: true,
            allowed_commands: Vec::new(),
        }
    }
}

// ── Cron scheduler ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJobConfig {
    /// Human-readable job name used in logs.
    pub name: String,

    /// Interval with suffix: `"30s"`, `"5m"`, `"2h"`, `"1d"`.
    pub schedule: String,

    /// Message text to inject on each tick.
    pub message: String,

    /// Channel ID to route the synthetic message to.
    pub channel_id: String,

    /// User ID to use for the synthetic message.
    pub user_id: String,

    /// Whether this job is active.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CronConfig {
    #[serde(default)]
    pub jobs: Vec<CronJobConfig>,
}

// ── MCP servers ───────────────────────────────────────────────────────────────

/// Configuration for one MCP server connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerEntry {
    /// Human-readable name used in log messages.
    pub name: String,

    /// Transport type: `"stdio"` (default) or `"http"`.
    #[serde(default = "default_mcp_transport")]
    pub transport: String,

    // ── stdio fields ──────────────────────────────────────────────────────────

    /// Command to spawn (stdio transport only).
    pub command: Option<String>,

    /// Arguments for the subprocess.
    #[serde(default)]
    pub args: Vec<String>,

    /// Extra environment variables for the subprocess (`[["KEY", "VALUE"], ...]`).
    #[serde(default)]
    pub env: Vec<(String, String)>,

    // ── http fields ───────────────────────────────────────────────────────────

    /// Base URL of the MCP HTTP server (http transport only).
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct McpConfig {
    /// List of MCP servers to connect to at startup.
    #[serde(default)]
    pub servers: Vec<McpServerEntry>,
}

fn default_mcp_transport() -> String {
    "stdio".to_string()
}

// ── Memory ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    /// Storage backend: `"inmemory"` (default), `"redis"`, `"sqlite"`, `"postgres"`, or `"vector"`.
    #[serde(default = "default_memory_backend")]
    pub backend: String,

    /// Redis connection URL. Required when `backend = "redis"`.
    pub redis_url: Option<String>,

    /// TTL in seconds for Redis history keys. 0 = no expiry.
    #[serde(default = "default_redis_ttl")]
    pub redis_ttl_seconds: u64,

    /// SQLite database file path. Required when `backend = "sqlite"`.
    pub sqlite_path: Option<String>,

    /// PostgreSQL connection URL. Required when `backend = "postgres"`.
    pub postgres_url: Option<String>,

    /// Vector store type: `"memory"` (default) or `"qdrant"`.
    #[serde(default = "default_vector_store")]
    pub vector_store: String,

    /// URL for external vector store (e.g. Qdrant). Optional.
    pub vector_store_url: Option<String>,

    /// Embedding provider: `"simple"` (default, deterministic n-gram embeddings).
    #[serde(default = "default_embedding_provider")]
    pub embedding_provider: String,

    /// Embedding model name (provider-specific, ignored for "simple").
    #[serde(default = "default_embedding_model")]
    pub embedding_model: String,

    /// Memory summarization settings.
    #[serde(default)]
    pub summarization: SummarizationConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummarizationConfig {
    #[serde(default)]
    pub enabled: bool,

    /// Trigger summarization when history exceeds this many messages.
    #[serde(default = "default_summarization_trigger_at")]
    pub trigger_at: usize,

    /// Keep this many recent messages after summarization.
    #[serde(default = "default_summarization_keep_recent")]
    pub keep_recent: usize,

    /// LLM model to use for summarization.
    #[serde(default = "default_summarization_model")]
    pub model: String,
}

impl Default for SummarizationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            trigger_at: default_summarization_trigger_at(),
            keep_recent: default_summarization_keep_recent(),
            model: default_summarization_model(),
        }
    }
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            backend: default_memory_backend(),
            redis_url: None,
            redis_ttl_seconds: default_redis_ttl(),
            sqlite_path: None,
            postgres_url: None,
            vector_store: default_vector_store(),
            vector_store_url: None,
            embedding_provider: default_embedding_provider(),
            embedding_model: default_embedding_model(),
            summarization: SummarizationConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtelConfig {
    pub endpoint: Option<String>,

    #[serde(default = "default_otel_service_name")]
    pub service_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DashboardConfig {
    pub auth_password: Option<String>,
}

impl Default for OtelConfig {
    fn default() -> Self {
        Self {
            endpoint: None,
            service_name: default_otel_service_name(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default = "default_max_steps")]
    pub max_steps: usize,

    pub filesystem_root: Option<String>,

    pub web_search_api_key: Option<String>,

    /// Shell tool configuration (sub-key `tools.shell`).
    #[serde(default)]
    pub shell: ShellToolConfig,

    /// Enable the PDF analysis tool. Env: `TOOLS_PDF_ENABLED`.
    #[serde(default)]
    pub pdf_enabled: bool,

    /// Enable the image analysis tool. Env: `TOOLS_IMAGE_ENABLED`.
    #[serde(default)]
    pub image_enabled: bool,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_steps: default_max_steps(),
            filesystem_root: None,
            web_search_api_key: None,
            shell: ShellToolConfig::default(),
            pdf_enabled: false,
            image_enabled: false,
        }
    }
}

// ── LLM retry ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRetryConfig {
    /// Enable LLM retry with exponential backoff (env: `AGENTS_RETRY_ENABLED`).
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Maximum number of attempts including the first (env: `AGENTS_RETRY_MAX_ATTEMPTS`).
    #[serde(default = "default_retry_max_attempts")]
    pub max_attempts: u32,

    /// Base delay in ms for the first retry (env: `AGENTS_RETRY_BASE_DELAY_MS`).
    #[serde(default = "default_retry_base_delay_ms")]
    pub base_delay_ms: u64,
}

impl Default for AgentRetryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_attempts: default_retry_max_attempts(),
            base_delay_ms: default_retry_base_delay_ms(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentsConfig {
    #[serde(default = "default_llm_provider")]
    pub llm_provider: String,

    #[serde(default = "default_llm_model")]
    pub llm_model: String,

    pub api_key: String,

    #[serde(default = "default_max_history")]
    pub max_history: usize,

    #[serde(default = "default_temperature")]
    pub temperature: f32,

    #[serde(default)]
    pub planning_enabled: bool,

    #[serde(default = "default_planning_max_steps")]
    pub planning_max_steps: usize,

    /// Override the API base URL (e.g. for test mock servers or Ollama).
    /// When `None`, the adapter uses its own default.
    #[serde(default)]
    pub api_base: Option<String>,

    /// AWS region for Bedrock. Defaults to `us-east-1`.
    pub aws_region: Option<String>,

    /// LLM retry configuration.
    #[serde(default)]
    pub retry: AgentRetryConfig,
}

// ── Default value functions ───────────────────────────────────────────────────

fn default_websocket_port() -> u16 {
    18789
}

fn default_max_body_bytes() -> usize {
    1_048_576 // 1 MiB
}

fn default_request_timeout_seconds() -> u64 {
    30
}

fn default_shutdown_timeout_seconds() -> u64 {
    30
}

fn default_rate_limit_messages() -> u32 {
    20
}

fn default_rate_limit_window_seconds() -> u64 {
    60
}

fn default_retry_max_attempts() -> u32 {
    3
}

fn default_retry_base_delay_ms() -> u64 {
    100
}

fn default_http_port() -> u16 {
    8080
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_true() -> bool {
    true
}

fn default_llm_provider() -> String {
    "anthropic".to_string()
}

fn default_llm_model() -> String {
    "claude-3-5-sonnet-20241022".to_string()
}

fn default_max_history() -> usize {
    20
}

fn default_temperature() -> f32 {
    0.7
}

fn default_max_steps() -> usize {
    10
}

fn default_otel_service_name() -> String {
    "rustynail".to_string()
}

fn default_planning_max_steps() -> usize {
    10
}

fn default_memory_backend() -> String {
    "inmemory".to_string()
}

fn default_redis_ttl() -> u64 {
    86400
}

fn default_telegram_mode() -> String {
    "webhook".to_string()
}

fn default_slack_mode() -> String {
    "webhook".to_string()
}

fn default_imap_port() -> u16 {
    993
}

fn default_smtp_port() -> u16 {
    587
}

fn default_inbox() -> String {
    "INBOX".to_string()
}

fn default_vector_store() -> String {
    "memory".to_string()
}

fn default_embedding_provider() -> String {
    "simple".to_string()
}

fn default_embedding_model() -> String {
    "none".to_string()
}

fn default_summarization_trigger_at() -> usize {
    40
}

fn default_summarization_keep_recent() -> usize {
    20
}

fn default_summarization_model() -> String {
    "claude-haiku-4-5-20251001".to_string()
}

impl Default for AgentsConfig {
    fn default() -> Self {
        Self {
            llm_provider: default_llm_provider(),
            llm_model: default_llm_model(),
            api_key: String::new(),
            max_history: default_max_history(),
            temperature: default_temperature(),
            planning_enabled: false,
            planning_max_steps: default_planning_max_steps(),
            api_base: None,
            aws_region: None,
            retry: AgentRetryConfig::default(),
        }
    }
}

impl Config {
    /// Load configuration from a YAML file
    pub fn from_file(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = serde_yaml::from_str(&content)?;
        Ok(config)
    }

    /// Load configuration from environment variables and optional file
    pub fn load() -> anyhow::Result<Self> {
        // Load .env file if present
        let _ = dotenvy::dotenv();

        // Try to load from config file if CONFIG_FILE env var is set
        if let Ok(config_path) = std::env::var("CONFIG_FILE") {
            return Self::from_file(config_path);
        }

        // Build config from environment variables
        let discord = std::env::var("DISCORD_BOT_TOKEN")
            .ok()
            .map(|token| DiscordConfig {
                enabled: true,
                auth: DiscordAuthConfig { token },
            });

        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY not set"))?;

        let whatsapp = if let (Ok(phone_number_id), Ok(access_token), Ok(verify_token)) = (
            std::env::var("WHATSAPP_PHONE_NUMBER_ID"),
            std::env::var("WHATSAPP_ACCESS_TOKEN"),
            std::env::var("WHATSAPP_VERIFY_TOKEN"),
        ) {
            Some(WhatsAppConfig {
                enabled: true,
                phone_number_id,
                access_token,
                verify_token,
            })
        } else {
            None
        };

        let telegram = if let (Ok(bot_token), Ok(webhook_secret)) = (
            std::env::var("TELEGRAM_BOT_TOKEN"),
            std::env::var("TELEGRAM_WEBHOOK_SECRET"),
        ) {
            Some(TelegramConfig {
                enabled: true,
                bot_token,
                webhook_secret,
                mode: std::env::var("TELEGRAM_MODE").unwrap_or_else(|_| default_telegram_mode()),
            })
        } else {
            None
        };

        let slack = if let (Ok(bot_token), Ok(signing_secret)) = (
            std::env::var("SLACK_BOT_TOKEN"),
            std::env::var("SLACK_SIGNING_SECRET"),
        ) {
            Some(SlackConfig {
                enabled: true,
                bot_token,
                signing_secret,
                app_token: std::env::var("SLACK_APP_TOKEN").ok(),
                mode: std::env::var("SLACK_MODE").unwrap_or_else(|_| default_slack_mode()),
            })
        } else {
            None
        };

        let sms = if let (Ok(account_sid), Ok(auth_token), Ok(from_number)) = (
            std::env::var("TWILIO_ACCOUNT_SID"),
            std::env::var("TWILIO_AUTH_TOKEN"),
            std::env::var("TWILIO_FROM_NUMBER"),
        ) {
            Some(SmsConfig {
                enabled: true,
                auth: SmsAuthConfig {
                    account_sid,
                    auth_token,
                    from_number,
                },
            })
        } else {
            None
        };

        let webchat = if std::env::var("WEBCHAT_ENABLED")
            .ok()
            .and_then(|s| s.parse::<bool>().ok())
            .unwrap_or(false)
        {
            Some(WebchatConfig {
                enabled: true,
                allowed_origins: std::env::var("WEBCHAT_ALLOWED_ORIGINS")
                    .ok()
                    .map(|s| s.split(',').map(|o| o.trim().to_string()).collect())
                    .unwrap_or_default(),
                welcome_message: std::env::var("WEBCHAT_WELCOME_MESSAGE").ok(),
            })
        } else {
            None
        };

        let email = if let (Ok(imap_host), Ok(smtp_host), Ok(email_user), Ok(email_pass)) = (
            std::env::var("EMAIL_IMAP_HOST"),
            std::env::var("EMAIL_SMTP_HOST"),
            std::env::var("EMAIL_USERNAME"),
            std::env::var("EMAIL_PASSWORD"),
        ) {
            Some(EmailConfig {
                enabled: true,
                imap: ImapConfig {
                    host: imap_host,
                    port: std::env::var("EMAIL_IMAP_PORT")
                        .ok()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or_else(default_imap_port),
                    username: email_user.clone(),
                    password: email_pass.clone(),
                    inbox: std::env::var("EMAIL_INBOX").unwrap_or_else(|_| default_inbox()),
                },
                smtp: SmtpConfig {
                    host: smtp_host,
                    port: std::env::var("EMAIL_SMTP_PORT")
                        .ok()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or_else(default_smtp_port),
                    username: email_user,
                    password: email_pass,
                    from_address: std::env::var("EMAIL_FROM_ADDRESS").unwrap_or_default(),
                },
            })
        } else {
            None
        };

        let teams = if let (Ok(app_id), Ok(app_password)) = (
            std::env::var("TEAMS_APP_ID"),
            std::env::var("TEAMS_APP_PASSWORD"),
        ) {
            Some(TeamsConfig {
                enabled: true,
                auth: TeamsAuthConfig {
                    app_id,
                    app_password,
                },
            })
        } else {
            None
        };

        Ok(Config {
            gateway: GatewayConfig {
                websocket_port: std::env::var("GATEWAY_WEBSOCKET_PORT")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or_else(default_websocket_port),
                http_port: std::env::var("GATEWAY_HTTP_PORT")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or_else(default_http_port),
                log_level: std::env::var("LOG_LEVEL").unwrap_or_else(|_| default_log_level()),
                api_token: std::env::var("GATEWAY_API_TOKEN").ok(),
                rate_limit: RateLimitConfig {
                    enabled: std::env::var("RATE_LIMIT_ENABLED")
                        .ok()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(false),
                    messages_per_window: std::env::var("RATE_LIMIT_MESSAGES")
                        .ok()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or_else(default_rate_limit_messages),
                    window_seconds: std::env::var("RATE_LIMIT_WINDOW_SECONDS")
                        .ok()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or_else(default_rate_limit_window_seconds),
                },
                max_body_bytes: std::env::var("GATEWAY_MAX_BODY_BYTES")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or_else(default_max_body_bytes),
                request_timeout_seconds: std::env::var("GATEWAY_REQUEST_TIMEOUT_SECONDS")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or_else(default_request_timeout_seconds),
                allowed_ws_origins: std::env::var("GATEWAY_ALLOWED_WS_ORIGINS")
                    .ok()
                    .map(|s| s.split(',').map(|o| o.trim().to_string()).filter(|o| !o.is_empty()).collect())
                    .unwrap_or_default(),
                shutdown_timeout_seconds: std::env::var("GATEWAY_SHUTDOWN_TIMEOUT_SECONDS")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or_else(default_shutdown_timeout_seconds),
            },
            channels: ChannelsConfig {
                discord,
                whatsapp,
                telegram,
                slack,
                sms,
                webhook: None, // webhook endpoints not configurable via env vars alone
                webchat,
                email,
                teams,
                test_channel: std::env::var("TEST_CHANNEL")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(false),
            },
            agents: AgentsConfig {
                llm_provider: std::env::var("LLM_PROVIDER")
                    .unwrap_or_else(|_| default_llm_provider()),
                llm_model: std::env::var("LLM_MODEL").unwrap_or_else(|_| default_llm_model()),
                api_key,
                max_history: default_max_history(),
                temperature: default_temperature(),
                planning_enabled: std::env::var("AGENTS_PLANNING_ENABLED")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(false),
                planning_max_steps: std::env::var("AGENTS_PLANNING_MAX_STEPS")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or_else(default_planning_max_steps),
                api_base: std::env::var("ANTHROPIC_API_BASE").ok(),
                aws_region: std::env::var("AWS_REGION").ok(),
                retry: AgentRetryConfig {
                    enabled: std::env::var("AGENTS_RETRY_ENABLED")
                        .ok()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(true),
                    max_attempts: std::env::var("AGENTS_RETRY_MAX_ATTEMPTS")
                        .ok()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or_else(default_retry_max_attempts),
                    base_delay_ms: std::env::var("AGENTS_RETRY_BASE_DELAY_MS")
                        .ok()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or_else(default_retry_base_delay_ms),
                },
            },
            tools: ToolsConfig {
                enabled: std::env::var("TOOLS_ENABLED")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(false),
                max_steps: std::env::var("TOOLS_MAX_STEPS")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or_else(default_max_steps),
                filesystem_root: std::env::var("TOOLS_FILESYSTEM_ROOT").ok(),
                web_search_api_key: std::env::var("TAVILY_API_KEY").ok(),
                shell: ShellToolConfig {
                    enabled: std::env::var("TOOLS_SHELL_ENABLED")
                        .ok()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(false),
                    require_approval: std::env::var("TOOLS_SHELL_REQUIRE_APPROVAL")
                        .ok()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(true),
                    allowed_commands: std::env::var("TOOLS_SHELL_ALLOWED_COMMANDS")
                        .ok()
                        .map(|s| s.split(',').map(|c| c.trim().to_string()).collect())
                        .unwrap_or_default(),
                },
                pdf_enabled: std::env::var("TOOLS_PDF_ENABLED")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(false),
                image_enabled: std::env::var("TOOLS_IMAGE_ENABLED")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(false),
            },
            otel: OtelConfig {
                endpoint: std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok(),
                service_name: std::env::var("OTEL_SERVICE_NAME")
                    .unwrap_or_else(|_| default_otel_service_name()),
            },
            dashboard: DashboardConfig {
                auth_password: std::env::var("DASHBOARD_AUTH_PASSWORD").ok(),
            },
            memory: MemoryConfig {
                backend: std::env::var("MEMORY_BACKEND")
                    .unwrap_or_else(|_| default_memory_backend()),
                redis_url: std::env::var("REDIS_URL").ok(),
                redis_ttl_seconds: std::env::var("REDIS_TTL_SECONDS")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or_else(default_redis_ttl),
                sqlite_path: std::env::var("SQLITE_PATH").ok(),
                postgres_url: std::env::var("DATABASE_URL").ok(),
                vector_store: std::env::var("VECTOR_STORE")
                    .unwrap_or_else(|_| default_vector_store()),
                vector_store_url: std::env::var("VECTOR_STORE_URL").ok(),
                embedding_provider: std::env::var("EMBEDDING_PROVIDER")
                    .unwrap_or_else(|_| default_embedding_provider()),
                embedding_model: std::env::var("EMBEDDING_MODEL")
                    .unwrap_or_else(|_| default_embedding_model()),
                summarization: SummarizationConfig {
                    enabled: std::env::var("SUMMARIZATION_ENABLED")
                        .ok()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(false),
                    trigger_at: std::env::var("SUMMARIZATION_TRIGGER_AT")
                        .ok()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or_else(default_summarization_trigger_at),
                    keep_recent: std::env::var("SUMMARIZATION_KEEP_RECENT")
                        .ok()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or_else(default_summarization_keep_recent),
                    model: std::env::var("SUMMARIZATION_MODEL")
                        .unwrap_or_else(|_| default_summarization_model()),
                },
            },
            mcp: McpConfig::default(),
            skills: SkillsConfig {
                enabled: std::env::var("SKILLS_ENABLED")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(false),
                paths: std::env::var("SKILLS_PATHS")
                    .ok()
                    .map(|s| s.split(':').map(|p| p.trim().to_string()).collect())
                    .unwrap_or_else(default_skills_paths),
                max_active: std::env::var("SKILLS_MAX_ACTIVE")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or_else(default_skills_max_active),
            },
            audit: AuditConfig {
                enabled: std::env::var("AUDIT_ENABLED")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(false),
                path: std::env::var("AUDIT_PATH").unwrap_or_default(),
            },
            cron: CronConfig::default(),
        })
    }
}
