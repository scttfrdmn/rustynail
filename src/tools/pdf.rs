use agenkit::{AgentError, Tool, ToolResult};
use async_trait::async_trait;
use base64::Engine;
use std::collections::HashMap;

const DEFAULT_MAX_BYTES: usize = 32 * 1024 * 1024; // 32 MB
const REQUEST_TIMEOUT_SECS: u64 = 60;
const DEFAULT_PROMPT: &str = "Analyze this PDF and summarize its contents.";

pub struct PdfAnalysisTool {
    api_key: String,
    api_base: String,
    model: String,
}

impl PdfAnalysisTool {
    pub fn new(api_key: String, api_base: String, model: String) -> Self {
        Self {
            api_key,
            api_base,
            model,
        }
    }
}

#[async_trait]
impl Tool for PdfAnalysisTool {
    fn name(&self) -> &str {
        "pdf_analysis"
    }

    fn description(&self) -> &str {
        "Fetches or reads a PDF file and analyzes it using Claude's document understanding. \
        Parameters: source (required, path or URL), prompt (optional, default: analyze and summarize), \
        max_bytes (optional, default 33554432 = 32 MB)."
    }

    fn parameters_schema(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "source": {
                    "type": "string",
                    "description": "Path to a local PDF file or an http(s):// URL"
                },
                "prompt": {
                    "type": "string",
                    "description": "What to ask about the PDF (default: analyze and summarize)"
                },
                "max_bytes": {
                    "type": "integer",
                    "description": "Maximum PDF size in bytes (default 33554432 = 32 MB)"
                }
            },
            "required": ["source"]
        }))
    }

    async fn execute(
        &self,
        params: HashMap<String, serde_json::Value>,
    ) -> Result<ToolResult, AgentError> {
        let source = match params.get("source").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return Ok(ToolResult::error("source parameter is required")),
        };

        let prompt = params
            .get("prompt")
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_PROMPT)
            .to_string();

        let max_bytes = params
            .get("max_bytes")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_MAX_BYTES);

        // Fetch bytes from URL or local file
        let bytes = if source.starts_with("http://") || source.starts_with("https://") {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
                .build()
                .map_err(|e| AgentError::Internal(format!("failed to build HTTP client: {}", e)))?;

            let response = client
                .get(&source)
                .send()
                .await
                .map_err(|e| AgentError::Internal(format!("fetch error: {}", e)))?;

            if !response.status().is_success() {
                return Ok(ToolResult::error(format!(
                    "HTTP {} for {}",
                    response.status(),
                    source
                )));
            }

            response
                .bytes()
                .await
                .map_err(|e| AgentError::Internal(format!("read error: {}", e)))?
                .to_vec()
        } else {
            tokio::fs::read(&source)
                .await
                .map_err(|e| AgentError::Internal(format!("file read error: {}", e)))?
        };

        if bytes.len() > max_bytes {
            return Ok(ToolResult::error(format!(
                "PDF exceeds size limit ({} > {} bytes)",
                bytes.len(),
                max_bytes
            )));
        }

        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);

        // POST to Anthropic API with document content block
        let payload = serde_json::json!({
            "model": self.model,
            "max_tokens": 1024,
            "messages": [{
                "role": "user",
                "content": [
                    {
                        "type": "document",
                        "source": {
                            "type": "base64",
                            "media_type": "application/pdf",
                            "data": b64
                        }
                    },
                    {
                        "type": "text",
                        "text": prompt
                    }
                ]
            }]
        });

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .build()
            .map_err(|e| AgentError::Internal(format!("failed to build HTTP client: {}", e)))?;

        let url = format!("{}/v1/messages", self.api_base.trim_end_matches('/'));

        let response = client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("anthropic-beta", "pdfs-2024-09-25")
            .header("content-type", "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(|e| AgentError::Internal(format!("API request error: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Ok(ToolResult::error(format!("API error {}: {}", status, body)));
        }

        let resp_json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| AgentError::Internal(format!("parse error: {}", e)))?;

        let text = resp_json["content"][0]["text"]
            .as_str()
            .unwrap_or("(no response)")
            .to_string();

        Ok(ToolResult::success(serde_json::json!(text)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_name() {
        let tool = PdfAnalysisTool::new(
            "key".to_string(),
            "https://api.anthropic.com".to_string(),
            "claude-3-5-sonnet-20241022".to_string(),
        );
        assert_eq!(tool.name(), "pdf_analysis");
    }
}
