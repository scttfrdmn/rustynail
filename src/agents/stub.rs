use agenkit::core::{Agent, AgentError, Message};
use async_trait::async_trait;

/// Stub LLM agent for zero-credential integration testing.
///
/// Echo mode (default, no `stub_response` set): returns `"echo: <user message>"`.
/// Fixed mode: always returns the configured `stub_response` string.
///
/// Selected when `agents.llm_provider = "stub"`.
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
        let text = match &self.response {
            Some(fixed) => fixed.clone(),
            None => {
                let user_text = message.content_as_str().unwrap_or("(empty)");
                format!("echo: {}", user_text)
            }
        };
        Ok(Message::with_text("assistant", text))
    }
}
