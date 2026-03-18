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

#[cfg(test)]
mod tests {
    use super::*;
    use agenkit::core::{AgentError, Message};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Always succeeds with a fixed response.
    struct OkAgent {
        reply: String,
    }

    #[async_trait]
    impl Agent for OkAgent {
        fn name(&self) -> &str {
            "ok"
        }

        async fn process(&self, _input: Message) -> Result<Message, AgentError> {
            Ok(Message::with_text("assistant", self.reply.clone()))
        }
    }

    /// Always fails with the configured error string.
    struct FailAgent {
        error: String,
        calls: Arc<AtomicU32>,
    }

    impl FailAgent {
        fn new(error: impl Into<String>) -> (Self, Arc<AtomicU32>) {
            let calls = Arc::new(AtomicU32::new(0));
            (
                Self {
                    error: error.into(),
                    calls: calls.clone(),
                },
                calls,
            )
        }
    }

    #[async_trait]
    impl Agent for FailAgent {
        fn name(&self) -> &str {
            "fail"
        }

        async fn process(&self, _input: Message) -> Result<Message, AgentError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(AgentError::ProcessingError(self.error.clone()))
        }
    }

    #[tokio::test]
    async fn test_primary_success_no_fallback() {
        let primary = Arc::new(OkAgent {
            reply: "hello".to_string(),
        });
        let agent = FallbackAgent::new(primary, vec![]);
        let input = Message::with_text("user", "hi");
        let result = agent.process(input).await.unwrap();
        assert_eq!(result.content_as_str().unwrap(), "hello");
    }

    #[tokio::test]
    async fn test_capacity_error_uses_fallback() {
        let (primary, _) = FailAgent::new("upstream 503 service unavailable");
        let fallback = Arc::new(OkAgent {
            reply: "from fallback".to_string(),
        });
        let agent = FallbackAgent::new(Arc::new(primary), vec![fallback]);
        let input = Message::with_text("user", "hi");
        let result = agent.process(input).await.unwrap();
        assert_eq!(result.content_as_str().unwrap(), "from fallback");
    }

    #[tokio::test]
    async fn test_non_capacity_error_not_forwarded() {
        let (primary, primary_calls) = FailAgent::new("400 bad request");
        let (fb, fb_calls) = FailAgent::new("should not be called");
        let agent = FallbackAgent::new(Arc::new(primary), vec![Arc::new(fb)]);
        let input = Message::with_text("user", "hi");
        let result = agent.process(input).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("400"));
        assert_eq!(primary_calls.load(Ordering::SeqCst), 1);
        assert_eq!(fb_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn test_all_fallbacks_fail_returns_last_error() {
        let (primary, _) = FailAgent::new("503 primary down");
        let (fb1, _) = FailAgent::new("503 fb1 down");
        let (fb2, _) = FailAgent::new("503 fb2 down");
        let agent = FallbackAgent::new(Arc::new(primary), vec![Arc::new(fb1), Arc::new(fb2)]);
        let input = Message::with_text("user", "hi");
        let result = agent.process(input).await;
        assert!(result.is_err());
        // Last error should be from fb2
        assert!(result.unwrap_err().to_string().contains("fb2 down"));
    }
}
