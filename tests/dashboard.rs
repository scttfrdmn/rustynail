mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use rustynail::gateway::http::{create_router, AppState};
use rustynail::gateway::rate_limiter::RateLimiter;
use rustynail::gateway::HotConfig;
use rustynail::config::RateLimitConfig;
use tower::ServiceExt;

fn make_state_with_auth(password: &str) -> AppState {
    let encoded =
        base64::engine::general_purpose::STANDARD.encode(format!("rustynail:{}", password));
    let expected = format!("Basic {}", encoded);
    let mut state = common::make_test_state();
    state.dashboard_expected_auth = Some(expected);
    state
}

// ── /dashboard (HTML) ─────────────────────────────────────────────────────────

#[tokio::test]
async fn dashboard_html_no_auth_configured_returns_200() {
    let app = create_router(common::make_test_state(), 1_048_576, 30);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/dashboard")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.contains("text/html"), "expected text/html, got: {}", ct);
}

#[tokio::test]
async fn dashboard_html_correct_credentials_returns_200() {
    let state = make_state_with_auth("secret");
    let app = create_router(state, 1_048_576, 30);
    let creds = base64::engine::general_purpose::STANDARD.encode("rustynail:secret");
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/dashboard")
                .header("authorization", format!("Basic {}", creds))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn dashboard_html_missing_auth_returns_401() {
    let state = make_state_with_auth("secret");
    let app = create_router(state, 1_048_576, 30);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/dashboard")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn dashboard_html_wrong_password_returns_401() {
    let state = make_state_with_auth("secret");
    let app = create_router(state, 1_048_576, 30);
    let creds = base64::engine::general_purpose::STANDARD.encode("rustynail:wrongpass");
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/dashboard")
                .header("authorization", format!("Basic {}", creds))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ── /dashboard/data (JSON) ────────────────────────────────────────────────────

#[tokio::test]
async fn dashboard_data_no_auth_configured_returns_200_json() {
    let app = create_router(common::make_test_state(), 1_048_576, 30);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/dashboard/data")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(json["version"].is_string());
    assert!(json["uptime_seconds"].is_number());
    assert!(json["messages_in"].is_number());
    assert!(json["messages_out"].is_number());
    assert!(json["active_users"].is_number());
    assert!(json["channels"].is_array());
    assert!(json["recent_messages"].is_array());
}

#[tokio::test]
async fn dashboard_data_auth_required_without_creds_returns_401() {
    let state = make_state_with_auth("secret");
    let app = create_router(state, 1_048_576, 30);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/dashboard/data")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
