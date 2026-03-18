use crate::config::{AgentsConfig, ToolsConfig};
use crate::tools::ToolRegistry;

const SYSTEM_PROMPT: &str = "You are a helpful AI assistant named RustyNail. \
    Be conversational, friendly, and concise. \
    You're chatting with users on Discord and other platforms.";

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
        }
    }

    /// Register a tool with the agent manager. New agents will pick it up.
    pub async fn register_tool(&self, tool: Arc<dyn Tool>) -> Result<()> {
        let mut registry = self.tool_registry.write().await;
        registry.register(tool)
    }

    /// Create an LLM backend based on the configured `llm_provider`.
    async fn create_llm(&self) -> Result<Arc<dyn Agent>> {
        let api_key = self.config.api_key.clone();
        let model = self.config.llm_model.clone();
        let temperature = self.config.temperature;
        let api_base = self.config.api_base.clone();

        let llm: Arc<dyn Agent> = match self.config.llm_provider.as_str() {
            "openai" => {
                let config = OpenAIConfig {
                    api_key,
                    model,
                    temperature: temperature as f64,
                    api_base: api_base
                        .unwrap_or_else(|| "https://api.openai.com/v1".to_string()),
                    ..Default::default()
                };
                Arc::new(OpenAIAgent::new(config))
            }
            "ollama" => {
                let config = OllamaConfig {
                    model,
                    temperature: temperature as f64,
                    api_base: api_base
                        .unwrap_or_else(|| "http://localhost:11434".to_string()),
                    ..Default::default()
                };
                Arc::new(OllamaAgent::new(config))
            }
            "gemini" => {
                let config = GeminiConfig {
                    api_key,
                    model,
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
                    region: self
                        .config
                        .aws_region
                        .clone()
                        .unwrap_or_else(|| "us-east-1".to_string()),
                    model,
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
                    model,
                    api_key: Some(api_key),
                    base_url: api_base
                        .unwrap_or_else(|| "http://localhost:4000".to_string()),
                    temperature: Some(temperature),
                    ..Default::default()
                };
                Arc::new(LiteLLMAdapter::new(config))
            }
            "openai-compat" => {
                let config = OpenAICompatibleConfig {
                    model,
                    api_key: Some(api_key),
                    base_url: api_base
                        .unwrap_or_else(|| "http://localhost:8000/v1".to_string()),
                    ..Default::default()
                };
                Arc::new(OpenAICompatibleAgent::new(config))
            }
            _ => {
                // Default: Anthropic
                let config = AnthropicConfig {
                    api_key,
                    model,
                    max_tokens: 1024,
                    temperature: temperature as f64,
                    api_base: api_base
                        .unwrap_or_else(|| "https://api.anthropic.com".to_string()),
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

        ConversationalAgent::new(ConversationalConfig {
            llm: llm_for_agent,
            max_history: self.config.max_history,
            system_prompt: Some(SYSTEM_PROMPT.to_string()),
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

        // Process message with the stored agent
        let agents = self.agents.read().await;
        let agent = agents.get(user_id).expect("agent was just inserted");

        let input = agenkit::core::Message::with_text("user", message);
        let response = agent
            .process(input)
            .await
            .map_err(|e| anyhow::anyhow!("agent error: {}", e))?;

        let response_text = response
            .content_as_str()
            .unwrap_or("I'm sorry, I couldn't generate a response.")
            .to_string();

        Ok(response_text)
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
