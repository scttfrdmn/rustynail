mod common;

use mockito::Server;
use rustynail::agents::AgentManager;
use rustynail::config::AgentsConfig;
use rustynail::gateway::dashboard::MessageStats;
use rustynail::gateway::user_prefs::UserPreferences;
use rustynail::types::Message;
use rustynail::Gateway;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Minimal valid Anthropic /v1/messages response understood by agenkit.
fn anthropic_mock_response() -> serde_json::Value {
    serde_json::json!({
        "id": "msg_test",
        "type": "message",
        "role": "assistant",
        "content": [{"type": "text", "text": "pong"}],
        "model": "claude-3-5-sonnet-20241022",
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 5, "output_tokens": 1}
    })
}

#[tokio::test]
async fn handle_message_routes_to_recording_channel() {
    // Start an in-process mockito server for the Anthropic API.
    let mut server = Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/messages")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(anthropic_mock_response().to_string())
        .create_async()
        .await;

    // Build AgentManager pointing at the mock server.
    let agents_cfg = AgentsConfig {
        api_key: "test_key_unused".to_string(),
        api_base: Some(server.url()),
        ..Default::default()
    };
    let agent_manager = Arc::new(AgentManager::new(agents_cfg));

    // Build a RecordingChannel and register it.
    let recording = common::RecordingChannel::new("recording-main");
    let sent_handle = recording.sent_handle();
    let channels: Arc<RwLock<Vec<Box<dyn rustynail::channels::Channel>>>> =
        Arc::new(RwLock::new(vec![Box::new(recording)]));

    let memory: Arc<dyn rustynail::memory::MemoryStore> =
        Arc::new(rustynail::memory::InMemoryStore::new(5));
    let user_prefs = Arc::new(UserPreferences::new());
    let stats = MessageStats::new();

    // Send an inbound message that targets our recording channel.
    let inbound = Message::new(
        "recording-main".to_string(),
        "user123".to_string(),
        "TestUser".to_string(),
        "ping".to_string(),
    );

    rustynail::gateway::handle_message_for_test(
        &memory,
        &agent_manager,
        &channels,
        &user_prefs,
        &stats,
        inbound,
    )
    .await
    .expect("handle_message should succeed");

    // Assert mock was called.
    mock.assert_async().await;

    // Assert RecordingChannel received the AI response.
    let sent = sent_handle.lock().await;
    assert_eq!(sent.len(), 1, "expected exactly one outbound message");
    assert_eq!(sent[0].content, "pong", "expected AI response text 'pong'");
    assert_eq!(sent[0].channel_id, "recording-main");
}
