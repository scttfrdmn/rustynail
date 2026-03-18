use crate::config::SummarizationConfig;
use crate::memory::MemoryStore;
use agenkit::adapters::anthropic::{AnthropicAgent, AnthropicConfig};
use agenkit::core::{Agent, Message as AgentMessage};
use std::sync::Arc;
use tracing::{error, info};

/// Summarises old conversation history when it grows beyond a threshold,
/// replacing the oldest messages with a compact `[Summary: ...]` entry.
pub struct MemorySummarizer {
    config: SummarizationConfig,
    api_key: String,
    api_base: String,
}

impl MemorySummarizer {
    pub fn new(config: SummarizationConfig, api_key: String, api_base: Option<String>) -> Self {
        Self {
            config,
            api_key,
            api_base: api_base.unwrap_or_else(|| "https://api.anthropic.com".to_string()),
        }
    }

    /// Called after adding a message. If history exceeds `trigger_at`, runs a
    /// summarisation (fire-and-forget via `tokio::spawn`).
    pub fn maybe_summarize(&self, store: Arc<dyn MemoryStore>, user_id: &str) {
        if !self.config.enabled {
            return;
        }

        let history = store.get_history(user_id);
        if history.len() <= self.config.trigger_at {
            return;
        }

        let config = self.config.clone();
        let api_key = self.api_key.clone();
        let api_base = self.api_base.clone();
        let uid = user_id.to_string();

        tokio::spawn(async move {
            let to_summarize_count = history.len().saturating_sub(config.keep_recent);
            let to_summarize = &history[..to_summarize_count];
            let keep_recent = history[to_summarize_count..].to_vec();

            let text = to_summarize.join("\n");

            let agent_config = AnthropicConfig {
                api_key: api_key.clone(),
                model: config.model.clone(),
                max_tokens: 512,
                temperature: 0.3,
                api_base: api_base.clone(),
                ..Default::default()
            };
            let agent = AnthropicAgent::new(agent_config);

            let prompt = format!(
                "Summarise the following conversation history concisely (2-4 sentences):\n\n{}",
                text
            );
            let input = AgentMessage::with_text("user", &prompt);

            match agent.process(input).await {
                Ok(resp) => {
                    let summary = resp
                        .content_as_str()
                        .unwrap_or("(summary unavailable)")
                        .to_string();

                    // Rebuild history: summary entry + recent messages
                    store.clear_history(&uid);
                    store.add_message(&uid, format!("[Summary: {}]", summary));
                    for msg in keep_recent {
                        store.add_message(&uid, msg);
                    }
                    info!("Summarised history for user {} ({} messages condensed)", uid, to_summarize_count);
                }
                Err(e) => {
                    error!("Summarisation error for user {}: {}", uid, e);
                }
            }
        });
    }
}
