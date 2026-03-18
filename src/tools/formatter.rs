use agenkit::{AgentError, Tool, ToolResult};
use async_trait::async_trait;
use std::collections::HashMap;

/// Pure-Rust text formatting utility for multi-channel message normalisation.
///
/// Useful when the same content needs to be adapted for different platforms:
/// Discord (Markdown), WhatsApp/Telegram (plain text), Slack (mrkdwn), etc.
///
/// Operations: `to_markdown`, `to_plain`, `truncate`, `wrap`, `summarize_header`.
pub struct FormatterTool;

#[async_trait]
impl Tool for FormatterTool {
    fn name(&self) -> &str {
        "formatter"
    }

    fn description(&self) -> &str {
        "Format and transform text for different messaging platforms. \
         Operations: to_markdown, to_plain, truncate, wrap, summarize_header."
    }

    fn parameters_schema(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "op": {
                    "type": "string",
                    "enum": ["to_markdown", "to_plain", "truncate", "wrap", "summarize_header"],
                    "description": "Transformation to apply"
                },
                "text": {
                    "type": "string",
                    "description": "Input text to transform (required for all ops)"
                },
                "max_length": {
                    "type": "integer",
                    "description": "Maximum length in characters (used by truncate)"
                },
                "width": {
                    "type": "integer",
                    "description": "Line width for word-wrapping (used by wrap; default 80)"
                }
            },
            "required": ["op", "text"]
        }))
    }

    async fn execute(
        &self,
        params: HashMap<String, serde_json::Value>,
    ) -> Result<ToolResult, AgentError> {
        let op = match params.get("op").and_then(|v| v.as_str()) {
            Some(op) => op,
            None => return Ok(ToolResult::error("missing required parameter 'op'")),
        };
        let text = match params.get("text").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => return Ok(ToolResult::error("missing required parameter 'text'")),
        };

        match op {
            "to_markdown" => {
                // Minimal plain-text → Markdown: wrap code-like words, preserve line breaks
                let result = to_markdown(text);
                Ok(ToolResult::success(serde_json::json!({ "result": result })))
            }

            "to_plain" => {
                let result = strip_markdown(text);
                Ok(ToolResult::success(serde_json::json!({ "result": result })))
            }

            "truncate" => {
                let max_length = params
                    .get("max_length")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(280) as usize;
                let result = truncate(text, max_length);
                Ok(ToolResult::success(serde_json::json!({ "result": result })))
            }

            "wrap" => {
                let width = params.get("width").and_then(|v| v.as_u64()).unwrap_or(80) as usize;
                let result = word_wrap(text, width);
                Ok(ToolResult::success(serde_json::json!({ "result": result })))
            }

            "summarize_header" => {
                // Extract the first non-empty line as a short header/subject
                let header = text
                    .lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty())
                    .unwrap_or("")
                    .to_string();
                // Trim to 100 chars
                let header = truncate(&header, 100);
                Ok(ToolResult::success(serde_json::json!({ "result": header })))
            }

            unknown => Ok(ToolResult::error(format!("unknown op '{}'", unknown))),
        }
    }
}

// ── Pure transformation functions ─────────────────────────────────────────────

/// Minimal plain-text → Markdown conversion:
/// - Words in ALL_CAPS become bold
/// - Words surrounded by backtick hints (e.g. `code`) are left as-is
/// - URL-like tokens are wrapped as `[url](url)`
fn to_markdown(text: &str) -> String {
    text.split_whitespace()
        .map(|word| {
            if looks_like_url(word) {
                format!("[{}]({})", word, word)
            } else if word.len() > 1 && word.chars().all(|c| c.is_uppercase() || c == '_') {
                format!("**{}**", word)
            } else {
                word.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Strip common Markdown syntax to produce plain text.
fn strip_markdown(text: &str) -> String {
    let mut result = text.to_string();
    // Remove bold/italic markers: **, *, __, _
    for marker in &["**", "__", "*", "_"] {
        result = result.replace(marker, "");
    }
    // Remove inline code backticks
    result = result.replace('`', "");
    // Remove Markdown links [text](url) → text
    let mut out = String::with_capacity(result.len());
    let mut chars = result.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '[' {
            let mut inner = String::new();
            let mut found_close = false;
            for ch in chars.by_ref() {
                if ch == ']' {
                    found_close = true;
                    break;
                }
                inner.push(ch);
            }
            if found_close {
                // Consume the (url) part if present
                if chars.peek() == Some(&'(') {
                    chars.next();
                    for ch in chars.by_ref() {
                        if ch == ')' {
                            break;
                        }
                    }
                }
                out.push_str(&inner);
            } else {
                out.push('[');
                out.push_str(&inner);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Truncate text to `max_length` characters, appending `…` if truncated.
fn truncate(text: &str, max_length: usize) -> String {
    if text.chars().count() <= max_length {
        return text.to_string();
    }
    // Truncate at a word boundary where possible
    let truncated: String = text.chars().take(max_length.saturating_sub(1)).collect();
    let trimmed = truncated.trim_end();
    format!("{}…", trimmed)
}

/// Wrap text at word boundaries to the given column width.
fn word_wrap(text: &str, width: usize) -> String {
    if width == 0 {
        return text.to_string();
    }
    let mut lines: Vec<String> = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            if line.is_empty() {
                line.push_str(word);
            } else if line.len() + 1 + word.len() <= width {
                line.push(' ');
                line.push_str(word);
            } else {
                lines.push(line.clone());
                line = word.to_string();
            }
        }
        lines.push(line);
    }
    lines.join("\n")
}

fn looks_like_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn run(op: &str, text: &str, extra: serde_json::Value) -> ToolResult {
        let mut params: HashMap<String, serde_json::Value> = HashMap::new();
        params.insert("op".to_string(), serde_json::json!(op));
        params.insert("text".to_string(), serde_json::json!(text));
        if let serde_json::Value::Object(map) = extra {
            for (k, v) in map {
                params.insert(k, v);
            }
        }
        FormatterTool.execute(params).await.unwrap()
    }

    #[tokio::test]
    async fn test_to_plain_strips_bold() {
        let r = run("to_plain", "**Hello** world", serde_json::json!({})).await;
        assert!(r.success);
        assert_eq!(r.output["result"].as_str().unwrap(), "Hello world");
    }

    #[tokio::test]
    async fn test_to_plain_strips_links() {
        let r = run(
            "to_plain",
            "Click [here](https://example.com) for more",
            serde_json::json!({}),
        )
        .await;
        assert!(r.success);
        assert_eq!(r.output["result"].as_str().unwrap(), "Click here for more");
    }

    #[tokio::test]
    async fn test_truncate() {
        let r = run(
            "truncate",
            "Hello world",
            serde_json::json!({ "max_length": 7 }),
        )
        .await;
        assert!(r.success);
        let result = r.output["result"].as_str().unwrap();
        assert!(result.contains('…'));
        assert!(result.chars().count() <= 7);
    }

    #[tokio::test]
    async fn test_truncate_short_text_unchanged() {
        let r = run("truncate", "Hi", serde_json::json!({ "max_length": 100 })).await;
        assert!(r.success);
        assert_eq!(r.output["result"].as_str().unwrap(), "Hi");
    }

    #[tokio::test]
    async fn test_wrap() {
        let r = run(
            "wrap",
            "one two three four five six",
            serde_json::json!({ "width": 10 }),
        )
        .await;
        assert!(r.success);
        let result = r.output["result"].as_str().unwrap();
        for line in result.lines() {
            assert!(line.len() <= 10, "line too long: '{}'", line);
        }
    }

    #[tokio::test]
    async fn test_summarize_header() {
        let r = run(
            "summarize_header",
            "\n\nMeeting notes\n\nAction items...",
            serde_json::json!({}),
        )
        .await;
        assert!(r.success);
        assert_eq!(r.output["result"].as_str().unwrap(), "Meeting notes");
    }

    #[tokio::test]
    async fn test_to_markdown_url() {
        let r = run(
            "to_markdown",
            "Visit https://example.com today",
            serde_json::json!({}),
        )
        .await;
        assert!(r.success);
        let result = r.output["result"].as_str().unwrap();
        assert!(result.contains("[https://example.com](https://example.com)"));
    }
}
