mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use rustynail::gateway::http::create_router;
use tower::ServiceExt;

#[tokio::test]
async fn health_returns_200() {
    let app = create_router(common::make_test_state());
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn health_body_contains_version() {
    let app = create_router(common::make_test_state());
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["status"], "ok");
    assert!(json["version"].is_string());
}

#[tokio::test]
async fn status_returns_200_with_channels_and_users() {
    let app = create_router(common::make_test_state());
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/status")
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
    assert!(json["channels"].is_array());
    assert!(json["active_users"].is_number());
}

#[tokio::test]
async fn metrics_returns_200() {
    let app = create_router(common::make_test_state());
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
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
    assert!(json["active_users"].is_number());
    assert!(json["channels_count"].is_number());
    assert!(json["healthy_channels"].is_number());
}

#[tokio::test]
async fn ready_returns_503_when_no_channels_running() {
    let app = create_router(common::make_test_state());
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/ready")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn live_returns_200() {
    let app = create_router(common::make_test_state());
    let resp = app
        .oneshot(Request::builder().uri("/live").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["status"], "alive");
}
