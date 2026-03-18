mod common;

use rustynail::agents::AgentManager;
use rustynail::config::{AgentsConfig, RateLimitConfig};
use rustynail::gateway::chunker::MessageChunker;
use rustynail::gateway::deduplicator::MessageDeduplicator;
use rustynail::gateway::dashboard::MessageStats;
use rustynail::gateway::handle_message_for_test_full;
use rustynail::gateway::rate_limiter::RateLimiter;
use rustynail::gateway::user_prefs::UserPreferences;
use rustynail::memory::{InMemoryStore, MemoryStore};
use rustynail::types::Message;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn stub_agent_manager() -> Arc<AgentManager> {
    Arc::new(AgentManager::new(AgentsConfig {
        llm_provider: "stub".to_string(),
        ..Default::default()
    }))
}

fn memory() -> Arc<dyn MemoryStore> {
    Arc::new(InMemoryStore::new(20))
}

fn stats() -> Arc<MessageStats> {
    MessageStats::new()
}

fn user_prefs() -> Arc<UserPreferences> {
    Arc::new(UserPreferences::new())
}

fn msg(user_id: &str, channel_id: &str, content: &str) -> Message {
    Message::new(
        channel_id.to_string(),
        user_id.to_string(),
        user_id.to_string(),
        content.to_string(),
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_pipeline_dedup_drops_duplicate() {
    let recording = common::RecordingChannel::new("chan1");
    let sent = recording.sent_handle();
    let channels: Arc<RwLock<Vec<Box<dyn rustynail::channels::Channel>>>> =
        Arc::new(RwLock::new(vec![Box::new(recording)]));

    let dedup = Arc::new(Mutex::new(MessageDeduplicator::new(256)));
    let agent_mgr = stub_agent_manager();
    let mem = memory();
    let s = stats();
    let up = user_prefs();

    // First message — should go through
    handle_message_for_test_full(
        &mem, &agent_mgr, &channels, &up, &s,
        msg("alice", "chan1", "hello"),
        None, None, None, None, false,
    )
    .await
    .unwrap();

    // Exact same message — should be dropped by deduplicator
    handle_message_for_test_full(
        &mem, &agent_mgr, &channels, &up, &s,
        msg("alice", "chan1", "hello"),
        None, None, Some(dedup.clone()), None, false,
    )
    .await
    .unwrap();

    // Make the first call re-use the dedup so both are checked
    // Reset and redo properly: send first message WITH dedup enabled
    // so the duplicate is in the ring buffer
    let dedup2 = Arc::new(Mutex::new(MessageDeduplicator::new(256)));

    handle_message_for_test_full(
        &mem, &agent_mgr, &channels, &up, &s,
        msg("bob", "chan1", "unique"),
        None, None, Some(dedup2.clone()), None, false,
    )
    .await
    .unwrap();

    let before = sent.lock().await.len();

    handle_message_for_test_full(
        &mem, &agent_mgr, &channels, &up, &s,
        msg("bob", "chan1", "unique"),
        None, None, Some(dedup2.clone()), None, false,
    )
    .await
    .unwrap();

    let after = sent.lock().await.len();
    // Second call should have been dropped — no new message sent
    assert_eq!(before, after, "duplicate should be dropped");
}

#[tokio::test]
async fn test_pipeline_multi_user_isolation() {
    let chan_a = common::RecordingChannel::new("chan-a");
    let chan_b = common::RecordingChannel::new("chan-b");
    let sent_a = chan_a.sent_handle();
    let sent_b = chan_b.sent_handle();

    let channels: Arc<RwLock<Vec<Box<dyn rustynail::channels::Channel>>>> =
        Arc::new(RwLock::new(vec![
            Box::new(chan_a),
            Box::new(chan_b),
        ]));

    let agent_mgr = stub_agent_manager();
    let mem = memory();
    let s = stats();
    let up = user_prefs();

    // User A sends a message to chan-a
    handle_message_for_test_full(
        &mem, &agent_mgr, &channels, &up, &s,
        msg("userA", "chan-a", "ping from A"),
        None, None, None, None, false,
    )
    .await
    .unwrap();

    // User B sends a message to chan-b
    handle_message_for_test_full(
        &mem, &agent_mgr, &channels, &up, &s,
        msg("userB", "chan-b", "ping from B"),
        None, None, None, None, false,
    )
    .await
    .unwrap();

    // chan-a received exactly 1 message (A's response)
    assert_eq!(sent_a.lock().await.len(), 1);
    // chan-b received exactly 1 message (B's response)
    assert_eq!(sent_b.lock().await.len(), 1);
    // Responses went to the correct channels
    assert_eq!(sent_a.lock().await[0].channel_id, "chan-a");
    assert_eq!(sent_b.lock().await[0].channel_id, "chan-b");
}

#[tokio::test]
async fn test_pipeline_chunking_splits_long_response() {
    // StubAgent echoes the full conversation history. We craft an input large
    // enough so the echo response exceeds the Discord 2000-char limit.
    // The stub formats: "echo: system: <prompt>\nuser: <long_input>"
    // The system prompt is ~100 chars; add enough 'x's to push past 2000.
    let long_input: String = "x".repeat(2100);

    let recording = common::RecordingChannel::new("discord-main");
    let sent = recording.sent_handle();
    let channels: Arc<RwLock<Vec<Box<dyn rustynail::channels::Channel>>>> =
        Arc::new(RwLock::new(vec![Box::new(recording)]));

    // Chunker with built-in Discord limit (2000 chars)
    let chunker = Arc::new(MessageChunker::new(HashMap::new()));

    let agent_mgr = stub_agent_manager();
    let mem = memory();
    let s = stats();
    let up = user_prefs();

    handle_message_for_test_full(
        &mem, &agent_mgr, &channels, &up, &s,
        msg("user1", "discord-main", &long_input),
        None, None, None, Some(chunker), false,
    )
    .await
    .unwrap();

    let messages = sent.lock().await;
    // Should have been split into at least 2 chunks
    assert!(
        messages.len() >= 2,
        "expected ≥2 chunks, got {}; total response len={}",
        messages.len(),
        messages.iter().map(|m| m.content.len()).sum::<usize>(),
    );
    // Every chunk must be ≤ 2000 chars
    for (i, chunk) in messages.iter().enumerate() {
        assert!(
            chunk.content.len() <= 2000,
            "chunk {} exceeds 2000 chars: {} chars",
            i,
            chunk.content.len()
        );
    }
    // Reassembled content should contain the long input
    let reassembled: String = messages.iter().map(|m| m.content.as_str()).collect();
    assert!(
        reassembled.contains(&long_input),
        "reassembled response should contain the long input"
    );
}

#[tokio::test]
async fn test_pipeline_formatting_slack_applied() {
    let recording = common::RecordingChannel::new("slack-main");
    let sent = recording.sent_handle();
    let channels: Arc<RwLock<Vec<Box<dyn rustynail::channels::Channel>>>> =
        Arc::new(RwLock::new(vec![Box::new(recording)]));

    // Stub agent will echo "echo: **hello**" — with Slack formatting that
    // becomes "echo: *hello*"
    let agent_mgr = stub_agent_manager();
    let mem = memory();
    let s = stats();
    let up = user_prefs();

    handle_message_for_test_full(
        &mem, &agent_mgr, &channels, &up, &s,
        msg("user1", "slack-main", "**hello**"),
        None, None, None, None, true, // formatting_enabled = true
    )
    .await
    .unwrap();

    let messages = sent.lock().await;
    assert_eq!(messages.len(), 1);
    // Slack bold is *text* not **text**
    assert!(
        messages[0].content.contains("*hello*"),
        "expected Slack-formatted bold, got: {}",
        messages[0].content
    );
    assert!(
        !messages[0].content.contains("**hello**"),
        "raw markdown should have been converted"
    );
}
