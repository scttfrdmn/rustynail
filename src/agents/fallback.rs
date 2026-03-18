use agenkit::core::{Agent, AgentError, Message};
use std::sync::Arc;

/// Error patterns that indicate a provider capacity/availability problem.
/// A match triggers fallback to the next provider.
/// NOTE: "429" (rate-limit) is deliberately excluded — retry handles that.
const FALLBACK_TRIGGERS: &[&str] = &["500", "503", "overloaded", "model not found"];

/// Wraps a primary `Agent` with an ordered list of fallback providers.
///
/// On `process()`:
/// 1. Try the primary agent.
/// 2. On error, check if the error text signals a capacity/overload condition.
/// 3. If yes, try each fallback in order; return the first success.
/// 4. If no fallback succeeds, propagate the last error.
/// 5. Non-capacity errors (including 429 rate-limit) are not forwarded to
///    fallbacks — they are returned immediately.
pub struct FallbackAgent {
    primary: Arc<dyn Agent>,
    fallbacks: Vec<Arc<dyn Agent>>,
}

impl FallbackAgent {
    pub fn new(primary: Arc<dyn Agent>, fallbacks: Vec<Arc<dyn Agent>>) -> Self {
        Self { primary, fallbacks }
    }
}

#[async_trait::async_trait]
impl Agent for FallbackAgent {
    fn name(&self) -> &str {
        "fallback"
    }

    async fn process(&self, input: Message) -> Result<Message, AgentError> {
        match self.primary.process(input.clone()).await {
            Ok(resp) => return Ok(resp),
            Err(e) => {
                let err_str = e.to_string().to_lowercase();
                let is_capacity_error = FALLBACK_TRIGGERS
                    .iter()
                    .any(|trigger| err_str.contains(trigger));

                if !is_capacity_error {
                    return Err(e);
                }

                tracing::warn!(
                    "Primary LLM returned capacity/overload error, trying {} fallback(s): {}",
                    self.fallbacks.len(),
                    e
                );

                let mut last_err = e;
                for (i, fallback) in self.fallbacks.iter().enumerate() {
                    match fallback.process(input.clone()).await {
                        Ok(resp) => {
                            tracing::info!("Fallback provider {} succeeded", i);
                            return Ok(resp);
                        }
                        Err(fb_err) => {
                            tracing::warn!("Fallback provider {} failed: {}", i, fb_err);
                            last_err = fb_err;
                        }
                    }
                }

                Err(last_err)
            }
        }
    }
}
