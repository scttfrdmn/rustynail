use crate::agents::AgentManager;
use crate::channels::Channel;
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::get,
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

/// HTTP server state shared across handlers
#[derive(Clone)]
pub struct AppState {
    pub channels: Arc<RwLock<Vec<Box<dyn Channel>>>>,
    pub agent_manager: Arc<AgentManager>,
}

/// Health check response
#[derive(Debug, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
}

/// Status response with detailed information
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

/// Metrics response
#[derive(Debug, Serialize, Deserialize)]
pub struct MetricsResponse {
    pub active_users: usize,
    pub channels_count: usize,
    pub healthy_channels: usize,
}

/// Health check endpoint - returns OK if service is running
async fn health_handler() -> impl IntoResponse {
    Json(HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

/// Status endpoint - returns detailed service status
async fn status_handler(State(state): State<AppState>) -> impl IntoResponse {
    let channels = state.channels.read().await;
    let active_users = state.agent_manager.active_users().await;

    let channel_statuses: Vec<ChannelStatus> = channels
        .iter()
        .map(|channel| {
            let health = channel.health();
            ChannelStatus {
                id: channel.id().to_string(),
                name: channel.name().to_string(),
                health: format!("{:?}", health),
                running: channel.is_running(),
            }
        })
        .collect();

    Json(StatusResponse {
        status: "running".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        channels: channel_statuses,
        active_users,
    })
}

/// Metrics endpoint - returns operational metrics
async fn metrics_handler(State(state): State<AppState>) -> impl IntoResponse {
    let channels = state.channels.read().await;
    let active_users = state.agent_manager.active_users().await;

    let healthy_channels = channels
        .iter()
        .filter(|c| c.health().is_healthy())
        .count();

    Json(MetricsResponse {
        active_users,
        channels_count: channels.len(),
        healthy_channels,
    })
}

/// Readiness check - returns OK only if system is ready to serve traffic
async fn readiness_handler(State(state): State<AppState>) -> impl IntoResponse {
    let channels = state.channels.read().await;

    // Check if at least one channel is operational
    let has_operational_channel = channels.iter().any(|c| c.health().is_operational());

    if has_operational_channel {
        (StatusCode::OK, Json(HealthResponse {
            status: "ready".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }))
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, Json(HealthResponse {
            status: "not ready".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }))
    }
}

/// Liveness check - returns OK if service is alive
async fn liveness_handler() -> impl IntoResponse {
    Json(HealthResponse {
        status: "alive".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

/// Create and configure the HTTP server router
pub fn create_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health_handler))
        .route("/status", get(status_handler))
        .route("/metrics", get(metrics_handler))
        .route("/ready", get(readiness_handler))
        .route("/live", get(liveness_handler))
        .with_state(state)
}

/// Start the HTTP server
pub async fn start_http_server(
    port: u16,
    channels: Arc<RwLock<Vec<Box<dyn Channel>>>>,
    agent_manager: Arc<AgentManager>,
) -> anyhow::Result<()> {
    let state = AppState {
        channels,
        agent_manager,
    };

    let app = create_router(state);
    let addr = format!("0.0.0.0:{}", port);

    info!("HTTP server starting on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;

    info!("HTTP server listening on {}", addr);
    info!("  Health: http://localhost:{}/health", port);
    info!("  Status: http://localhost:{}/status", port);
    info!("  Metrics: http://localhost:{}/metrics", port);
    info!("  Ready: http://localhost:{}/ready", port);
    info!("  Live: http://localhost:{}/live", port);

    axum::serve(listener, app).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    #[tokio::test]
    async fn test_health_endpoint() {
        let state = AppState {
            channels: Arc::new(RwLock::new(Vec::new())),
            agent_manager: Arc::new(AgentManager::new(Default::default())),
        };

        let app = create_router(state);

        let response = app
            .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }
}
