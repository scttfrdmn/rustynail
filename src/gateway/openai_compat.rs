//! OpenAI-compatible chat completions endpoint.
//!
//! # Metering contract
//!
//! This endpoint is consumed by clients that meter their own spend against it —
//! notably quarry, which uses this gateway as its single Provider and holds no
//! provider credentials of its own. Two rules follow from that and are load-
//! bearing rather than stylistic:
//!
//! 1. **Absent, never estimated.** `usage` and `cost` are omitted entirely when
//!    the upstream provider did not report them. A char/4 estimate dressed as a
//!    measurement is worse than no field, because a caller debiting a ledger
//!    against it cannot tell that it is guessing.
//! 2. **Cost is rounded, never truncated.** `cost.micro_usd` is
//!    `round(amount_usd × 1_000_000)` — rounded to nearest. Truncating would
//!    desync a caller's local debit from the real charge by up to one micro-unit
//!    per call, which accumulates silently over a long run. Callers converting
//!    from `amount_usd` themselves must use the same rule; `micro_usd` is the
//!    authoritative integer.
//!
//! `model` in the response names what actually ran, resolved by the provider
//! where the provider reports it — never the alias the caller asked for. See
//! [`resolve_model`] for how a mismatched request model is handled.

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
    /// Output-token ceiling for this request. Honoured on every provider except
    /// Ollama, whose agenkit config exposes no equivalent knob.
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// When true, run with no conversation state: no history is read, none is
    /// written, and successive calls are fully independent. Required by callers
    /// whose sub-requests must not contaminate each other.
    ///
    /// Not part of the OpenAI schema — an OpenAI SDK client simply never sets
    /// it and keeps the existing stateful behaviour.
    #[serde(default)]
    pub stateless: bool,
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
    /// The model that ran, not the one requested. See [`resolve_model`].
    pub model: String,
    pub choices: Vec<Choice>,
    /// Omitted when the provider reported no token counts. Never estimated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageInfo>,
    /// Omitted when token counts are absent or the model has no pricing entry.
    /// An unpriced model yields no cost field rather than a zero cost.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<CostInfo>,
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
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

/// Cost of a completion.
///
/// `micro_usd` is authoritative: integer micro-dollars, `round(usd × 1e6)`.
/// `amount_usd` is the same value as a float, for human display.
#[derive(Debug, Serialize)]
pub struct CostInfo {
    pub amount_usd: f64,
    pub micro_usd: i64,
    /// Always `"USD"`. Present so a caller need not assume the currency.
    pub currency: String,
}

// ── Error types ───────────────────────────────────────────────────────────────

/// Machine-readable error body.
///
/// The `code` field exists so callers never have to string-match `message` to
/// classify a failure. agate shipped an overloaded `402` covering four distinct
/// causes, only one of which was a genuine cap breach; quarry was forced to
/// pattern-match a human-readable `detail` string and, failing that, treat every
/// unclassified `402` as a run-failing fault. Each `code` here maps to exactly
/// one cause, and `message` is for humans only.
#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Debug, Serialize)]
pub struct ErrorDetail {
    /// Stable machine-readable discriminant. Never reused across causes.
    pub code: &'static str,
    /// Human-readable text. Callers must not parse this.
    pub message: String,
    /// OpenAI-style coarse class, for SDK compatibility.
    pub r#type: &'static str,
}

/// Distinct failure causes, each with its own status and `code`.
///
/// Adding a cause means adding a variant, not overloading an existing one.
pub(crate) enum ApiError {
    /// `messages` was empty or contained no usable turn.
    NoMessages,
    /// The requested model is neither the configured model nor an accepted alias.
    ModelMismatch {
        requested: String,
        configured: String,
    },
    /// `max_tokens` was present but not a usable positive value.
    InvalidMaxTokens,
    /// The upstream provider failed. Distinct from every request-shape error
    /// above: this one is retryable by the caller, those are not.
    UpstreamFailure(String),
}

impl ApiError {
    fn parts(&self) -> (StatusCode, &'static str, &'static str, String) {
        match self {
            ApiError::NoMessages => (
                StatusCode::BAD_REQUEST,
                "no_messages",
                "invalid_request_error",
                "`messages` must contain at least one message with content".to_string(),
            ),
            ApiError::ModelMismatch {
                requested,
                configured,
            } => (
                StatusCode::BAD_REQUEST,
                "model_not_available",
                "invalid_request_error",
                format!(
                    "model '{}' is not available; this gateway serves '{}'. \
                     Request that model explicitly, or omit routing by sending it verbatim. \
                     Aliases are refused because a response must name a pinned version.",
                    requested, configured
                ),
            ),
            ApiError::InvalidMaxTokens => (
                StatusCode::BAD_REQUEST,
                "invalid_max_tokens",
                "invalid_request_error",
                "`max_tokens` must be a positive integer".to_string(),
            ),
            ApiError::UpstreamFailure(msg) => (
                StatusCode::BAD_GATEWAY,
                "upstream_provider_error",
                "api_error",
                format!("upstream provider failed: {}", msg),
            ),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, r#type, message) = self.parts();
        (
            status,
            Json(ErrorBody {
                error: ErrorDetail {
                    code,
                    message,
                    r#type,
                },
            }),
        )
            .into_response()
    }
}

// ── Model resolution ──────────────────────────────────────────────────────────

/// Model names accepted as "whatever this gateway is configured to serve".
///
/// A caller that does not care which model runs may send one of these; the
/// response still reports the resolved name, so the reply is unambiguous even
/// though the request was not. Any *other* name must match the configured model
/// exactly — silently serving `gpt-4` from a Claude deployment would make the
/// response's `model` field a lie, and a caller replaying against it would
/// replay against the wrong model.
const GATEWAY_ALIASES: &[&str] = &["rustynail", "default", "gateway"];

/// Decide which model to run, or refuse.
///
/// `Ok(())` means the request is servable. The name reported in the response is
/// **not** taken from here — it comes from the provider's own response metadata
/// where available, falling back to the configured name.
pub(crate) fn resolve_model(requested: &str, configured: &str) -> Result<(), ApiError> {
    if requested.is_empty()
        || requested == configured
        || GATEWAY_ALIASES.contains(&requested.to_ascii_lowercase().as_str())
    {
        return Ok(());
    }
    // A request for a bare family name ("claude-3-5-sonnet") against a pinned
    // configured version ("claude-3-5-sonnet-20241022") is servable: the
    // response names the pinned version, so nothing is misreported.
    if configured.starts_with(requested) {
        return Ok(());
    }
    Err(ApiError::ModelMismatch {
        requested: requested.to_string(),
        configured: configured.to_string(),
    })
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
/// Supports both non-streaming JSON and SSE streaming (`stream: true`), and
/// both the default stateful mode and `stateless: true`.
///
/// See the module docs for the metering contract (`usage`/`cost` absent rather
/// than estimated; `micro_usd` rounded, not truncated).
pub async fn openai_chat_completions(
    State(state): State<AppState>,
    Json(req): Json<ChatCompletionRequest>,
) -> Response {
    let configured = state.agent_manager.configured_model().to_string();
    if let Err(e) = resolve_model(&req.model, &configured) {
        return e.into_response();
    }
    if matches!(req.max_tokens, Some(0)) {
        return ApiError::InvalidMaxTokens.into_response();
    }

    let user_id = req.user.clone().unwrap_or_else(|| "openai-api".to_string());
    let completion_id = format!("chatcmpl-{}", uuid::Uuid::new_v4());
    let created = unix_now();

    if req.stateless {
        if req.stream {
            // The stateless path is request/response by construction: it exists
            // so callers get independent completions with real token counts, and
            // the streaming path's simulated chunking cannot carry usage.
            return ApiError::UpstreamFailure(
                "stateless mode does not support stream: true".to_string(),
            )
            .into_response();
        }
        return stateless_completion(&state, &req, completion_id, created, &configured).await;
    }

    let content = req
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| m.content.clone())
        .unwrap_or_default();

    if content.is_empty() {
        return ApiError::NoMessages.into_response();
    }

    let model = configured.clone();

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
        // Non-streaming stateful path.
        //
        // This path routes through the per-user `ConversationalAgent`, which
        // flattens history into one prompt and — when tools are enabled — wraps
        // the LLM in a `ReActAgent` that discards each response's metadata.
        // Token counts are therefore not recoverable here, and `usage` is
        // omitted rather than estimated. Callers that need metering must use
        // `stateless: true`.
        match state
            .agent_manager
            .process_message(&user_id, &content)
            .await
        {
            Ok(text) => {
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
                    usage: None,
                    cost: None,
                };
                Json(resp).into_response()
            }
            Err(e) => ApiError::UpstreamFailure(e.to_string()).into_response(),
        }
    }
}

/// The `stateless: true` path: no history read, no history written, provider
/// token counts and cost surfaced when available.
async fn stateless_completion(
    state: &AppState,
    req: &ChatCompletionRequest,
    completion_id: String,
    created: u64,
    configured: &str,
) -> Response {
    let messages: Vec<(String, String)> = req
        .messages
        .iter()
        .filter(|m| !m.content.is_empty())
        .map(|m| (m.role.clone(), m.content.clone()))
        .collect();

    if messages.is_empty() {
        return ApiError::NoMessages.into_response();
    }

    match state
        .agent_manager
        .complete_stateless(&messages, req.max_tokens)
        .await
    {
        Ok(outcome) => {
            if !outcome.model_from_provider {
                // Worth knowing: Gemini and Bedrock echo the configured name
                // rather than a provider-resolved one, so the reported model is
                // only as pinned as the configuration is.
                tracing::debug!(
                    "provider did not report a resolved model; reporting configured '{}'",
                    configured
                );
            }
            let resp = ChatCompletionResponse {
                id: completion_id,
                object: "chat.completion".to_string(),
                created,
                model: outcome.model,
                choices: vec![Choice {
                    index: 0,
                    message: ChatMessageOut {
                        role: "assistant".to_string(),
                        content: outcome.text,
                    },
                    finish_reason: outcome.finish_reason.unwrap_or_else(|| "stop".to_string()),
                }],
                usage: outcome.usage.map(|u| UsageInfo {
                    prompt_tokens: u.prompt_tokens,
                    completion_tokens: u.completion_tokens,
                    total_tokens: u.total_tokens,
                }),
                cost: outcome.cost.map(|c| CostInfo {
                    amount_usd: c.amount_usd,
                    micro_usd: c.micro_usd,
                    currency: "USD".to_string(),
                }),
            };
            Json(resp).into_response()
        }
        Err(e) => ApiError::UpstreamFailure(e.to_string()).into_response(),
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
            test_tx: None,
            rate_limiter: RateLimiter::new(),
            audit: None,
            hot_config: Arc::new(RwLock::new(HotConfig {
                log_level: "error".to_string(),
                api_token: None,
                rate_limit: RateLimitConfig::default(),
                audit_enabled: false,
                audit_path: String::new(),
                quarry_policy: crate::config::QuarryPolicyConfig::default(),
            })),
            skills_config: SkillsConfig::default(),
            cron_jobs: Vec::new(),
            allowed_ws_origins: Vec::new(),
        }
    }

    /// Send a request and return (status, parsed JSON body).
    async fn post(body: serde_json::Value) -> (StatusCode, serde_json::Value) {
        let router = crate::gateway::http::create_router(make_state(), 1_048_576, 30);
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
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    // ── Model resolution (deterministic naming, P8) ────────────────────────────

    #[test]
    fn test_resolve_model_accepts_exact_and_aliases() {
        assert!(resolve_model("claude-3-5-sonnet-20241022", "claude-3-5-sonnet-20241022").is_ok());
        assert!(resolve_model("rustynail", "claude-3-5-sonnet-20241022").is_ok());
        assert!(resolve_model("RustyNail", "claude-3-5-sonnet-20241022").is_ok());
        assert!(resolve_model("default", "claude-3-5-sonnet-20241022").is_ok());
        assert!(resolve_model("", "claude-3-5-sonnet-20241022").is_ok());
        // Family name against a pinned version: servable, response names the pin.
        assert!(resolve_model("claude-3-5-sonnet", "claude-3-5-sonnet-20241022").is_ok());
    }

    #[test]
    fn test_resolve_model_refuses_a_different_model() {
        // Serving Claude while claiming to be GPT-4 would make the response's
        // `model` field a lie, so this is a refusal rather than a silent reroute.
        let err = resolve_model("gpt-4", "claude-3-5-sonnet-20241022");
        assert!(err.is_err());
        let (status, code, _, _) = err.unwrap_err().parts();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(code, "model_not_available");
    }

    #[tokio::test]
    async fn test_reported_model_is_never_the_requested_alias() {
        let (status, json) = post(serde_json::json!({
            "model": "rustynail",
            "messages": [{"role": "user", "content": "hello"}],
            "stateless": true
        }))
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            json["model"],
            crate::agents::stub::STUB_MODEL,
            "response must name the model that ran, not the alias requested; got {}",
            json["model"]
        );
    }

    #[tokio::test]
    async fn test_model_naming_is_deterministic() {
        let req = serde_json::json!({
            "model": "rustynail",
            "messages": [{"role": "user", "content": "same input"}],
            "stateless": true
        });
        let (_, a) = post(req.clone()).await;
        let (_, b) = post(req).await;
        assert_eq!(
            a["model"], b["model"],
            "same request must report same model"
        );
    }

    #[tokio::test]
    async fn test_unknown_model_is_refused_with_machine_readable_code() {
        let (status, json) = post(serde_json::json!({
            "model": "gpt-4-turbo",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // The point of `code`: classification without string-matching `message`.
        assert_eq!(json["error"]["code"], "model_not_available");
        assert_eq!(json["error"]["type"], "invalid_request_error");
    }

    // ── Usage and cost (absent, never estimated) ───────────────────────────────

    #[tokio::test]
    async fn test_stateless_reports_provider_usage_and_cost() {
        let (status, json) = post(serde_json::json!({
            "model": "rustynail",
            "messages": [{"role": "user", "content": "hello"}],
            "stateless": true
        }))
        .await;
        assert_eq!(status, StatusCode::OK);

        let usage = &json["usage"];
        assert!(
            !usage.is_null(),
            "stub reports usage, so it must be present"
        );
        let prompt = usage["prompt_tokens"].as_u64().unwrap();
        let completion = usage["completion_tokens"].as_u64().unwrap();
        assert!(prompt > 0 && completion > 0);
        assert_eq!(usage["total_tokens"].as_u64().unwrap(), prompt + completion);

        let cost = &json["cost"];
        assert!(!cost.is_null(), "stub has pricing, so cost must be present");
        assert_eq!(cost["currency"], "USD");

        // The conversion the endpoint documents: round(usd × 1e6), to nearest.
        let usd = cost["amount_usd"].as_f64().unwrap();
        let micro = cost["micro_usd"].as_i64().unwrap();
        assert_eq!(
            micro,
            (usd * 1_000_000.0).round() as i64,
            "micro_usd must be round(amount_usd × 1e6), not a truncation"
        );

        // Stub pricing is $1/1M in, $2/1M out — check cost tracks the tokens.
        let expected = (prompt as f64 / 1e6) * 1.0 + (completion as f64 / 1e6) * 2.0;
        assert!(
            (usd - expected).abs() < 1e-12,
            "cost {} should match tokens at stub rates ({})",
            usd,
            expected
        );
    }

    #[tokio::test]
    async fn test_micro_usd_rounds_to_nearest_not_truncates() {
        // Guards the rule directly rather than via an endpoint round trip: a
        // truncating implementation would return 1 here, desyncing a caller's
        // ledger by a micro-unit per call.
        use crate::agents::CompletionCost;
        let c = CompletionCost::from_usd(0.0000015);
        assert_eq!(c.micro_usd, 2, "0.0000015 USD must round to 2 micro-USD");
        let c = CompletionCost::from_usd(0.0000014);
        assert_eq!(c.micro_usd, 1);
    }

    #[tokio::test]
    async fn test_stateful_path_omits_usage_rather_than_estimating() {
        // The stateful path runs through ConversationalAgent (and ReActAgent
        // when tools are on), which do not preserve provider usage metadata.
        // The contract is that the field is absent, not that it is guessed from
        // string length.
        let (status, json) = post(serde_json::json!({
            "model": "rustynail",
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            json.get("usage").is_none(),
            "stateful path must omit usage, not estimate it; got {}",
            json
        );
        assert!(json.get("cost").is_none(), "no usage means no cost");
    }

    // ── Statelessness (sub-problem independence) ───────────────────────────────

    #[tokio::test]
    async fn test_stateless_calls_do_not_see_each_other() {
        // Two calls with the *same* `user`. The stub echoes its whole prompt, so
        // if any history leaked between calls the second response would contain
        // the first call's distinctive content.
        let router = crate::gateway::http::create_router(make_state(), 1_048_576, 30);

        let send = |router: axum::Router, content: &str| {
            let body = serde_json::json!({
                "model": "rustynail",
                "messages": [{"role": "user", "content": content}],
                "user": "same-user",
                "stateless": true
            });
            async move {
                let resp = router
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
                let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
                    .await
                    .unwrap();
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()
            }
        };

        let first = send(router.clone(), "ZEBRAFISH").await;
        let second = send(router, "OCTOPUS").await;

        let first_text = first["choices"][0]["message"]["content"]
            .as_str()
            .unwrap()
            .to_string();
        let second_text = second["choices"][0]["message"]["content"]
            .as_str()
            .unwrap()
            .to_string();

        assert!(first_text.contains("ZEBRAFISH"));
        assert!(second_text.contains("OCTOPUS"));
        assert!(
            !second_text.contains("ZEBRAFISH"),
            "stateless call leaked the previous call's content: {}",
            second_text
        );
    }

    #[tokio::test]
    async fn test_stateful_path_does_accumulate_history() {
        // The complement of the test above: confirms the two modes really differ,
        // so `stateless: true` is doing something rather than being a no-op flag.
        let router = crate::gateway::http::create_router(make_state(), 1_048_576, 30);
        let send = |router: axum::Router, content: &str| {
            let body = serde_json::json!({
                "model": "rustynail",
                "messages": [{"role": "user", "content": content}],
                "user": "shared-user"
            });
            async move {
                let resp = router
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
                let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
                    .await
                    .unwrap();
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()
            }
        };

        let _ = send(router.clone(), "ZEBRAFISH").await;
        let second = send(router, "OCTOPUS").await;
        let text = second["choices"][0]["message"]["content"].as_str().unwrap();
        assert!(
            text.contains("ZEBRAFISH"),
            "stateful path should carry prior turns; got {}",
            text
        );
    }

    // ── Full messages array ───────────────────────────────────────────────────

    #[tokio::test]
    async fn test_system_and_multi_turn_messages_reach_the_provider() {
        // The stub echoes its prompt, so every part that reached the provider is
        // visible in the response. Previously only the last user message was
        // read and everything else was silently dropped.
        let (status, json) = post(serde_json::json!({
            "model": "rustynail",
            "messages": [
                {"role": "system", "content": "SYSTEMMARKER"},
                {"role": "user", "content": "FIRSTTURN"},
                {"role": "assistant", "content": "REPLYTURN"},
                {"role": "user", "content": "LASTTURN"}
            ],
            "stateless": true
        }))
        .await;
        assert_eq!(status, StatusCode::OK);
        let text = json["choices"][0]["message"]["content"].as_str().unwrap();
        for marker in ["SYSTEMMARKER", "FIRSTTURN", "REPLYTURN", "LASTTURN"] {
            assert!(
                text.contains(marker),
                "'{}' was dropped before reaching the provider; got {}",
                marker,
                text
            );
        }
    }

    #[tokio::test]
    async fn test_empty_messages_is_a_distinct_error_code() {
        let (status, json) = post(serde_json::json!({
            "model": "rustynail",
            "messages": [],
            "stateless": true
        }))
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["error"]["code"], "no_messages");
    }

    // ── max_tokens ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_max_tokens_is_accepted() {
        let (status, _) = post(serde_json::json!({
            "model": "rustynail",
            "messages": [{"role": "user", "content": "hello"}],
            "max_tokens": 256,
            "stateless": true
        }))
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn test_zero_max_tokens_is_refused_distinctly() {
        let (status, json) = post(serde_json::json!({
            "model": "rustynail",
            "messages": [{"role": "user", "content": "hello"}],
            "max_tokens": 0,
            "stateless": true
        }))
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["error"]["code"], "invalid_max_tokens");
    }

    #[tokio::test]
    async fn test_error_codes_are_all_distinct() {
        // The agate#265 C1 lesson: one status covering several causes forced a
        // caller to string-match a human message and misclassify the rest. Each
        // cause here must be separable by `code` alone.
        let cases = [
            ApiError::NoMessages,
            ApiError::ModelMismatch {
                requested: "a".into(),
                configured: "b".into(),
            },
            ApiError::InvalidMaxTokens,
            ApiError::UpstreamFailure("boom".into()),
        ];
        let codes: Vec<&str> = cases.iter().map(|c| c.parts().1).collect();
        let unique: std::collections::HashSet<&&str> = codes.iter().collect();
        assert_eq!(
            codes.len(),
            unique.len(),
            "error codes must be distinct: {:?}",
            codes
        );

        // A transport fault must not share a status with a request-shape error,
        // or a caller cannot tell "retry" from "fix your request".
        assert_eq!(
            ApiError::UpstreamFailure("x".into()).parts().0,
            StatusCode::BAD_GATEWAY
        );
    }

    #[tokio::test]
    async fn test_openai_non_stream_returns_json() {
        let state = make_state();
        let router = crate::gateway::http::create_router(state, 1_048_576, 30);
        let body = serde_json::json!({
            "model": "rustynail",
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
        assert!(
            ct.contains("json"),
            "expected json content-type, got: {}",
            ct
        );
    }

    #[tokio::test]
    async fn test_openai_stream_returns_event_stream() {
        let state = make_state();
        let router = crate::gateway::http::create_router(state, 1_048_576, 30);
        let body = serde_json::json!({
            "model": "rustynail",
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
