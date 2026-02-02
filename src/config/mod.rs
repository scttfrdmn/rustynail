use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub gateway: GatewayConfig,
    pub channels: ChannelsConfig,
    pub agents: AgentsConfig,
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

impl Default for AgentsConfig {
    fn default() -> Self {
        Self {
            llm_provider: default_llm_provider(),
            llm_model: default_llm_model(),
            api_key: String::new(),
            max_history: default_max_history(),
            temperature: default_temperature(),
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
        let discord_token = std::env::var("DISCORD_BOT_TOKEN")
            .map_err(|_| anyhow::anyhow!("DISCORD_BOT_TOKEN not set"))?;

        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY not set"))?;

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
                discord: Some(DiscordConfig {
                    enabled: true,
                    auth: DiscordAuthConfig {
                        token: discord_token,
                    },
                }),
            },
            agents: AgentsConfig {
                llm_provider: default_llm_provider(),
                llm_model: std::env::var("LLM_MODEL").unwrap_or_else(|_| default_llm_model()),
                api_key,
                max_history: default_max_history(),
                temperature: default_temperature(),
            },
        })
    }
}
