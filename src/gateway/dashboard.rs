use crate::types::Message;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse},
};
use chrono::{DateTime, Utc};
use prometheus::{Counter, Gauge, Histogram, HistogramOpts, Opts, Registry};
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{broadcast, RwLock};
use tracing::warn;

use crate::gateway::http::{AppState, ChannelStatus};

// ── Dashboard events ──────────────────────────────────────────────────────────

/// Events streamed over the `/dashboard/ws` WebSocket.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DashboardEvent {
    /// Emitted every 5 seconds with current system counters.
    StatsUpdate {
        messages_in: u64,
        messages_out: u64,
        active_users: usize,
        healthy_channels: usize,
        uptime_seconds: u64,
    },
    /// Emitted on each inbound or outbound message.
    MessageEvent {
        channel: String,
        user: String,
        preview: String,
        direction: String,
    },
}

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
/// Atomic counters provide lock-free reads. The recent-message ring buffer is
/// guarded by a `RwLock` and capped at 50 entries. Prometheus metrics are
/// registered in an internal `Registry` and updated on every increment.
pub struct MessageStats {
    messages_in: AtomicU64,
    messages_out: AtomicU64,
    /// Total LLM input tokens consumed since startup.
    tokens_in: AtomicU64,
    /// Total LLM output tokens generated since startup.
    tokens_out: AtomicU64,
    start_instant: Instant,
    start_time: DateTime<Utc>,
    recent: RwLock<VecDeque<RecentMessage>>,
    // Prometheus registry and metric handles
    prom_registry: Registry,
    prom_messages_in: Counter,
    prom_messages_out: Counter,
    prom_tokens_in: Counter,
    prom_tokens_out: Counter,
    prom_active_users: Gauge,
    prom_healthy_channels: Gauge,
    prom_message_duration: Histogram,
    // Dashboard WebSocket broadcast
    event_tx: broadcast::Sender<DashboardEvent>,
}

impl MessageStats {
    pub fn new() -> Arc<Self> {
        let registry = Registry::new();

        let prom_messages_in = Counter::with_opts(Opts::new(
            "rustynail_messages_in_total",
            "Total inbound messages since startup",
        ))
        .expect("counter creation failed");

        let prom_messages_out = Counter::with_opts(Opts::new(
            "rustynail_messages_out_total",
            "Total outbound messages since startup",
        ))
        .expect("counter creation failed");

        let prom_tokens_in = Counter::with_opts(Opts::new(
            "rustynail_tokens_in_total",
            "Total LLM input tokens consumed since startup",
        ))
        .expect("counter creation failed");

        let prom_tokens_out = Counter::with_opts(Opts::new(
            "rustynail_tokens_out_total",
            "Total LLM output tokens generated since startup",
        ))
        .expect("counter creation failed");

        let prom_active_users = Gauge::with_opts(Opts::new(
            "rustynail_active_users",
            "Current active user sessions",
        ))
        .expect("gauge creation failed");

        let prom_healthy_channels = Gauge::with_opts(Opts::new(
            "rustynail_healthy_channels",
            "Number of healthy channel adapters",
        ))
        .expect("gauge creation failed");

        let prom_message_duration = Histogram::with_opts(
            HistogramOpts::new(
                "rustynail_message_duration_seconds",
                "Message processing latency in seconds",
            )
            .buckets(vec![0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0]),
        )
        .expect("histogram creation failed");

        registry
            .register(Box::new(prom_messages_in.clone()))
            .expect("register failed");
        registry
            .register(Box::new(prom_messages_out.clone()))
            .expect("register failed");
        registry
            .register(Box::new(prom_tokens_in.clone()))
            .expect("register failed");
        registry
            .register(Box::new(prom_tokens_out.clone()))
            .expect("register failed");
        registry
            .register(Box::new(prom_active_users.clone()))
            .expect("register failed");
        registry
            .register(Box::new(prom_healthy_channels.clone()))
            .expect("register failed");
        registry
            .register(Box::new(prom_message_duration.clone()))
            .expect("register failed");

        // Channel capacity of 256; lagged receivers will skip events gracefully.
        let (event_tx, _) = broadcast::channel(256);

        Arc::new(Self {
            messages_in: AtomicU64::new(0),
            messages_out: AtomicU64::new(0),
            tokens_in: AtomicU64::new(0),
            tokens_out: AtomicU64::new(0),
            start_instant: Instant::now(),
            start_time: Utc::now(),
            recent: RwLock::new(VecDeque::new()),
            prom_registry: registry,
            prom_messages_in,
            prom_messages_out,
            prom_tokens_in,
            prom_tokens_out,
            prom_active_users,
            prom_healthy_channels,
            prom_message_duration,
            event_tx,
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

    /// Total LLM input tokens consumed since startup.
    pub fn tokens_in(&self) -> u64 {
        self.tokens_in.load(Ordering::Relaxed)
    }

    /// Total LLM output tokens generated since startup.
    pub fn tokens_out(&self) -> u64 {
        self.tokens_out.load(Ordering::Relaxed)
    }

    /// Record token usage from a single LLM completion.
    pub fn record_tokens(&self, input: u64, output: u64) {
        self.tokens_in.fetch_add(input, Ordering::Relaxed);
        self.tokens_out.fetch_add(output, Ordering::Relaxed);
        self.prom_tokens_in.inc_by(input as f64);
        self.prom_tokens_out.inc_by(output as f64);
    }

    /// Wall-clock time the gateway started.
    pub fn start_time(&self) -> DateTime<Utc> {
        self.start_time
    }

    /// Seconds elapsed since the gateway started (monotonic).
    pub fn uptime_seconds(&self) -> u64 {
        self.start_instant.elapsed().as_secs()
    }

    /// Subscribe to dashboard events for the WebSocket handler.
    pub fn subscribe(&self) -> broadcast::Receiver<DashboardEvent> {
        self.event_tx.subscribe()
    }

    /// Observe a message processing duration (feeds the histogram).
    pub fn observe_message_duration(&self, duration_secs: f64) {
        self.prom_message_duration.observe(duration_secs);
    }

    /// Update the active-users gauge (called from the metrics handler).
    pub fn set_active_users(&self, n: usize) {
        self.prom_active_users.set(n as f64);
    }

    /// Update the healthy-channels gauge (called from the metrics handler).
    pub fn set_healthy_channels(&self, n: usize) {
        self.prom_healthy_channels.set(n as f64);
    }

    /// Gather all metric families for Prometheus encoding.
    pub fn prometheus_gather(&self) -> Vec<prometheus::proto::MetricFamily> {
        self.prom_registry.gather()
    }

    pub async fn record_inbound_async(&self, message: &Message) {
        self.messages_in.fetch_add(1, Ordering::Relaxed);
        self.prom_messages_in.inc();

        let entry = RecentMessage {
            timestamp: Utc::now(),
            channel_id: message.channel_id.clone(),
            user_id: message.user_id.clone(),
            content_preview: message.content.chars().take(120).collect(),
            direction: "inbound".to_string(),
        };
        let _ = self.event_tx.send(DashboardEvent::MessageEvent {
            channel: entry.channel_id.clone(),
            user: entry.user_id.clone(),
            preview: entry.content_preview.clone(),
            direction: "inbound".to_string(),
        });

        let mut recent = self.recent.write().await;
        recent.push_front(entry);
        if recent.len() > 50 {
            recent.pop_back();
        }
    }

    pub async fn record_outbound_async(&self, message: &Message) {
        self.messages_out.fetch_add(1, Ordering::Relaxed);
        self.prom_messages_out.inc();

        let entry = RecentMessage {
            timestamp: Utc::now(),
            channel_id: message.channel_id.clone(),
            user_id: message.user_id.clone(),
            content_preview: message.content.chars().take(120).collect(),
            direction: "outbound".to_string(),
        };
        let _ = self.event_tx.send(DashboardEvent::MessageEvent {
            channel: entry.channel_id.clone(),
            user: entry.user_id.clone(),
            preview: entry.content_preview.clone(),
            direction: "outbound".to_string(),
        });

        let mut recent = self.recent.write().await;
        recent.push_front(entry);
        if recent.len() > 50 {
            recent.pop_back();
        }
    }

    pub async fn recent_messages(&self) -> Vec<RecentMessage> {
        self.recent.read().await.iter().cloned().collect()
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
    pub tokens_in: u64,
    pub tokens_out: u64,
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
        tokens_in: state.stats.tokens_in(),
        tokens_out: state.stats.tokens_out(),
        active_users,
        channels: channel_statuses,
        recent_messages: recent,
    };

    axum::Json(data).into_response()
}
