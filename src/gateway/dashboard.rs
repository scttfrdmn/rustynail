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

use crate::gateway::http::AppState;

// ── Data structures ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct RecentMessage {
    pub timestamp: DateTime<Utc>,
    pub channel_id: String,
    pub user_id: String,
    pub content_preview: String,
    pub direction: String,
}

pub struct MessageStats {
    pub messages_in: AtomicU64,
    pub messages_out: AtomicU64,
    pub start_instant: Instant,
    pub start_time: DateTime<Utc>,
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

    pub async fn record_inbound_async(&self, message: &Message) {
        self.messages_in.fetch_add(1, Ordering::Relaxed);
        let preview = message.content.chars().take(120).collect::<String>();
        let entry = RecentMessage {
            timestamp: Utc::now(),
            channel_id: message.channel_id.clone(),
            user_id: message.user_id.clone(),
            content_preview: preview,
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
        let preview = message.content.chars().take(120).collect::<String>();
        let entry = RecentMessage {
            timestamp: Utc::now(),
            channel_id: message.channel_id.clone(),
            user_id: message.user_id.clone(),
            content_preview: preview,
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

    pub fn uptime_seconds(&self) -> u64 {
        self.start_instant.elapsed().as_secs()
    }
}

#[derive(Debug, Serialize)]
pub struct ChannelStatus {
    pub id: String,
    pub name: String,
    pub health: String,
    pub running: bool,
}

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
        start_time: state.stats.start_time,
        uptime_seconds: state.stats.uptime_seconds(),
        messages_in: state.stats.messages_in.load(Ordering::Relaxed),
        messages_out: state.stats.messages_out.load(Ordering::Relaxed),
        active_users,
        channels: channel_statuses,
        recent_messages: recent,
    };

    axum::Json(data).into_response()
}
