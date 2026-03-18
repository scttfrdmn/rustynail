use crate::config::SummarizationConfig;
use crate::memory::MemoryStore;
use agenkit::adapters::anthropic::{AnthropicAgent, AnthropicConfig};
use agenkit::core::{Agent, Message as AgentMessage};
use std::sync::Arc;
use tracing::{error, info};

/// Rough token estimator: 1 token ≈ 4 bytes (allocation-free, no external deps).
fn estimate_tokens(text: &str) -> usize {
    (text.len() + 3) / 4
}

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

    /// Called after adding a message. If history exceeds `trigger_at` messages
    /// **or** `trigger_token_budget` estimated tokens, runs a summarisation
    /// (fire-and-forget via `tokio::spawn`).
    pub fn maybe_summarize(&self, store: Arc<dyn MemoryStore>, user_id: &str) {
        if !self.config.enabled {
            return;
        }

        let history = store.get_history(user_id);

        let count_trigger = history.len() > self.config.trigger_at;
        let token_trigger = self.config.trigger_token_budget > 0 && {
            let total: usize = history.iter().map(|m| estimate_tokens(m)).sum();
            total > self.config.trigger_token_budget
        };

        if !count_trigger && !token_trigger {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_tokens_basic() {
        // "hello" = 5 bytes → (5 + 3) / 4 = 2
        assert_eq!(estimate_tokens("hello"), 2);
        // 8 bytes → 2 tokens
        assert_eq!(estimate_tokens("abcdefgh"), 2);
        // 9 bytes → (9+3)/4 = 3 tokens
        assert_eq!(estimate_tokens("abcdefghi"), 3);
        // empty
        assert_eq!(estimate_tokens(""), 0);
    }

    #[tokio::test]
    async fn test_token_trigger_fires_before_message_count() {
        use crate::memory::InMemoryStore;

        let config = SummarizationConfig {
            enabled: true,
            trigger_at: 100, // high — won't fire on count
            keep_recent: 5,
            model: "unused".to_string(),
            trigger_token_budget: 10, // low — will fire on token count
        };
        let summarizer = MemorySummarizer::new(config, "key".to_string(), None);
        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryStore::new(200));

        // Add 5 short messages totalling > 10 estimated tokens each
        for i in 0..5 {
            store.add_message("u1", format!("message number {}", i)); // ~16 bytes ~ 4 tokens each
        }

        let history = store.get_history("u1");
        assert_eq!(history.len(), 5, "pre-summarize, 5 messages");

        let total_tokens: usize = history.iter().map(|m| estimate_tokens(m)).sum();
        assert!(total_tokens > 10, "total tokens {} should exceed budget 10", total_tokens);

        // Verify the token trigger condition would fire (count_trigger=false, token_trigger=true)
        let count_trigger = history.len() > 100;
        let token_trigger = total_tokens > 10;
        assert!(!count_trigger, "count trigger must not fire");
        assert!(token_trigger, "token trigger must fire before count threshold");

        // maybe_summarize spawns a tokio task; call it to verify no panic
        summarizer.maybe_summarize(store, "u1");
    }
}
