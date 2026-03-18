mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use rustynail::gateway::http::create_router;
use tower::ServiceExt;

#[tokio::test]
async fn get_preferences_unknown_user_returns_null() {
    let app = create_router(common::make_test_state(), 1_048_576, 30);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/users/alice/preferences")
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
    assert!(json["preferred_channel_id"].is_null());
}

#[tokio::test]
async fn set_preference_then_get_returns_value() {
    let state = common::make_test_state();
    // POST to set pref
    let set_body = serde_json::json!({ "preferred_channel_id": "whatsapp-main" });
    let app = create_router(state.clone(), 1_048_576, 30);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/users/alice/preferences")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&set_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // GET to confirm
    let app = create_router(state, 1_048_576, 30);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/users/alice/preferences")
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
    assert_eq!(json["preferred_channel_id"], "whatsapp-main");
}

#[tokio::test]
async fn update_preference_overwrites_previous_value() {
    let state = common::make_test_state();

    // First POST
    let body1 = serde_json::json!({ "preferred_channel_id": "whatsapp-main" });
    create_router(state.clone(), 1_048_576, 30)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/users/bob/preferences")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body1).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    // Second POST (update)
    let body2 = serde_json::json!({ "preferred_channel_id": "telegram-main" });
    create_router(state.clone(), 1_048_576, 30)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/users/bob/preferences")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body2).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    // GET should return last value
    let resp = create_router(state, 1_048_576, 30)
        .oneshot(
            Request::builder()
                .uri("/users/bob/preferences")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["preferred_channel_id"], "telegram-main");
}
