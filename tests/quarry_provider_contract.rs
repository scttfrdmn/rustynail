//! Wire-level contract test for the `/v1/chat/completions` endpoint as consumed
//! by quarry's OpenAI-compatible Provider.
//!
//! Unlike the unit tests in `src/gateway/openai_compat.rs`, which drive the
//! router in-process via `oneshot`, these bind a real TCP socket and speak HTTP
//! over it with a real client — the same path quarry's subprocess takes when it
//! POSTs to `http://localhost:<http_port>/v1/chat/completions`. Serialisation,
//! header handling and status codes are all exercised for real, so a contract
//! break shows up here even if the in-process handler is fine.
//!
//! Everything runs against `llm_provider: stub`, so there are **no credentials
//! and no network egress**. That is the point: this is the CI-runnable half of
//! the round trip.
//!
//! # Scope
//!
//! This covers the *gateway* side of the contract. The full round trip named in
//! issue #109 also requires quarry's Q2 `provider.OpenAICompatProvider`, which
//! does not exist upstream yet. When it lands, the remaining work is to run that
//! provider against this server; the assertions below are written to match what
//! it will parse, so they should not need to change.

use rustynail::config::{AgentsConfig, RateLimitConfig, SkillsConfig};
use rustynail::gateway::dashboard::MessageStats;
use rustynail::gateway::http::{create_router, AppState};
use rustynail::gateway::rate_limiter::RateLimiter;
use rustynail::gateway::user_prefs::UserPreferences;
use rustynail::gateway::HotConfig;
use std::sync::Arc;
use tokio::sync::RwLock;

/// The model name the stub provider reports. A pinned-looking, explicitly
/// versioned name, because the contract requires the response to name a resolved
/// version rather than an alias.
const STUB_MODEL: &str = "stub-echo-v1";

fn stub_state() -> AppState {
    AppState {
        channels: Arc::new(RwLock::new(Vec::new())),
        agent_manager: Arc::new(rustynail::agents::AgentManager::new(AgentsConfig {
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
        })),
        skills_config: SkillsConfig::default(),
        cron_jobs: Vec::new(),
        allowed_ws_origins: Vec::new(),
    }
}

/// Bind an ephemeral port and serve the gateway on it. Returns the base URL.
///
/// Port 0 lets the OS pick, so concurrent test binaries never collide.
async fn spawn_gateway() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    let app = create_router(stub_state(), 1_048_576, 30);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{}", addr)
}

/// POST a chat-completions request over real HTTP.
async fn post_completion(
    base: &str,
    body: serde_json::Value,
) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", base))
        .json(&body)
        .send()
        .await
        .expect("request failed");
    let status = resp.status();
    let json = resp.json().await.unwrap_or(serde_json::Value::Null);
    (status, json)
}

// ── The provider contract ─────────────────────────────────────────────────────

/// The whole contract in one round trip: what quarry's provider sends, and every
/// field it reads back off the wire.
#[tokio::test]
async fn quarry_provider_round_trip_over_real_http() {
    let base = spawn_gateway().await;

    let (status, json) = post_completion(
        &base,
        serde_json::json!({
            "model": STUB_MODEL,
            "messages": [
                {"role": "system", "content": "You decompose problems."},
                {"role": "user", "content": "What is 2+2?"}
            ],
            "max_tokens": 512,
            "stateless": true
        }),
    )
    .await;

    assert_eq!(status, 200, "body: {}", json);

    // Envelope: the OpenAI shape quarry's provider deserialises.
    assert_eq!(json["object"], "chat.completion");
    assert!(json["id"].as_str().unwrap().starts_with("chatcmpl-"));
    assert!(json["created"].as_u64().unwrap() > 0);

    // Content is present and non-empty.
    let content = json["choices"][0]["message"]["content"].as_str().unwrap();
    assert!(!content.is_empty());
    assert_eq!(json["choices"][0]["message"]["role"], "assistant");
    assert_eq!(json["choices"][0]["index"], 0);
    assert!(!json["choices"][0]["finish_reason"].is_null());

    // The resolved model, not the alias. A caller replaying against this record
    // needs the name of what actually ran.
    assert_eq!(json["model"], STUB_MODEL);

    // Real token counts from the provider.
    let usage = &json["usage"];
    let prompt = usage["prompt_tokens"].as_u64().expect("prompt_tokens");
    let completion = usage["completion_tokens"]
        .as_u64()
        .expect("completion_tokens");
    assert!(prompt > 0 && completion > 0);
    assert_eq!(usage["total_tokens"].as_u64().unwrap(), prompt + completion);

    // Cost in the documented unit, with the documented conversion.
    let cost = &json["cost"];
    assert_eq!(cost["currency"], "USD");
    let usd = cost["amount_usd"].as_f64().expect("amount_usd");
    let micro = cost["micro_usd"].as_i64().expect("micro_usd");
    assert_eq!(
        micro,
        (usd * 1_000_000.0).round() as i64,
        "micro_usd must be round(amount_usd × 1e6)"
    );
    assert!(micro > 0, "a completion with tokens must cost something");
}

/// The system message and every prior turn must reach the provider.
///
/// The stub echoes its prompt, so anything dropped en route is visibly absent.
/// Before this work the handler read only the last user message.
#[tokio::test]
async fn full_messages_array_survives_the_wire() {
    let base = spawn_gateway().await;
    let (status, json) = post_completion(
        &base,
        serde_json::json!({
            "model": STUB_MODEL,
            "messages": [
                {"role": "system", "content": "SYSMARK"},
                {"role": "user", "content": "TURNONE"},
                {"role": "assistant", "content": "TURNTWO"},
                {"role": "user", "content": "TURNTHREE"}
            ],
            "stateless": true
        }),
    )
    .await;
    assert_eq!(status, 200);
    let content = json["choices"][0]["message"]["content"].as_str().unwrap();
    for marker in ["SYSMARK", "TURNONE", "TURNTWO", "TURNTHREE"] {
        assert!(
            content.contains(marker),
            "'{}' never reached the provider: {}",
            marker,
            content
        );
    }
}

/// Independent sub-problems must stay independent.
///
/// quarry treats agreement between sibling sub-answers as a replication signal.
/// If siblings shared a conversation history the signal would be worthless —
/// each would see its predecessors' answers and agree for the wrong reason. Two
/// calls with the *same* `user` must not see each other.
#[tokio::test]
async fn stateless_sub_problems_are_independent() {
    let base = spawn_gateway().await;

    let (_, first) = post_completion(
        &base,
        serde_json::json!({
            "model": STUB_MODEL,
            "messages": [{"role": "user", "content": "SUBPROBLEM_ALPHA"}],
            "user": "quarry-run-1",
            "stateless": true
        }),
    )
    .await;
    let (_, second) = post_completion(
        &base,
        serde_json::json!({
            "model": STUB_MODEL,
            "messages": [{"role": "user", "content": "SUBPROBLEM_BETA"}],
            "user": "quarry-run-1",
            "stateless": true
        }),
    )
    .await;

    let a = first["choices"][0]["message"]["content"].as_str().unwrap();
    let b = second["choices"][0]["message"]["content"].as_str().unwrap();
    assert!(a.contains("SUBPROBLEM_ALPHA"));
    assert!(b.contains("SUBPROBLEM_BETA"));
    assert!(
        !b.contains("SUBPROBLEM_ALPHA"),
        "sibling sub-problems shared state: {}",
        b
    );
}

/// A cap/request-shape refusal and a transport fault must be distinguishable
/// without parsing a human-readable message.
///
/// agate shipped an overloaded `402` covering four causes; quarry had to
/// string-match `detail` and treated everything it could not classify as a
/// run-failing fault. Each cause here carries its own `code`.
#[tokio::test]
async fn errors_are_machine_classifiable() {
    let base = spawn_gateway().await;

    let (status, json) = post_completion(
        &base,
        serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "hi"}],
            "stateless": true
        }),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(json["error"]["code"], "model_not_available");

    let (status, json) = post_completion(
        &base,
        serde_json::json!({
            "model": STUB_MODEL,
            "messages": [],
            "stateless": true
        }),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(json["error"]["code"], "no_messages");

    let (status, json) = post_completion(
        &base,
        serde_json::json!({
            "model": STUB_MODEL,
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 0,
            "stateless": true
        }),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(json["error"]["code"], "invalid_max_tokens");
}

/// Same request twice must report the same model string. Non-deterministic
/// routing breaks byte-identical replay.
#[tokio::test]
async fn model_naming_is_stable_across_calls() {
    let base = spawn_gateway().await;
    let req = serde_json::json!({
        "model": STUB_MODEL,
        "messages": [{"role": "user", "content": "deterministic"}],
        "stateless": true
    });
    let (_, a) = post_completion(&base, req.clone()).await;
    let (_, b) = post_completion(&base, req).await;
    assert_eq!(a["model"], b["model"]);
    assert_eq!(a["usage"]["prompt_tokens"], b["usage"]["prompt_tokens"]);
}

/// An unreachable provider is a `502` with its own code, and carries no cost.
///
/// This is the "retry" class, and it must be separable from the `400` request-
/// shape classes above: a caller that cannot tell them apart either retries a
/// malformed request forever or gives up on a transient fault.
///
/// (Cost omission for an unpriced *but reachable* model is covered by
/// `test_unpriced_model_yields_no_cost` in `src/agents/manager.rs`, which can
/// reach the pricing lookup this test fails before.)
#[tokio::test]
async fn unreachable_provider_is_a_distinct_retryable_error() {
    let mut state = stub_state();
    state.agent_manager = Arc::new(rustynail::agents::AgentManager::new(AgentsConfig {
        llm_provider: "openai-compat".to_string(),
        llm_model: "totally-unpriced-model-xyz".to_string(),
        api_key: "unused".to_string(),
        api_base: Some("http://127.0.0.1:1".to_string()), // refused connection
        ..Default::default()
    }));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = create_router(state, 1_048_576, 30);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let base = format!("http://{}", addr);

    let (status, json) = post_completion(
        &base,
        serde_json::json!({
            "model": "totally-unpriced-model-xyz",
            "messages": [{"role": "user", "content": "hi"}],
            "stateless": true
        }),
    )
    .await;

    // The provider is unreachable, so this is an upstream failure — and it must
    // be reported as one, distinctly from a request-shape error.
    assert_eq!(status, 502, "body: {}", json);
    assert_eq!(json["error"]["code"], "upstream_provider_error");
    assert!(
        json.get("cost").is_none(),
        "an error response must not carry a cost"
    );
}
