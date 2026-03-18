use crate::agents::StreamEvent;
use crate::gateway::http::AppState;
use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Json, Response},
};
use serde::{Deserialize, Serialize};

// ── Request types ─────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub stream: bool,
    pub user: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

// ── Non-stream response types ─────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<Choice>,
    pub usage: UsageInfo,
}

#[derive(Debug, Serialize)]
pub struct Choice {
    pub index: u32,
    pub message: ChatMessageOut,
    pub finish_reason: String,
}

#[derive(Debug, Serialize)]
pub struct ChatMessageOut {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct UsageInfo {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub total_tokens: usize,
}

// ── SSE chunk types ───────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct ChatCompletionChunk {
    id: String,
    object: String,
    created: u64,
    model: String,
    choices: Vec<ChunkChoice>,
}

#[derive(Debug, Serialize)]
struct ChunkChoice {
    index: u32,
    delta: ChunkDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct ChunkDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
}

// ── Handler ───────────────────────────────────────────────────────────────────

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// OpenAI-compatible `POST /v1/chat/completions`.
///
/// Supports both non-streaming JSON and SSE streaming (`stream: true`).
pub async fn openai_chat_completions(
    State(state): State<AppState>,
    Json(req): Json<ChatCompletionRequest>,
) -> Response {
    let user_id = req.user.unwrap_or_else(|| "openai-api".to_string());
    let content = req
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| m.content.clone())
        .unwrap_or_default();

    let completion_id = format!("chatcmpl-{}", uuid::Uuid::new_v4());
    let created = unix_now();
    let model = req.model.clone();

    if req.stream {
        // SSE streaming path
        let mut stream_rx = state
            .agent_manager
            .clone()
            .process_message_stream(user_id, content)
            .await;

        let id_clone = completion_id.clone();
        let model_clone = model.clone();

        // Build the SSE body as a single string (buffered streaming)
        // axum 0.7 does not expose easy async SSE without the axum-extra crate.
        // We collect the stream into an SSE body and return it with the right headers.
        let mut sse_body = String::new();

        // First chunk: role
        let first_chunk = ChatCompletionChunk {
            id: id_clone.clone(),
            object: "chat.completion.chunk".to_string(),
            created,
            model: model_clone.clone(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: ChunkDelta {
                    role: Some("assistant".to_string()),
                    content: None,
                },
                finish_reason: None,
            }],
        };
        if let Ok(json) = serde_json::to_string(&first_chunk) {
            sse_body.push_str(&format!("data: {}\n\n", json));
        }

        while let Some(event) = stream_rx.recv().await {
            match event {
                StreamEvent::Token(t) => {
                    let chunk = ChatCompletionChunk {
                        id: id_clone.clone(),
                        object: "chat.completion.chunk".to_string(),
                        created,
                        model: model_clone.clone(),
                        choices: vec![ChunkChoice {
                            index: 0,
                            delta: ChunkDelta {
                                role: None,
                                content: Some(t),
                            },
                            finish_reason: None,
                        }],
                    };
                    if let Ok(json) = serde_json::to_string(&chunk) {
                        sse_body.push_str(&format!("data: {}\n\n", json));
                    }
                }
                StreamEvent::Done => {
                    let stop_chunk = ChatCompletionChunk {
                        id: id_clone.clone(),
                        object: "chat.completion.chunk".to_string(),
                        created,
                        model: model_clone.clone(),
                        choices: vec![ChunkChoice {
                            index: 0,
                            delta: ChunkDelta {
                                role: None,
                                content: None,
                            },
                            finish_reason: Some("stop".to_string()),
                        }],
                    };
                    if let Ok(json) = serde_json::to_string(&stop_chunk) {
                        sse_body.push_str(&format!("data: {}\n\n", json));
                    }
                    sse_body.push_str("data: [DONE]\n\n");
                    break;
                }
                StreamEvent::Error(e) => {
                    tracing::warn!("OpenAI SSE: stream error: {}", e);
                    sse_body.push_str("data: [DONE]\n\n");
                    break;
                }
            }
        }

        (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "text/event-stream"),
                (header::CACHE_CONTROL, "no-cache"),
            ],
            sse_body,
        )
            .into_response()
    } else {
        // Non-streaming path
        match state.agent_manager.process_message(&user_id, &content).await {
            Ok(text) => {
                let prompt_tokens = (content.len() + 3) / 4;
                let completion_tokens = (text.len() + 3) / 4;
                let resp = ChatCompletionResponse {
                    id: completion_id,
                    object: "chat.completion".to_string(),
                    created,
                    model,
                    choices: vec![Choice {
                        index: 0,
                        message: ChatMessageOut {
                            role: "assistant".to_string(),
                            content: text,
                        },
                        finish_reason: "stop".to_string(),
                    }],
                    usage: UsageInfo {
                        prompt_tokens,
                        completion_tokens,
                        total_tokens: prompt_tokens + completion_tokens,
                    },
                };
                Json(resp).into_response()
            }
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("agent error: {}", e),
            )
                .into_response(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use std::sync::Arc;
    use tokio::sync::RwLock;
    use tower::ServiceExt;

    fn make_state() -> crate::gateway::http::AppState {
        use crate::config::{AgentsConfig, RateLimitConfig, SkillsConfig};
        use crate::gateway::dashboard::MessageStats;
        use crate::gateway::http::AppState;
        use crate::gateway::rate_limiter::RateLimiter;
        use crate::gateway::user_prefs::UserPreferences;
        use crate::gateway::HotConfig;

        AppState {
            channels: Arc::new(RwLock::new(Vec::new())),
            agent_manager: Arc::new(crate::agents::AgentManager::new(AgentsConfig {
                llm_provider: "stub".to_string(),
                api_key: "unused".to_string(),
                ..Default::default()
            })),
            whatsapp_tx: None,
            whatsapp_verify_token: String::new(),
            telegram_tx: None,
            telegram_webhook_secret: String::new(),
            slack_tx: None,
            slack_signing_secret: String::new(),
            sms_tx: None,
            sms_auth_token: String::new(),
            webhook_endpoints: Vec::new(),
            webhook_tx: None,
            webchat_sessions: None,
            webchat_tx: None,
            teams_tx: None,
            teams_hmac_secret: String::new(),
            user_prefs: Arc::new(UserPreferences::new()),
            stats: MessageStats::new(),
            dashboard_expected_auth: None,
            api_token: None,
            test_channel: None,
            rate_limiter: RateLimiter::new(),
            audit: None,
            hot_config: Arc::new(RwLock::new(HotConfig {
                log_level: "error".to_string(),
                api_token: None,
                rate_limit: RateLimitConfig::default(),
                audit_enabled: false,
                audit_path: String::new(),
            })),
            skills_config: SkillsConfig::default(),
            cron_jobs: Vec::new(),
            allowed_ws_origins: Vec::new(),
        }
    }

    #[tokio::test]
    async fn test_openai_non_stream_returns_json() {
        let state = make_state();
        let router = crate::gateway::http::create_router(state, 1_048_576, 30);
        let body = serde_json::json!({
            "model": "claude",
            "messages": [{"role": "user", "content": "hello"}]
        });
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("Content-Type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let ct = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(ct.contains("json"), "expected json content-type, got: {}", ct);
    }

    #[tokio::test]
    async fn test_openai_stream_returns_event_stream() {
        let state = make_state();
        let router = crate::gateway::http::create_router(state, 1_048_576, 30);
        let body = serde_json::json!({
            "model": "claude",
            "messages": [{"role": "user", "content": "hello"}],
            "stream": true
        });
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("Content-Type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let ct = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            ct.contains("text/event-stream"),
            "expected text/event-stream, got: {}",
            ct
        );
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8_lossy(&body_bytes);
        assert!(
            body_str.contains("data: [DONE]"),
            "SSE body must end with [DONE], got: {}",
            body_str
        );
    }
}
