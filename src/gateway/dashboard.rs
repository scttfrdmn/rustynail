use crate::types::Message;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse},
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tracing::warn;

use crate::gateway::http::{AppState, ChannelStatus};

// ── Data structures ───────────────────────────────────────────────────────────

/// A single message entry stored in the recent-messages ring buffer.
#[derive(Debug, Clone, Serialize)]
pub struct RecentMessage {
    pub timestamp: DateTime<Utc>,
    pub channel_id: String,
    pub user_id: String,
    pub content_preview: String,
    pub direction: String,
}

/// Shared message statistics threaded through the gateway.
///
/// Counters are atomics (lock-free reads). The recent-message ring buffer is
/// guarded by a `RwLock` and capped at 50 entries.
pub struct MessageStats {
    messages_in: AtomicU64,
    messages_out: AtomicU64,
    start_instant: Instant,
    start_time: DateTime<Utc>,
    recent: RwLock<VecDeque<RecentMessage>>,
}

impl MessageStats {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            messages_in: AtomicU64::new(0),
            messages_out: AtomicU64::new(0),
            start_instant: Instant::now(),
            start_time: Utc::now(),
            recent: RwLock::new(VecDeque::new()),
        })
    }

    /// Total inbound messages since startup.
    pub fn messages_in(&self) -> u64 {
        self.messages_in.load(Ordering::Relaxed)
    }

    /// Total outbound messages since startup.
    pub fn messages_out(&self) -> u64 {
        self.messages_out.load(Ordering::Relaxed)
    }

    /// Wall-clock time the gateway started.
    pub fn start_time(&self) -> DateTime<Utc> {
        self.start_time
    }

    pub async fn record_inbound_async(&self, message: &Message) {
        self.messages_in.fetch_add(1, Ordering::Relaxed);
        let entry = RecentMessage {
            timestamp: Utc::now(),
            channel_id: message.channel_id.clone(),
            user_id: message.user_id.clone(),
            content_preview: message.content.chars().take(120).collect(),
            direction: "inbound".to_string(),
        };
        let mut recent = self.recent.write().await;
        recent.push_front(entry);
        if recent.len() > 50 {
            recent.pop_back();
        }
    }

    pub async fn record_outbound_async(&self, message: &Message) {
        self.messages_out.fetch_add(1, Ordering::Relaxed);
        let entry = RecentMessage {
            timestamp: Utc::now(),
            channel_id: message.channel_id.clone(),
            user_id: message.user_id.clone(),
            content_preview: message.content.chars().take(120).collect(),
            direction: "outbound".to_string(),
        };
        let mut recent = self.recent.write().await;
        recent.push_front(entry);
        if recent.len() > 50 {
            recent.pop_back();
        }
    }

    pub async fn recent_messages(&self) -> Vec<RecentMessage> {
        self.recent.read().await.iter().cloned().collect()
    }

    /// Seconds elapsed since the gateway started (monotonic).
    pub fn uptime_seconds(&self) -> u64 {
        self.start_instant.elapsed().as_secs()
    }
}

/// JSON payload returned by `GET /dashboard/data`.
#[derive(Debug, Serialize)]
pub struct DashboardData {
    pub version: &'static str,
    pub start_time: DateTime<Utc>,
    pub uptime_seconds: u64,
    pub messages_in: u64,
    pub messages_out: u64,
    pub active_users: usize,
    pub channels: Vec<ChannelStatus>,
    pub recent_messages: Vec<RecentMessage>,
}

// ── Auth helper ───────────────────────────────────────────────────────────────

fn verify_dashboard_auth(headers: &HeaderMap, expected: &Option<String>) -> bool {
    let expected_value = match expected {
        Some(v) => v,
        None => return true, // no auth configured
    };

    let provided = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    provided == expected_value
}

// ── Handlers ──────────────────────────────────────────────────────────────────

pub async fn dashboard_html_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !verify_dashboard_auth(&headers, &state.dashboard_expected_auth) {
        warn!("Dashboard: unauthorized access attempt");
        return (
            StatusCode::UNAUTHORIZED,
            [
                ("WWW-Authenticate", "Basic realm=\"RustyNail Dashboard\""),
                ("Content-Type", "text/plain"),
            ],
            "Unauthorized",
        )
            .into_response();
    }

    Html(include_str!("dashboard.html")).into_response()
}

pub async fn dashboard_data_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !verify_dashboard_auth(&headers, &state.dashboard_expected_auth) {
        warn!("Dashboard data: unauthorized access attempt");
        return (
            StatusCode::UNAUTHORIZED,
            [("WWW-Authenticate", "Basic realm=\"RustyNail Dashboard\"")],
            axum::Json(serde_json::json!({"error": "unauthorized"})),
        )
            .into_response();
    }

    let channels = state.channels.read().await;
    let active_users = state.agent_manager.active_users().await;

    let channel_statuses: Vec<ChannelStatus> = channels
        .iter()
        .map(|ch| ChannelStatus {
            id: ch.id().to_string(),
            name: ch.name().to_string(),
            health: format!("{:?}", ch.health()),
            running: ch.is_running(),
        })
        .collect();

    let recent = state.stats.recent_messages().await;

    let data = DashboardData {
        version: env!("CARGO_PKG_VERSION"),
        start_time: state.stats.start_time(),
        uptime_seconds: state.stats.uptime_seconds(),
        messages_in: state.stats.messages_in(),
        messages_out: state.stats.messages_out(),
        active_users,
        channels: channel_statuses,
        recent_messages: recent,
    };

    axum::Json(data).into_response()
}
