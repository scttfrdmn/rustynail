use crate::agents::fallback::FallbackAgent;
use crate::config::{AgentRetryConfig, AgentsConfig, ToolsConfig};
use crate::gateway::dashboard::MessageStats;
use crate::tools::ToolRegistry;

const SYSTEM_PROMPT: &str = "You are a helpful AI assistant named RustyNail. \
    Be conversational, friendly, and concise. \
    You're chatting with users on Discord and other platforms.";

use crate::agents::stub::StubAgent;
use agenkit::adapters::anthropic::{AnthropicAgent, AnthropicConfig};
use agenkit::adapters::bedrock::{BedrockAdapter, BedrockConfig};
use agenkit::adapters::gemini::{GeminiAdapter, GeminiConfig};
use agenkit::adapters::litellm::{LiteLLMAdapter, LiteLLMConfig};
use agenkit::adapters::ollama::{OllamaAgent, OllamaConfig};
use agenkit::adapters::openai::{OpenAIAgent, OpenAIConfig};
use agenkit::adapters::openai_compatible::{OpenAICompatibleAgent, OpenAICompatibleConfig};
use agenkit::core::Agent;
use agenkit::patterns::planning::{PlanningAgent, PlanningConfig};
use agenkit::patterns::react::{ReActAgent, ReActConfig};
use agenkit::patterns::{ConversationalAgent, ConversationalConfig};
use agenkit::Tool;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

/// Manages per-user conversational agents
pub struct AgentManager {
    config: AgentsConfig,
    tools_config: ToolsConfig,
    agents: Arc<RwLock<HashMap<String, ConversationalAgent>>>,
    tool_registry: Arc<RwLock<ToolRegistry>>,
    planning_agent: Option<Arc<PlanningAgent>>,
    /// Skills context appended to every new agent's system prompt (when skills are enabled).
    /// Wrapped in Arc<RwLock<>> to support hot-reload via `reload_skills_context`.
    skills_context: Arc<RwLock<Option<String>>>,
    /// Prometheus / dashboard stats (optional; absent in tests).
    stats: Option<Arc<MessageStats>>,
    /// LLM retry configuration.
    retry_config: AgentRetryConfig,
}

impl AgentManager {
    pub fn new(config: AgentsConfig) -> Self {
        Self::with_tools(config, ToolsConfig::default(), ToolRegistry::new())
    }

    pub fn with_tools(
        config: AgentsConfig,
        tools_config: ToolsConfig,
        registry: ToolRegistry,
    ) -> Self {
        Self::with_tools_and_skills(config, tools_config, registry, None)
    }

    pub fn with_tools_and_skills(
        config: AgentsConfig,
        tools_config: ToolsConfig,
        registry: ToolRegistry,
        skills_context: Option<String>,
    ) -> Self {
        Self::with_tools_skills_and_stats(config, tools_config, registry, skills_context, None)
    }

    pub fn with_tools_skills_and_stats(
        config: AgentsConfig,
        tools_config: ToolsConfig,
        registry: ToolRegistry,
        skills_context: Option<String>,
        stats: Option<Arc<MessageStats>>,
    ) -> Self {
        let retry_config = config.retry.clone();

        let planning_agent = if config.planning_enabled {
            let anthropic_config = AnthropicConfig {
                api_key: config.api_key.clone(),
                model: config.llm_model.clone(),
                max_tokens: 1024,
                temperature: config.temperature as f64,
                api_base: config
                    .api_base
                    .clone()
                    .unwrap_or_else(|| "https://api.anthropic.com".to_string()),
                ..Default::default()
            };
            let llm = Arc::new(AnthropicAgent::new(anthropic_config));
            match PlanningAgent::new(PlanningConfig {
                llm,
                executor: None,
                max_steps: config.planning_max_steps,
                allow_replanning: false,
                system_prompt: None,
            }) {
                Ok(agent) => {
                    info!(
                        "Planning agent created (max_steps={})",
                        config.planning_max_steps
                    );
                    Some(Arc::new(agent))
                }
                Err(e) => {
                    tracing::error!("Failed to create planning agent: {}", e);
                    None
                }
            }
        } else {
            None
        };

        Self {
            config,
            tools_config,
            agents: Arc::new(RwLock::new(HashMap::new())),
            tool_registry: Arc::new(RwLock::new(registry)),
            planning_agent,
            skills_context: Arc::new(RwLock::new(skills_context)),
            stats,
            retry_config,
        }
    }

    /// Register a tool with the agent manager. New agents will pick it up.
    pub async fn register_tool(&self, tool: Arc<dyn Tool>) -> Result<()> {
        let mut registry = self.tool_registry.write().await;
        registry.register(tool)
    }

    /// Replace the skills context used for all future agent creations.
    /// Existing per-user agents are not affected until their next recreation.
    pub async fn reload_skills_context(&self, ctx: Option<String>) {
        let mut lock = self.skills_context.write().await;
        *lock = ctx;
    }

    /// Create an LLM backend based on the configured `llm_provider`.
    ///
    /// When fallback providers are configured the primary is wrapped in a
    /// `FallbackAgent` that tries each fallback in order on capacity errors.
    async fn create_llm(&self) -> Result<Arc<dyn Agent>> {
        let primary = self.create_llm_from_config(
            &self.config.api_key,
            &self.config.llm_model,
            self.config.temperature,
            self.config.api_base.as_deref(),
            self.config.aws_region.as_deref(),
            &self.config.llm_provider,
        )
        .await?;

        if self.config.fallback_providers.is_empty() {
            return Ok(primary);
        }

        let mut fallbacks: Vec<Arc<dyn Agent>> = Vec::new();
        for fb_cfg in &self.config.fallback_providers {
            match self
                .create_llm_from_config(
                    &fb_cfg.api_key,
                    &fb_cfg.model,
                    self.config.temperature,
                    fb_cfg.api_base.as_deref(),
                    None,
                    &fb_cfg.provider,
                )
                .await
            {
                Ok(agent) => fallbacks.push(agent),
                Err(e) => {
                    tracing::warn!("Failed to create fallback LLM '{}': {}", fb_cfg.provider, e)
                }
            }
        }

        if fallbacks.is_empty() {
            return Ok(primary);
        }

        info!(
            "FallbackAgent configured with {} fallback provider(s)",
            fallbacks.len()
        );
        Ok(Arc::new(FallbackAgent::new(primary, fallbacks)))
    }

    /// Build a single LLM adapter from explicit parameters.
    async fn create_llm_from_config(
        &self,
        api_key: &str,
        model: &str,
        temperature: f32,
        api_base: Option<&str>,
        aws_region: Option<&str>,
        provider: &str,
    ) -> Result<Arc<dyn Agent>> {
        let llm: Arc<dyn Agent> = match provider {
            "stub" => Arc::new(StubAgent::new()),
            "openai" => {
                let config = OpenAIConfig {
                    api_key: api_key.to_string(),
                    model: model.to_string(),
                    temperature: temperature as f64,
                    api_base: api_base
                        .unwrap_or("https://api.openai.com/v1")
                        .to_string(),
                    ..Default::default()
                };
                Arc::new(OpenAIAgent::new(config))
            }
            "ollama" => {
                let config = OllamaConfig {
                    model: model.to_string(),
                    temperature: temperature as f64,
                    api_base: api_base
                        .unwrap_or("http://localhost:11434")
                        .to_string(),
                    ..Default::default()
                };
                Arc::new(OllamaAgent::new(config))
            }
            "gemini" => {
                let config = GeminiConfig {
                    api_key: api_key.to_string(),
                    model: model.to_string(),
                    temperature: Some(temperature),
                    ..Default::default()
                };
                Arc::new(
                    GeminiAdapter::new(config)
                        .map_err(|e| anyhow::anyhow!("failed to create GeminiAdapter: {}", e))?,
                )
            }
            "bedrock" => {
                let config = BedrockConfig {
                    region: aws_region.unwrap_or("us-east-1").to_string(),
                    model: model.to_string(),
                    temperature: Some(temperature),
                    ..Default::default()
                };
                Arc::new(
                    BedrockAdapter::new(config)
                        .await
                        .map_err(|e| anyhow::anyhow!("failed to create BedrockAdapter: {}", e))?,
                )
            }
            "litellm" => {
                let config = LiteLLMConfig {
                    model: model.to_string(),
                    api_key: Some(api_key.to_string()),
                    base_url: api_base
                        .unwrap_or("http://localhost:4000")
                        .to_string(),
                    temperature: Some(temperature),
                    ..Default::default()
                };
                Arc::new(LiteLLMAdapter::new(config))
            }
            "openai-compat" => {
                let config = OpenAICompatibleConfig {
                    model: model.to_string(),
                    api_key: Some(api_key.to_string()),
                    base_url: api_base
                        .unwrap_or("http://localhost:8000/v1")
                        .to_string(),
                    ..Default::default()
                };
                Arc::new(OpenAICompatibleAgent::new(config))
            }
            _ => {
                // Default: Anthropic
                let config = AnthropicConfig {
                    api_key: api_key.to_string(),
                    model: model.to_string(),
                    max_tokens: 1024,
                    temperature: temperature as f64,
                    api_base: api_base
                        .unwrap_or("https://api.anthropic.com")
                        .to_string(),
                    ..Default::default()
                };
                Arc::new(AnthropicAgent::new(config))
            }
        };

        Ok(llm)
    }

    /// Create a new conversational agent, wrapping with ReActAgent when tools are enabled.
    async fn create_agent(&self) -> Result<ConversationalAgent> {
        let llm = self.create_llm().await?;

        let registry = self.tool_registry.read().await;
        let llm_for_agent: Arc<dyn Agent> = if self.tools_config.enabled && !registry.is_empty() {
            info!(
                "Creating ReActAgent with {} tool(s): {:?}",
                registry.all().len(),
                registry.names()
            );
            let react = ReActAgent::new(ReActConfig {
                agent: llm,
                tools: registry.all(),
                max_steps: self.tools_config.max_steps,
                verbose: false,
                prompt_template: None,
            })
            .map_err(|e| anyhow::anyhow!("failed to create ReActAgent: {}", e))?;
            Arc::new(react)
        } else {
            llm
        };
        drop(registry);

        // Build system prompt, optionally appending skill context
        let system_prompt = {
            let ctx = self.skills_context.read().await;
            match ctx.as_deref() {
                Some(c) if !c.is_empty() => format!("{}{}", SYSTEM_PROMPT, c),
                _ => SYSTEM_PROMPT.to_string(),
            }
        };

        ConversationalAgent::new(ConversationalConfig {
            llm: llm_for_agent,
            max_history: self.config.max_history,
            system_prompt: Some(system_prompt),
            include_system: true,
        })
        .map_err(|e| anyhow::anyhow!("failed to create ConversationalAgent: {}", e))
    }

    /// Process a planning task, bypassing per-user conversation history.
    async fn process_planning_message(&self, task: &str) -> Result<String> {
        let agent = match &self.planning_agent {
            Some(a) => a.clone(),
            None => return Err(anyhow::anyhow!("planning agent not configured")),
        };

        let input = agenkit::core::Message::with_text("user", task);
        let response = agent
            .process(input)
            .await
            .map_err(|e| anyhow::anyhow!("planning agent error: {}", e))?;

        Ok(response
            .content_as_str()
            .unwrap_or("I'm sorry, I couldn't generate a plan.")
            .to_string())
    }

    /// Process a message for a specific user, maintaining per-user conversation history.
    ///
    /// When retry is enabled, failed LLM calls are retried with exponential backoff.
    /// After all attempts are exhausted the error is propagated — the caller
    /// (`handle_message_inner`) is responsible for the friendly fallback and metrics.
    pub async fn process_message(&self, user_id: &str, message: &str) -> Result<String> {
        // Route /plan commands to the planning agent
        if self.planning_agent.is_some() {
            if let Some(task) = message.strip_prefix("/plan ") {
                return self.process_planning_message(task).await;
            }
        }

        // Check if agent exists under a read lock first
        {
            let agents = self.agents.read().await;
            if !agents.contains_key(user_id) {
                drop(agents);
                // Create and insert new agent
                let new_agent = self.create_agent().await?;
                let mut agents = self.agents.write().await;
                // Double-check in case another task beat us to it
                agents.entry(user_id.to_string()).or_insert(new_agent);
            }
        }

        // Process message with the stored agent, with optional retry
        let agents = self.agents.read().await;
        let agent = agents.get(user_id).expect("agent was just inserted");
        let input = agenkit::core::Message::with_text("user", message);

        let max_attempts = if self.retry_config.enabled {
            self.retry_config.max_attempts.max(1)
        } else {
            1
        };

        let mut last_error: Option<anyhow::Error> = None;

        for attempt in 0..max_attempts {
            if attempt > 0 {
                // Record retry metric and apply exponential backoff (+ optional jitter)
                if let Some(ref stats) = self.stats {
                    stats.record_llm_retry();
                }
                let base = self.retry_config.base_delay_ms
                    * 2u64.saturating_pow(attempt - 1);
                let delay_ms = if self.retry_config.jitter_enabled {
                    // ±20%: factor in [0.8, 1.2)
                    let nanos = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.subsec_nanos())
                        .unwrap_or(0);
                    let jitter = 0.8 + (nanos % 400) as f64 / 1000.0;
                    (base as f64 * jitter) as u64
                } else {
                    base
                };
                tracing::warn!(
                    "LLM attempt {}/{} for user '{}' failed, retrying in {}ms",
                    attempt,
                    max_attempts,
                    user_id,
                    delay_ms
                );
                tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
            }

            match agent.process(input.clone()).await {
                Ok(response) => {
                    let text = response
                        .content_as_str()
                        .unwrap_or("I'm sorry, I couldn't generate a response.")
                        .to_string();
                    return Ok(text);
                }
                Err(e) => {
                    last_error = Some(anyhow::anyhow!("agent error: {}", e));
                }
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("all LLM attempts failed")))
    }

    /// Clear conversation history for a user.
    pub async fn clear_history(&self, user_id: &str, keep_system: bool) {
        let agents = self.agents.read().await;
        if let Some(agent) = agents.get(user_id) {
            agent.clear_history(keep_system);
        }
    }

    /// Remove a user's agent (to free memory).
    pub async fn remove_user(&self, user_id: &str) {
        let mut agents = self.agents.write().await;
        agents.remove(user_id);
    }

    /// Get the number of active users.
    pub async fn active_users(&self) -> usize {
        self.agents.read().await.len()
    }
}
