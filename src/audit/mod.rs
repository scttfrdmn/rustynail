//! Structured audit logging.
//!
//! Emits NDJSON events to stderr or a file. The background writer task is non-blocking;
//! callers use `log()` which sends to an unbounded channel and returns immediately.

use crate::config::AuditConfig;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::sync::Arc;
use tokio::sync::mpsc;

// ── Audit events ─────────────────────────────────────────────────────────────

/// All structured events emitted by the audit logger.
///
/// Serialized with an `"event"` discriminant tag (snake_case) plus a `"ts"` field.
#[derive(Debug, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AuditEvent {
    /// A bearer-auth request was rejected.
    AuthRejected { path: String, reason: String },
    /// A message was blocked by the per-user rate limiter.
    RateLimitHit { user_id: String, channel_id: String },
    /// An inbound message was received.
    MessageReceived {
        user_id: String,
        channel_id: String,
        bytes: usize,
    },
    /// A tool was executed by an agent.
    ToolExecuted {
        user_id: String,
        tool_name: String,
        success: bool,
    },
    /// The config was reloaded via SIGHUP.
    ConfigReloaded { changed_fields: Vec<String> },
    /// A new per-user agent was created.
    AgentCreated { user_id: String },
    /// The LLM returned an error for a user's message.
    LlmError { user_id: String, error: String },
    /// An admin API endpoint was called.
    AdminAction {
        endpoint: String,
        /// Path parameter (e.g. user_id for memory clear).
        #[serde(skip_serializing_if = "Option::is_none")]
        param: Option<String>,
        success: bool,
    },
}

// ── Internal record wrapper ───────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct AuditRecord {
    ts: DateTime<Utc>,
    #[serde(flatten)]
    event: AuditEvent,
}

// ── AuditLogger ───────────────────────────────────────────────────────────────

/// Non-blocking structured audit logger.
///
/// Spawns a background Tokio task that writes NDJSON lines to the configured
/// destination (stderr when `path` is empty, a file otherwise).
pub struct AuditLogger {
    sender: mpsc::UnboundedSender<String>,
}

impl AuditLogger {
    /// Create a new `AuditLogger` from config and spawn its background writer.
    pub fn new(config: &AuditConfig) -> Arc<Self> {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let path = config.path.clone();

        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;

            // Open destination: file when path is set, stderr otherwise.
            let mut writer: Box<dyn tokio::io::AsyncWrite + Unpin + Send> = if path.is_empty() {
                Box::new(tokio::io::stderr())
            } else {
                match tokio::fs::OpenOptions::new()
                    .append(true)
                    .create(true)
                    .open(&path)
                    .await
                {
                    Ok(f) => Box::new(f),
                    Err(e) => {
                        eprintln!("audit: failed to open '{}': {}", path, e);
                        Box::new(tokio::io::stderr())
                    }
                }
            };

            while let Some(line) = rx.recv().await {
                let _ = writer.write_all(line.as_bytes()).await;
                let _ = writer.write_all(b"\n").await;
            }
        });

        Arc::new(Self { sender: tx })
    }

    /// Emit an audit event. Non-blocking — serializes and enqueues for background write.
    pub fn log(&self, event: AuditEvent) {
        let record = AuditRecord {
            ts: Utc::now(),
            event,
        };
        if let Ok(json) = serde_json::to_string(&record) {
            let _ = self.sender.send(json);
        }
    }
}
