mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use rustynail::gateway::http::create_router;
use tower::ServiceExt;

// ── Helper ────────────────────────────────────────────────────────────────────

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_admin_clear_memory_returns_200() {
    let app = create_router(common::make_test_state(), 1_048_576, 30);
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri("/admin/memory/user123")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_admin_channels_health_returns_json() {
    let app = create_router(common::make_test_state(), 1_048_576, 30);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/admin/channels/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert!(json.is_array());
}

#[tokio::test]
async fn test_admin_skills_reload_returns_200() {
    let app = create_router(common::make_test_state(), 1_048_576, 30);
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/admin/skills/reload")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_admin_requires_bearer_when_configured() {
    use rustynail::config::RateLimitConfig;
    use rustynail::gateway::HotConfig;
    use tokio::sync::RwLock;

    let mut state = common::make_test_state();
    // Set an API token so bearer auth is enforced
    let token = "secret-admin-token".to_string();
    state.api_token = Some(token.clone());
    state.hot_config = std::sync::Arc::new(RwLock::new(HotConfig {
        log_level: "error".to_string(),
        api_token: Some(token),
        rate_limit: RateLimitConfig::default(),
        audit_enabled: false,
        audit_path: String::new(),
    }));

    let app = create_router(state, 1_048_576, 30);
    // Request without auth header → 401
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri("/admin/memory/someuser")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_admin_channels_health_structure() {
    use rustynail::channels::Channel;
    use tokio::sync::RwLock;

    // Compose a state with a recording channel so health entries exist
    let mut state = common::make_test_state();
    let recording = common::RecordingChannel::new("test-ch-1");
    let channels: Vec<Box<dyn Channel>> = vec![Box::new(recording)];
    state.channels = std::sync::Arc::new(RwLock::new(channels));

    let app = create_router(state, 1_048_576, 30);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/admin/channels/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let arr = json.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    let entry = &arr[0];
    assert!(entry["id"].is_string());
    assert!(entry["name"].is_string());
    assert!(entry["health"].is_string());
}
