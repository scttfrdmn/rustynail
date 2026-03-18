use crate::agents::AgentManager;
use crate::channels::Channel;
use crate::channels::whatsapp::{parse_webhook, WebhookBody};
use crate::gateway::user_prefs::UserPreferences;
use crate::types::Message;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn};

/// HTTP server state shared across handlers
#[derive(Clone)]
pub struct AppState {
    pub channels: Arc<RwLock<Vec<Box<dyn Channel>>>>,
    pub agent_manager: Arc<AgentManager>,
    pub whatsapp_tx: Option<mpsc::UnboundedSender<Message>>,
    pub whatsapp_verify_token: String,
    pub user_prefs: Arc<UserPreferences>,
}

// ── Response types ──────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StatusResponse {
    pub status: String,
    pub version: String,
    pub channels: Vec<ChannelStatus>,
    pub active_users: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ChannelStatus {
    pub id: String,
    pub name: String,
    pub health: String,
    pub running: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MetricsResponse {
    pub active_users: usize,
    pub channels_count: usize,
    pub healthy_channels: usize,
}

// ── Health / status handlers ─────────────────────────────────────────────────

async fn health_handler() -> impl IntoResponse {
    Json(HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

async fn status_handler(State(state): State<AppState>) -> impl IntoResponse {
    let channels = state.channels.read().await;
    let active_users = state.agent_manager.active_users().await;

    let channel_statuses: Vec<ChannelStatus> = channels
        .iter()
        .map(|channel| ChannelStatus {
            id: channel.id().to_string(),
            name: channel.name().to_string(),
            health: format!("{:?}", channel.health()),
            running: channel.is_running(),
        })
        .collect();

    Json(StatusResponse {
        status: "running".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        channels: channel_statuses,
        active_users,
    })
}

async fn metrics_handler(State(state): State<AppState>) -> impl IntoResponse {
    let channels = state.channels.read().await;
    let active_users = state.agent_manager.active_users().await;
    let healthy_channels = channels.iter().filter(|c| c.health().is_healthy()).count();

    Json(MetricsResponse {
        active_users,
        channels_count: channels.len(),
        healthy_channels,
    })
}

async fn readiness_handler(State(state): State<AppState>) -> impl IntoResponse {
    let channels = state.channels.read().await;
    let has_operational = channels.iter().any(|c| c.health().is_operational());

    if has_operational {
        (
            StatusCode::OK,
            Json(HealthResponse {
                status: "ready".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            }),
        )
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(HealthResponse {
                status: "not ready".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            }),
        )
    }
}

async fn liveness_handler() -> impl IntoResponse {
    Json(HealthResponse {
        status: "alive".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

// ── WhatsApp webhook handlers ─────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct WhatsAppVerifyParams {
    #[serde(rename = "hub.mode")]
    pub mode: Option<String>,
    #[serde(rename = "hub.verify_token")]
    pub verify_token: Option<String>,
    #[serde(rename = "hub.challenge")]
    pub challenge: Option<String>,
}

async fn whatsapp_webhook_verify(
    State(state): State<AppState>,
    Query(params): Query<WhatsAppVerifyParams>,
) -> impl IntoResponse {
    let mode = params.mode.as_deref().unwrap_or("");
    let token = params.verify_token.as_deref().unwrap_or("");
    let challenge = params.challenge.as_deref().unwrap_or("");

    if mode == "subscribe" && token == state.whatsapp_verify_token {
        info!("WhatsApp webhook verified");
        (StatusCode::OK, challenge.to_string()).into_response()
    } else {
        warn!(
            "WhatsApp webhook verification failed: mode={}, token_match={}",
            mode,
            token == state.whatsapp_verify_token
        );
        StatusCode::FORBIDDEN.into_response()
    }
}

async fn whatsapp_webhook_receive(
    State(state): State<AppState>,
    Json(body): Json<WebhookBody>,
) -> impl IntoResponse {
    let tx = match &state.whatsapp_tx {
        Some(tx) => tx,
        None => {
            warn!("Received WhatsApp webhook but no sender configured");
            return StatusCode::SERVICE_UNAVAILABLE;
        }
    };

    let messages = parse_webhook("whatsapp-main", &body);
    for msg in messages {
        if let Err(e) = tx.send(msg) {
            warn!("Failed to enqueue WhatsApp message: {}", e);
        }
    }

    StatusCode::OK
}

// ── User preferences handlers ─────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct UserPreferenceResponse {
    pub preferred_channel_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SetUserPreferenceRequest {
    pub preferred_channel_id: String,
}

async fn get_user_preferences(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
) -> impl IntoResponse {
    let preferred_channel_id = state.user_prefs.get(&user_id).await;
    Json(UserPreferenceResponse { preferred_channel_id })
}

async fn set_user_preferences(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    Json(body): Json<SetUserPreferenceRequest>,
) -> impl IntoResponse {
    state
        .user_prefs
        .set(&user_id, &body.preferred_channel_id)
        .await;
    StatusCode::OK
}

// ── Router ────────────────────────────────────────────────────────────────────

pub fn create_router(state: AppState) -> Router {
    Router::new()
        // Health / observability
        .route("/health", get(health_handler))
        .route("/status", get(status_handler))
        .route("/metrics", get(metrics_handler))
        .route("/ready", get(readiness_handler))
        .route("/live", get(liveness_handler))
        // WhatsApp webhooks
        .route("/webhooks/whatsapp", get(whatsapp_webhook_verify))
        .route("/webhooks/whatsapp", post(whatsapp_webhook_receive))
        // User preferences
        .route("/users/:user_id/preferences", get(get_user_preferences))
        .route(
            "/users/:user_id/preferences",
            post(set_user_preferences),
        )
        .with_state(state)
}

pub async fn start_http_server(
    port: u16,
    channels: Arc<RwLock<Vec<Box<dyn Channel>>>>,
    agent_manager: Arc<AgentManager>,
    whatsapp_tx: Option<mpsc::UnboundedSender<Message>>,
    whatsapp_verify_token: String,
    user_prefs: Arc<UserPreferences>,
) -> anyhow::Result<()> {
    let state = AppState {
        channels,
        agent_manager,
        whatsapp_tx,
        whatsapp_verify_token,
        user_prefs,
    };

    let app = create_router(state);
    let addr = format!("0.0.0.0:{}", port);

    info!("HTTP server starting on {}", addr);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!("HTTP server listening on {}", addr);
    info!("  Health:   http://localhost:{}/health", port);
    info!("  Status:   http://localhost:{}/status", port);
    info!("  Metrics:  http://localhost:{}/metrics", port);
    info!("  Ready:    http://localhost:{}/ready", port);
    info!("  Live:     http://localhost:{}/live", port);

    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn make_state() -> AppState {
        AppState {
            channels: Arc::new(RwLock::new(Vec::new())),
            agent_manager: Arc::new(AgentManager::new(Default::default())),
            whatsapp_tx: None,
            whatsapp_verify_token: "test-token".to_string(),
            user_prefs: Arc::new(UserPreferences::new()),
        }
    }

    #[tokio::test]
    async fn test_health_endpoint() {
        let app = create_router(make_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_whatsapp_verify_valid() {
        let app = create_router(make_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/webhooks/whatsapp?hub.mode=subscribe&hub.verify_token=test-token&hub.challenge=abc123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_whatsapp_verify_invalid_token() {
        let app = create_router(make_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/webhooks/whatsapp?hub.mode=subscribe&hub.verify_token=wrong&hub.challenge=abc123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_user_preferences_set_and_get() {
        let state = make_state();
        // Manually set a pref to test the GET
        state.user_prefs.set("alice", "whatsapp-main").await;

        let app = create_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/users/alice/preferences")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["preferred_channel_id"], "whatsapp-main");
    }
}
