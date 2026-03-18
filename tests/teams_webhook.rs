mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use rustynail::gateway::http::create_router;
use tokio::sync::mpsc;
use tower::ServiceExt;

// ── HMAC helper ───────────────────────────────────────────────────────────────

fn teams_hmac_header(secret: &str, body: &[u8]) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    format!("HMAC {}", hex::encode(mac.finalize().into_bytes()))
}

/// Minimal valid Teams activity JSON body.
fn teams_body() -> &'static [u8] {
    b"{\"type\":\"message\",\"from\":{\"id\":\"user1\",\"name\":\"Alice\"},\"conversation\":{\"id\":\"conv1\"},\"text\":\"hello\"}"
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_teams_no_hmac_secret_accepts_any_request() {
    let (teams_tx, _rx) = mpsc::unbounded_channel();
    let mut state = common::make_test_state();
    state.teams_tx = Some(teams_tx);
    state.teams_hmac_secret = String::new(); // disabled

    let app = create_router(state, 1_048_576, 30);
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/channels/teams/messages")
                .header("content-type", "application/json")
                .body(Body::from(teams_body()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_teams_valid_hmac_accepted() {
    let secret = "s3cr3t";
    let body = teams_body();
    let auth_header = teams_hmac_header(secret, body);

    let (teams_tx, _rx) = mpsc::unbounded_channel();
    let mut state = common::make_test_state();
    state.teams_tx = Some(teams_tx);
    state.teams_hmac_secret = secret.to_string();

    let app = create_router(state, 1_048_576, 30);
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/channels/teams/messages")
                .header("content-type", "application/json")
                .header("authorization", auth_header)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_teams_invalid_hmac_rejected() {
    let (teams_tx, _rx) = mpsc::unbounded_channel();
    let mut state = common::make_test_state();
    state.teams_tx = Some(teams_tx);
    state.teams_hmac_secret = "correct-secret".to_string();

    let app = create_router(state, 1_048_576, 30);
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/channels/teams/messages")
                .header("content-type", "application/json")
                .header("authorization", "HMAC deadbeefdeadbeef")
                .body(Body::from(teams_body()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_teams_missing_auth_header_rejected() {
    let (teams_tx, _rx) = mpsc::unbounded_channel();
    let mut state = common::make_test_state();
    state.teams_tx = Some(teams_tx);
    state.teams_hmac_secret = "some-secret".to_string();

    let app = create_router(state, 1_048_576, 30);
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/channels/teams/messages")
                .header("content-type", "application/json")
                // No Authorization header
                .body(Body::from(teams_body()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_teams_malformed_json_rejected() {
    let (teams_tx, _rx) = mpsc::unbounded_channel();
    let mut state = common::make_test_state();
    state.teams_tx = Some(teams_tx);
    state.teams_hmac_secret = String::new(); // no HMAC check

    let app = create_router(state, 1_048_576, 30);
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/channels/teams/messages")
                .header("content-type", "application/json")
                .body(Body::from("{bad json"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}
