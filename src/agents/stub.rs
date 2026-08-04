use agenkit::core::{Agent, AgentError, Message};
use async_trait::async_trait;

/// Model name the stub reports as having produced a response.
///
/// A fixed, explicitly versioned name rather than an alias: the stub exists so
/// the metering path can be exercised without credentials, and that path asserts
/// the reported model is deterministic and pinned. `-v1` is a version marker, not
/// a real release.
pub const STUB_MODEL: &str = "stub-echo-v1";

/// Stub LLM agent for zero-credential integration testing.
///
/// Echo mode (default, no `stub_response` set): returns `"echo: <user message>"`.
/// Fixed mode: always returns the configured `stub_response` string.
///
/// Selected when `agents.llm_provider = "stub"`.
///
/// Reports `model` and `usage` metadata in the same shape a real adapter does,
/// so the endpoint's token-count and cost handling is testable with no
/// credentials. The token counts are a deliberate char/4 approximation — that is
/// honest here because the stub *is* the provider, so its self-reported counts
/// are by definition authoritative for it. The same arithmetic in the endpoint
/// would be a fabrication, which is why it does not live there.
pub struct StubAgent {
    response: Option<String>,
}

impl StubAgent {
    pub fn new() -> Self {
        Self { response: None }
    }

    pub fn with_response(response: impl Into<String>) -> Self {
        Self {
            response: Some(response.into()),
        }
    }
}

impl Default for StubAgent {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Agent for StubAgent {
    fn name(&self) -> &str {
        "stub"
    }

    async fn process(&self, message: Message) -> Result<Message, AgentError> {
        let prompt = message.content_as_str().unwrap_or("(empty)").to_string();
        let text = match &self.response {
            Some(fixed) => fixed.clone(),
            None => format!("echo: {}", prompt),
        };

        let prompt_tokens = prompt.len().div_ceil(4) as u64;
        let completion_tokens = text.len().div_ceil(4) as u64;

        let mut msg = Message::with_text("assistant", text);
        msg.metadata
            .insert("model".to_string(), serde_json::json!(STUB_MODEL));
        msg.metadata.insert(
            "usage".to_string(),
            serde_json::json!({
                "prompt_tokens": prompt_tokens,
                "completion_tokens": completion_tokens,
                "total_tokens": prompt_tokens + completion_tokens,
            }),
        );
        msg.metadata
            .insert("finish_reason".to_string(), serde_json::json!("stop"));
        Ok(msg)
    }
}
