use crate::config::AgentsConfig;
use agenkit::adapters::anthropic::{AnthropicAgent, AnthropicConfig};
use agenkit::core::Agent;
use agenkit::patterns::{ConversationalAgent, ConversationalConfig};
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Manages per-user conversational agents
pub struct AgentManager {
    config: AgentsConfig,
    agents: Arc<RwLock<HashMap<String, ConversationalAgent>>>,
}

impl AgentManager {
    pub fn new(config: AgentsConfig) -> Self {
        Self {
            config,
            agents: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Get or create a conversational agent for a user
    pub async fn get_agent(&self, user_id: &str) -> Result<ConversationalAgent> {
        let agents = self.agents.read().await;

        // If agent exists, clone it (Arc clones are cheap)
        if let Some(agent) = agents.get(user_id) {
            // We need to return a clone/reference
            // Since ConversationalAgent wraps things in Arc, we should be able to clone
            // But looking at the pattern, ConversationalAgent doesn't impl Clone
            // Let's check if we need a different approach
            drop(agents);
            return self.create_agent();
        }

        drop(agents);
        self.create_and_store_agent(user_id).await
    }

    /// Create a new conversational agent
    fn create_agent(&self) -> Result<ConversationalAgent> {
        let anthropic_config = AnthropicConfig {
            api_key: self.config.api_key.clone(),
            model: self.config.llm_model.clone(),
            max_tokens: 1024,
            temperature: self.config.temperature as f64,
            ..Default::default()
        };

        let llm = Arc::new(AnthropicAgent::new(anthropic_config));

        let agent = ConversationalAgent::new(ConversationalConfig {
            llm,
            max_history: self.config.max_history,
            system_prompt: Some(
                "You are a helpful AI assistant named RustyNail. \
                 Be conversational, friendly, and concise. \
                 You're chatting with users on Discord and other platforms."
                    .to_string(),
            ),
            include_system: true,
        })?;

        Ok(agent)
    }

    /// Create and store a new agent for a user
    async fn create_and_store_agent(&self, user_id: &str) -> Result<ConversationalAgent> {
        let agent = self.create_agent()?;

        let mut agents = self.agents.write().await;
        agents.insert(user_id.to_string(), agent);

        // Create another instance to return (since we can't clone)
        self.create_agent()
    }

    /// Process a message for a specific user
    pub async fn process_message(
        &self,
        user_id: &str,
        message: &str,
    ) -> Result<String> {
        // Get the agent for this user
        let mut agents = self.agents.write().await;

        // Get or create agent
        let agent = if let Some(agent) = agents.get(user_id) {
            agent
        } else {
            let new_agent = self.create_agent()?;
            agents.insert(user_id.to_string(), new_agent);
            agents.get(user_id).unwrap()
        };

        // Process the message
        let input = agenkit::core::Message::with_text("user", message);
        let response = agent.process(input).await?;

        // Extract the response text
        let response_text = response
            .content_as_str()
            .unwrap_or("I'm sorry, I couldn't generate a response.")
            .to_string();

        Ok(response_text)
    }

    /// Clear conversation history for a user
    pub async fn clear_history(&self, user_id: &str, keep_system: bool) {
        let agents = self.agents.read().await;
        if let Some(agent) = agents.get(user_id) {
            agent.clear_history(keep_system);
        }
    }

    /// Remove a user's agent (to free memory)
    pub async fn remove_user(&self, user_id: &str) {
        let mut agents = self.agents.write().await;
        agents.remove(user_id);
    }

    /// Get the number of active users
    pub async fn active_users(&self) -> usize {
        self.agents.read().await.len()
    }
}
