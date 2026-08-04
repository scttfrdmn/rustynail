//! Zero-credential integration tests using the stub LLM + test channel harness.
//!
//! These tests require a running RustyNail instance configured with harness.yaml:
//!   CONFIG_FILE=configs/harness.yaml cargo run
//!
//! Set HARNESS_URL=http://localhost:8080 to enable these tests.
//! Tests are skipped when HARNESS_URL is not set.
//!
//! `GET /test/responses` drains a single process-wide buffer, so these tests
//! serialize on `HARNESS_LOCK` and drain any stale responses before injecting.
//! Without that they steal each other's replies when run in parallel.

use std::time::Duration;
use tokio::sync::Mutex;

/// Async-aware so the guard can be held across the `await`s below.
static HARNESS_LOCK: Mutex<()> = Mutex::const_new(());

fn harness_url() -> Option<String> {
    std::env::var("HARNESS_URL").ok()
}

/// Drain and discard anything left in the response buffer by a previous test.
async fn drain_stale(client: &reqwest::Client, base: &str) {
    let _ = client.get(format!("{}/test/responses", base)).send().await;
}

/// Poll `/test/responses` until at least `want` messages arrive, or time out.
async fn collect_responses(
    client: &reqwest::Client,
    base: &str,
    want: usize,
) -> Vec<serde_json::Value> {
    let mut collected = Vec::new();
    for _ in 0..50 {
        let body: serde_json::Value = client
            .get(format!("{}/test/responses", base))
            .send()
            .await
            .expect("responses request failed")
            .json()
            .await
            .expect("parse json");

        if let Some(arr) = body.as_array() {
            collected.extend(arr.iter().cloned());
        }
        if collected.len() >= want {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    collected
}

#[tokio::test]
async fn harness_health() {
    let base = match harness_url() {
        Some(u) => u,
        None => return, // skip when not running against a live harness
    };

    let resp = reqwest::get(format!("{}/health", base))
        .await
        .expect("health request failed");
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.expect("parse json");
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn harness_echo() {
    let base = match harness_url() {
        Some(u) => u,
        None => return,
    };
    let _guard = HARNESS_LOCK.lock().await;

    let client = reqwest::Client::new();
    drain_stale(&client, &base).await;

    // Inject a message via POST /test/send
    let send_resp = client
        .post(format!("{}/test/send", base))
        .json(&serde_json::json!({
            "user_id": "harness-user-1",
            "content": "hello"
        }))
        .send()
        .await
        .expect("send request failed");
    assert_eq!(send_resp.status(), 200);

    let arr = collect_responses(&client, &base, 1).await;
    assert!(!arr.is_empty(), "expected at least one response");

    // The stub agent echoes the whole conversation the ConversationalAgent hands
    // it (system prompt + history), so assert on the parts we control rather
    // than an exact "echo: hello".
    let content = arr[0]["content"].as_str().unwrap_or("");
    assert!(
        content.starts_with("echo:"),
        "expected stub echo response, got: {}",
        content
    );
    assert!(
        content.contains("hello"),
        "expected injected message in response, got: {}",
        content
    );
}

#[tokio::test]
async fn harness_multi() {
    let base = match harness_url() {
        Some(u) => u,
        None => return,
    };
    let _guard = HARNESS_LOCK.lock().await;

    let client = reqwest::Client::new();
    drain_stale(&client, &base).await;

    // Send two messages from different users
    for (user, msg) in [("user-a", "first"), ("user-b", "second")] {
        let resp = client
            .post(format!("{}/test/send", base))
            .json(&serde_json::json!({"user_id": user, "content": msg}))
            .send()
            .await
            .expect("send failed");
        assert_eq!(resp.status(), 200, "send failed for {}", user);
    }

    let arr = collect_responses(&client, &base, 2).await;
    assert!(arr.len() >= 2, "expected 2 responses, got {}", arr.len());
}
