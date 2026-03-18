use crate::config::{AgentsConfig, ToolsConfig};
use crate::tools::ToolRegistry;
use agenkit::adapters::anthropic::{AnthropicAgent, AnthropicConfig};
use agenkit::core::Agent;
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
        Self {
            config,
            tools_config,
            agents: Arc::new(RwLock::new(HashMap::new())),
            tool_registry: Arc::new(RwLock::new(registry)),
        }
    }

    /// Register a tool with the agent manager. New agents will pick it up.
    pub async fn register_tool(&self, tool: Arc<dyn Tool>) -> Result<()> {
        let mut registry = self.tool_registry.write().await;
        registry.register(tool)
    }

    /// Create a new conversational agent, wrapping with ReActAgent when tools are enabled.
    async fn create_agent(&self) -> Result<ConversationalAgent> {
        let anthropic_config = AnthropicConfig {
            api_key: self.config.api_key.clone(),
            model: self.config.llm_model.clone(),
            max_tokens: 1024,
            temperature: self.config.temperature as f64,
            ..Default::default()
        };

        let llm = Arc::new(AnthropicAgent::new(anthropic_config));

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
            system_prompt: Some(
                "You are a helpful AI assistant named RustyNail. \
                 Be conversational, friendly, and concise. \
                 You're chatting with users on Discord and other platforms."
                    .to_string(),
            ),
            include_system: true,
        })
        .map_err(|e| anyhow::anyhow!("failed to create ConversationalAgent: {}", e))
    }

    /// Process a message for a specific user, maintaining per-user conversation history.
    pub async fn process_message(&self, user_id: &str, message: &str) -> Result<String> {
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
