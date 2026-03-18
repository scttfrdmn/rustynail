use agenkit::{AgentError, Tool, ToolResult};
use async_trait::async_trait;
use scraper::{Html, Selector};
use std::collections::HashMap;

const DEFAULT_MAX_BYTES: usize = 512 * 1024; // 512 KB
const REQUEST_TIMEOUT_SECS: u64 = 15;

pub struct WebFetchTool;

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetches a URL and returns the page content as plain text. \
        HTML is stripped to readable text; non-HTML responses are returned as-is. \
        Parameters: url (required), max_bytes (optional, default 524288)."
    }

    fn parameters_schema(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch"
                },
                "max_bytes": {
                    "type": "integer",
                    "description": "Maximum bytes to read from the response (default 524288 = 512 KB)"
                }
            },
            "required": ["url"]
        }))
    }

    async fn execute(
        &self,
        params: HashMap<String, serde_json::Value>,
    ) -> Result<ToolResult, AgentError> {
        let url = match params.get("url").and_then(|v| v.as_str()) {
            Some(u) => u.to_string(),
            None => return Ok(ToolResult::error("url parameter is required")),
        };

        let max_bytes = params
            .get("max_bytes")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_MAX_BYTES);

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .user_agent("RustyNail/0.8 (+https://github.com/scttfrdmn/rustynail)")
            .build()
            .map_err(|e| AgentError::Internal(format!("failed to build HTTP client: {}", e)))?;

        let response = client
            .get(&url)
            .send()
            .await
            .map_err(|e| AgentError::Internal(format!("fetch error: {}", e)))?;

        if !response.status().is_success() {
            return Ok(ToolResult::error(format!(
                "HTTP {} for {}",
                response.status(),
                url
            )));
        }

        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let bytes = response
            .bytes()
            .await
            .map_err(|e| AgentError::Internal(format!("read error: {}", e)))?;

        let truncated = if bytes.len() > max_bytes {
            &bytes[..max_bytes]
        } else {
            &bytes[..]
        };

        let raw = String::from_utf8_lossy(truncated).into_owned();

        let text = if content_type.contains("text/html") || content_type.is_empty() {
            strip_html(&raw)
        } else {
            raw
        };

        let text = text.trim().to_string();
        if text.is_empty() {
            return Ok(ToolResult::success(serde_json::json!("(empty page)")));
        }

        Ok(ToolResult::success(serde_json::json!(text)))
    }
}

/// Strip HTML tags, skip <script> and <style> subtrees, collapse whitespace.
fn strip_html(html: &str) -> String {
    let document = Html::parse_document(html);

    // Build selectors for elements we want to skip entirely
    let _skip_selector = Selector::parse("script, style, noscript, head").unwrap_or_else(|_| {
        // Fallback: parse a known-good selector
        Selector::parse("script").expect("script selector must parse")
    });

    // Collect text from all text nodes not inside skipped elements
    let mut parts: Vec<String> = Vec::new();

    for node in document.root_element().descendants() {
        // Skip nodes inside script/style/etc
        let mut in_skip = false;
        {
            let mut ancestor = node.parent();
            while let Some(a) = ancestor {
                if let Some(el) = a.value().as_element() {
                    let name = el.name();
                    if matches!(name, "script" | "style" | "noscript" | "head") {
                        in_skip = true;
                        break;
                    }
                }
                ancestor = a.parent();
            }
        }
        if in_skip {
            continue;
        }

        if let Some(text) = node.value().as_text() {
            let t = text.trim();
            if !t.is_empty() {
                parts.push(t.to_string());
            }
        }
    }

    // Join with spaces and collapse runs of whitespace
    let joined = parts.join(" ");
    joined
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_html_basic() {
        let html = r#"<html><head><title>Test</title><style>body{}</style></head>
            <body><h1>Hello</h1><p>World</p><script>alert(1)</script></body></html>"#;
        let text = strip_html(html);
        assert!(text.contains("Hello"));
        assert!(text.contains("World"));
        assert!(!text.contains("alert"));
        assert!(!text.contains("body{}"));
    }

    #[test]
    fn test_tool_name() {
        assert_eq!(WebFetchTool.name(), "web_fetch");
    }
}
