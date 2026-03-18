//! Zero-credential integration tests using the stub LLM + test channel harness.
//!
//! These tests require a running RustyNail instance configured with harness.yaml:
//!   CONFIG_FILE=configs/harness.yaml cargo run
//!
//! Set HARNESS_URL=http://localhost:8080 to enable these tests.
//! Tests are skipped when HARNESS_URL is not set.

fn harness_url() -> Option<String> {
    std::env::var("HARNESS_URL").ok()
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

    let client = reqwest::Client::new();

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

    // Give the async pipeline a moment to process
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Retrieve responses
    let responses_resp = client
        .get(format!("{}/test/responses", base))
        .send()
        .await
        .expect("responses request failed");
    assert_eq!(responses_resp.status(), 200);

    let responses: serde_json::Value = responses_resp.json().await.expect("parse json");
    let arr = responses.as_array().expect("expected array");
    assert!(!arr.is_empty(), "expected at least one response");

    // Stub agent in echo mode returns "echo: hello"
    let content = arr[0]["content"].as_str().unwrap_or("");
    assert!(
        content.contains("echo: hello"),
        "expected echo response, got: {}",
        content
    );
}

#[tokio::test]
async fn harness_multi() {
    let base = match harness_url() {
        Some(u) => u,
        None => return,
    };

    let client = reqwest::Client::new();

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

    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

    let responses: serde_json::Value = client
        .get(format!("{}/test/responses", base))
        .send()
        .await
        .expect("responses failed")
        .json()
        .await
        .expect("parse");

    let arr = responses.as_array().expect("array");
    assert!(arr.len() >= 2, "expected 2 responses, got {}", arr.len());
}
