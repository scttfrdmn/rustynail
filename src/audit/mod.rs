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
    /// A quarry subprocess was spawned.
    ///
    /// `env_keys` names the variables placed in the child's environment — the
    /// keys only, never the values. The child gets an explicitly constructed
    /// environment rather than an inherited one, and this is the record that lets
    /// an operator confirm no provider or channel credential leaked into it.
    QuarryRunStarted {
        run_id: String,
        user_id: String,
        channel_id: String,
        binary_path: String,
        env_keys: Vec<String>,
    },
    /// A quarry subprocess ended.
    ///
    /// `termination` is the classified outcome, not the raw exit code:
    /// `completed`, `truncated`, `no_answer`, `timed_out`, `cancelled`,
    /// `crashed`, `killed_by_signal`, or `stream_malformed`. `truncated_by` names
    /// which cap bit — and is **absent** when nothing did, because a run that
    /// finished within its caps has no such fact to report.
    QuarryRunEnded {
        run_id: String,
        user_id: String,
        termination: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        truncated_by: Option<String>,
        events: usize,
        bad_lines: usize,
        cost_micro_usd: i64,
        duration_ms: u64,
    },
    /// Policy resolved what a quarry run was allowed to spend, or refused it.
    ///
    /// Logged for grants as well as refusals: "what was this run allowed to
    /// spend" is the first question asked after an unexpected bill, and it cannot
    /// be answered from refusals alone.
    ///
    /// `scope_key` is the canonical scope string folded into every cache key. It
    /// contains verified channel identity, which is already in the other quarry
    /// events, and recording it is what lets an operator confirm two tenants never
    /// addressed the same cache entry.
    QuarryPolicyDecision {
        user_id: String,
        channel_id: String,
        /// Which precedence level supplied the entry: `sender`, `channel`, or
        /// `default`. Absent on a refusal with no matching entry.
        #[serde(skip_serializing_if = "Option::is_none")]
        matched: Option<String>,
        granted: bool,
        /// The refusal code when `granted` is false.
        #[serde(skip_serializing_if = "Option::is_none")]
        refusal: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        requested_spend_micro_usd: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        granted_spend_micro_usd: Option<i64>,
        /// `denomination:kind` pairs, e.g. `spend:reduced`.
        adjustments: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        scope_key: Option<String>,
    },
    /// A sender approved, cancelled, or let expire a quarry plan gate.
    ///
    /// The record of who agreed to spend. Cancellations and timeouts are logged
    /// too, and for the same reason: "nobody approved this" is the fact that
    /// explains an absent run, and without it a cancelled run and a run that was
    /// never requested look identical.
    ///
    /// `decision` is one of `approved`, `cancelled`, `expired`, `superseded`.
    ///
    /// The caps are recorded alongside it because an approval that does not say
    /// *what* was approved cannot answer the question it exists to answer. They are
    /// the granted caps as shown in the plan message, not what the sender asked for
    /// — [`AuditEvent::QuarryPolicyDecision`] already holds that.
    QuarryPlanDecision {
        request_id: String,
        user_id: String,
        channel_id: String,
        decision: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        spend_micro_usd: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        latency_seconds: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        due: Option<String>,
    },
    /// A quarry run could not be started, or failed in supervision.
    QuarryRunFailed {
        #[serde(skip_serializing_if = "Option::is_none")]
        run_id: Option<String>,
        user_id: String,
        reason: String,
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
