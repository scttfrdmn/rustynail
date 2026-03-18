use agenkit::{AgentError, Tool, ToolResult};
use async_trait::async_trait;
use base64::Engine;
use std::collections::HashMap;

const DEFAULT_MAX_BYTES: usize = 5 * 1024 * 1024; // 5 MB
const REQUEST_TIMEOUT_SECS: u64 = 30;
const DEFAULT_PROMPT: &str = "Describe this image in detail.";

pub struct ImageAnalysisTool {
    api_key: String,
    api_base: String,
    model: String,
}

impl ImageAnalysisTool {
    pub fn new(api_key: String, api_base: String, model: String) -> Self {
        Self {
            api_key,
            api_base,
            model,
        }
    }
}

/// Detect media type from file extension or Content-Type header.
fn detect_media_type(source: &str, content_type_header: Option<&str>) -> Option<&'static str> {
    // Check Content-Type header first
    if let Some(ct) = content_type_header {
        if ct.contains("image/jpeg") || ct.contains("image/jpg") {
            return Some("image/jpeg");
        }
        if ct.contains("image/png") {
            return Some("image/png");
        }
        if ct.contains("image/gif") {
            return Some("image/gif");
        }
        if ct.contains("image/webp") {
            return Some("image/webp");
        }
    }

    // Fall back to file extension
    let lower = source.to_lowercase();
    let path = lower.split('?').next().unwrap_or(&lower);
    if path.ends_with(".jpg") || path.ends_with(".jpeg") {
        Some("image/jpeg")
    } else if path.ends_with(".png") {
        Some("image/png")
    } else if path.ends_with(".gif") {
        Some("image/gif")
    } else if path.ends_with(".webp") {
        Some("image/webp")
    } else {
        None
    }
}

#[async_trait]
impl Tool for ImageAnalysisTool {
    fn name(&self) -> &str {
        "image_analysis"
    }

    fn description(&self) -> &str {
        "Fetches or reads an image (jpeg/png/gif/webp) and analyzes it using Claude's vision. \
        Parameters: source (required, path or URL), prompt (optional, default: describe the image), \
        max_bytes (optional, default 5242880 = 5 MB)."
    }

    fn parameters_schema(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "source": {
                    "type": "string",
                    "description": "Path to a local image file or an http(s):// URL (jpeg/png/gif/webp)"
                },
                "prompt": {
                    "type": "string",
                    "description": "What to ask about the image (default: describe the image in detail)"
                },
                "max_bytes": {
                    "type": "integer",
                    "description": "Maximum image size in bytes (default 5242880 = 5 MB)"
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

        // Fetch bytes + detect media type
        let (bytes, media_type) =
            if source.starts_with("http://") || source.starts_with("https://") {
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
                    .build()
                    .map_err(|e| {
                        AgentError::Internal(format!("failed to build HTTP client: {}", e))
                    })?;

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

                let ct = response
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string());

                let mt = detect_media_type(&source, ct.as_deref());

                let data = response
                    .bytes()
                    .await
                    .map_err(|e| AgentError::Internal(format!("read error: {}", e)))?
                    .to_vec();

                (data, mt)
            } else {
                let data = tokio::fs::read(&source)
                    .await
                    .map_err(|e| AgentError::Internal(format!("file read error: {}", e)))?;
                let mt = detect_media_type(&source, None);
                (data, mt)
            };

        let media_type = match media_type {
            Some(mt) => mt,
            None => {
                return Ok(ToolResult::error(
                    "unsupported image format; supported: jpeg, png, gif, webp",
                ))
            }
        };

        if bytes.len() > max_bytes {
            return Ok(ToolResult::error(format!(
                "image exceeds size limit ({} > {} bytes)",
                bytes.len(),
                max_bytes
            )));
        }

        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);

        let payload = serde_json::json!({
            "model": self.model,
            "max_tokens": 1024,
            "messages": [{
                "role": "user",
                "content": [
                    {
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": media_type,
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
        let tool = ImageAnalysisTool::new(
            "key".to_string(),
            "https://api.anthropic.com".to_string(),
            "claude-3-5-sonnet-20241022".to_string(),
        );
        assert_eq!(tool.name(), "image_analysis");
    }

    #[test]
    fn test_detect_media_type_extension() {
        assert_eq!(detect_media_type("photo.jpg", None), Some("image/jpeg"));
        assert_eq!(detect_media_type("photo.jpeg", None), Some("image/jpeg"));
        assert_eq!(detect_media_type("image.png", None), Some("image/png"));
        assert_eq!(detect_media_type("anim.gif", None), Some("image/gif"));
        assert_eq!(detect_media_type("img.webp", None), Some("image/webp"));
        assert_eq!(detect_media_type("file.pdf", None), None);
    }

    #[test]
    fn test_detect_media_type_content_type_wins() {
        assert_eq!(
            detect_media_type("file.png", Some("image/jpeg")),
            Some("image/jpeg")
        );
    }

    #[test]
    fn test_detect_media_type_url_with_query() {
        assert_eq!(
            detect_media_type("https://example.com/img.png?v=1", None),
            Some("image/png")
        );
    }
}
