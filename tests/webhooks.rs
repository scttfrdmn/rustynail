mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use rustynail::gateway::http::create_router;
use tower::ServiceExt;

// ── WhatsApp ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn whatsapp_verify_valid_token_returns_200() {
    let app = create_router(common::make_test_state());
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/webhooks/whatsapp?hub.mode=subscribe&hub.verify_token=test-verify-token&hub.challenge=abc123")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(bytes, "abc123");
}

#[tokio::test]
async fn whatsapp_verify_wrong_token_returns_403() {
    let app = create_router(common::make_test_state());
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/webhooks/whatsapp?hub.mode=subscribe&hub.verify_token=wrong&hub.challenge=abc123")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn whatsapp_receive_without_sender_returns_503() {
    // make_test_state() has no whatsapp_tx
    let app = create_router(common::make_test_state());
    let body = serde_json::json!({
        "object": "whatsapp_business_account",
        "entry": []
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhooks/whatsapp")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn whatsapp_receive_with_sender_returns_200() {
    let (state, _wa_rx, _tg_rx, _sl_rx) = common::make_test_state_with_webhooks();
    let app = create_router(state);
    let body = serde_json::json!({
        "object": "whatsapp_business_account",
        "entry": []
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhooks/whatsapp")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ── Telegram ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn telegram_receive_without_sender_returns_503() {
    let app = create_router(common::make_test_state());
    let update = serde_json::json!({
        "update_id": 1,
        "message": {
            "message_id": 1,
            "date": 1700000000,
            "chat": { "id": 42, "type": "private" },
            "from": { "id": 99, "is_bot": false, "first_name": "Test" },
            "text": "hello"
        }
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhooks/telegram")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&update).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn telegram_receive_with_sender_returns_200() {
    let (state, _wa_rx, _tg_rx, _sl_rx) = common::make_test_state_with_webhooks();
    let app = create_router(state);
    let update = serde_json::json!({
        "update_id": 1,
        "message": {
            "message_id": 1,
            "date": 1700000000,
            "chat": { "id": 42, "type": "private" },
            "from": { "id": 99, "is_bot": false, "first_name": "Test" },
            "text": "hello"
        }
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhooks/telegram")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&update).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ── Slack ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn slack_url_verification_returns_challenge() {
    let app = create_router(common::make_test_state());
    let body = serde_json::json!({
        "type": "url_verification",
        "challenge": "my_challenge_string"
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhooks/slack")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["challenge"], "my_challenge_string");
}

#[tokio::test]
async fn slack_event_without_sender_returns_503() {
    let app = create_router(common::make_test_state());
    let body = serde_json::json!({
        "type": "event_callback",
        "team_id": "T123",
        "event_id": "Ev123",
        "event": {
            "type": "message",
            "channel": "C123",
            "user": "U123",
            "text": "hello",
            "ts": "1700000000.000000"
        }
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhooks/slack")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn slack_event_with_sender_returns_200() {
    let (state, _wa_rx, _tg_rx, _sl_rx) = common::make_test_state_with_webhooks();
    let app = create_router(state);
    let body = serde_json::json!({
        "type": "event_callback",
        "team_id": "T123",
        "event_id": "Ev123",
        "event": {
            "type": "message",
            "channel": "C123",
            "user": "U123",
            "text": "hello",
            "ts": "1700000000.000000"
        }
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhooks/slack")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}
