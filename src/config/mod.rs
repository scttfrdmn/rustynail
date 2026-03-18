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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayConfig {
    #[serde(default = "default_websocket_port")]
    pub websocket_port: u16,

    #[serde(default = "default_http_port")]
    pub http_port: u16,

    #[serde(default = "default_log_level")]
    pub log_level: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelsConfig {
    pub discord: Option<DiscordConfig>,
    pub whatsapp: Option<WhatsAppConfig>,
    pub telegram: Option<TelegramConfig>,
    pub slack: Option<SlackConfig>,
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
pub struct MemoryConfig {
    /// Storage backend: `"inmemory"` (default) or `"redis"`.
    #[serde(default = "default_memory_backend")]
    pub backend: String,

    /// Redis connection URL. Required when `backend = "redis"`.
    pub redis_url: Option<String>,

    /// TTL in seconds for Redis history keys. 0 = no expiry.
    #[serde(default = "default_redis_ttl")]
    pub redis_ttl_seconds: u64,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            backend: default_memory_backend(),
            redis_url: None,
            redis_ttl_seconds: default_redis_ttl(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlackConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default)]
    pub bot_token: String,

    #[serde(default)]
    pub signing_secret: String,
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
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_steps: default_max_steps(),
            filesystem_root: None,
            web_search_api_key: None,
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

    /// Override the Anthropic API base URL (e.g. for test mock servers).
    /// When `None`, defaults to `https://api.anthropic.com`.
    #[serde(default)]
    pub api_base: Option<String>,
}

// Default values
fn default_websocket_port() -> u16 {
    18789
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
            },
            channels: ChannelsConfig {
                discord,
                whatsapp,
                telegram,
                slack,
            },
            agents: AgentsConfig {
                llm_provider: default_llm_provider(),
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
            },
        })
    }
}
