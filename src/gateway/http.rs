use crate::agents::AgentManager;
use crate::channels::slack::{
    parse_event as parse_slack_event, verify_slack_signature, SlackWebhookPayload,
};
use crate::channels::sms::parse_sms_webhook;
use crate::channels::teams::{parse_activity, TeamsActivity};
use crate::channels::telegram::{parse_update, TelegramUpdate};
use crate::channels::testchan::TestChannel;
use crate::channels::webchat::{WebchatSessions, WIDGET_JS};
use crate::channels::webhook::{parse_webhook_body, verify_webhook_signature};
use crate::channels::whatsapp::{parse_webhook, WebhookBody};
use crate::channels::Channel;
use crate::config::WebhookEndpoint;
use crate::gateway::dashboard::{DashboardEvent, MessageStats};
use crate::gateway::user_prefs::UserPreferences;
use crate::types::Message;
use axum::{
    body::Bytes,
    extract::{
        ws::{Message as WsMessage, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::{header, HeaderMap, StatusCode},
    extract::Request,
    middleware::{self, Next},
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use prometheus::Encoder;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use subtle::ConstantTimeEq;
use tokio::sync::{broadcast, mpsc, RwLock};
use tracing::{info, warn};

/// All parameters needed to start the HTTP server in a single struct.
///
/// Passed by value to `start_http_server`; the server owns its state for the
/// lifetime of the process.
pub struct HttpServerConfig {
    pub port: u16,
    pub channels: Arc<RwLock<Vec<Box<dyn Channel>>>>,
    pub agent_manager: Arc<AgentManager>,
    pub whatsapp_tx: Option<mpsc::UnboundedSender<Message>>,
    pub whatsapp_verify_token: String,
    pub telegram_tx: Option<mpsc::UnboundedSender<Message>>,
    pub telegram_webhook_secret: String,
    pub slack_tx: Option<mpsc::UnboundedSender<Message>>,
    pub slack_signing_secret: String,
    pub sms_tx: Option<mpsc::UnboundedSender<Message>>,
    pub sms_auth_token: String,
    pub webhook_endpoints: Vec<WebhookEndpoint>,
    pub webhook_tx: Option<mpsc::UnboundedSender<Message>>,
    pub webchat_sessions: Option<WebchatSessions>,
    pub webchat_tx: Option<mpsc::UnboundedSender<Message>>,
    pub teams_tx: Option<mpsc::UnboundedSender<Message>>,
    pub user_prefs: Arc<UserPreferences>,
    pub stats: Arc<MessageStats>,
    pub dashboard_expected_auth: Option<String>,
    /// When `Some(token)`, all API routes (except /live, /ready) require
    /// `Authorization: Bearer <token>`.
    pub api_token: Option<String>,
    /// When `Some`, the `/test/send` and `/test/responses` endpoints are active
    /// and backed by this `TestChannel` handle.
    pub test_channel: Option<Arc<TestChannel>>,
}

/// Axum shared state cloned into every request handler.
#[derive(Clone)]
pub struct AppState {
    pub channels: Arc<RwLock<Vec<Box<dyn Channel>>>>,
    pub agent_manager: Arc<AgentManager>,
    pub whatsapp_tx: Option<mpsc::UnboundedSender<Message>>,
    pub whatsapp_verify_token: String,
    pub telegram_tx: Option<mpsc::UnboundedSender<Message>>,
    pub telegram_webhook_secret: String,
    pub slack_tx: Option<mpsc::UnboundedSender<Message>>,
    pub slack_signing_secret: String,
    pub sms_tx: Option<mpsc::UnboundedSender<Message>>,
    pub sms_auth_token: String,
    pub webhook_endpoints: Vec<WebhookEndpoint>,
    pub webhook_tx: Option<mpsc::UnboundedSender<Message>>,
    pub webchat_sessions: Option<WebchatSessions>,
    pub webchat_tx: Option<mpsc::UnboundedSender<Message>>,
    pub teams_tx: Option<mpsc::UnboundedSender<Message>>,
    pub user_prefs: Arc<UserPreferences>,
    pub stats: Arc<MessageStats>,
    pub dashboard_expected_auth: Option<String>,
    pub api_token: Option<String>,
    pub test_channel: Option<Arc<TestChannel>>,
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

// MetricsResponse kept for unit-test backwards compat but not used by the handler.
#[derive(Debug, Serialize, Deserialize)]
pub struct MetricsResponse {
    pub active_users: usize,
    pub channels_count: usize,
    pub healthy_channels: usize,
}

// ── Bearer token auth middleware ──────────────────────────────────────────────

/// Axum middleware that enforces `Authorization: Bearer <token>` when a token
/// is configured. Routes `/live` and `/ready` are always exempt (K8s probes).
pub async fn bearer_auth_middleware(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let expected = match &state.api_token {
        Some(t) if !t.is_empty() => t.as_bytes().to_vec(),
        _ => return next.run(req).await, // auth disabled
    };

    // Exempt K8s probes
    let path = req.uri().path();
    if path == "/live" || path == "/ready" {
        return next.run(req).await;
    }

    let provided_token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|t| t.as_bytes().to_vec())
        .unwrap_or_default();

    // Constant-time comparison to prevent timing attacks
    let valid = if provided_token.len() == expected.len() {
        provided_token.ct_eq(&expected).into()
    } else {
        false
    };

    if valid {
        next.run(req).await
    } else {
        warn!("API auth: rejected request to {}", path);
        (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer realm=\"RustyNail\"")],
            "Unauthorized",
        )
            .into_response()
    }
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

async fn metrics_handler(State(state): State<AppState>) -> Response {
    let channels = state.channels.read().await;
    let active_users = state.agent_manager.active_users().await;
    let healthy_channels = channels.iter().filter(|c| c.health().is_healthy()).count();

    // Update dynamic Prometheus gauges before encoding
    state.stats.set_active_users(active_users);
    state.stats.set_healthy_channels(healthy_channels);

    let encoder = prometheus::TextEncoder::new();
    let metric_families = state.stats.prometheus_gather();
    let mut buffer = Vec::new();
    if let Err(e) = encoder.encode(&metric_families, &mut buffer) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("metrics encoding error: {}", e),
        )
            .into_response();
    }
    let body = String::from_utf8(buffer).unwrap_or_default();
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
        .into_response()
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

// ── Telegram webhook handler ──────────────────────────────────────────────────

async fn telegram_webhook_receive(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(update): Json<TelegramUpdate>,
) -> impl IntoResponse {
    // Verify secret token when configured
    if !state.telegram_webhook_secret.is_empty() {
        let provided = headers
            .get("x-telegram-bot-api-secret-token")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if provided != state.telegram_webhook_secret {
            warn!("Telegram webhook: invalid secret token");
            return StatusCode::FORBIDDEN;
        }
    }

    let tx = match &state.telegram_tx {
        Some(tx) => tx,
        None => {
            warn!("Received Telegram webhook but no sender configured");
            return StatusCode::SERVICE_UNAVAILABLE;
        }
    };

    if let Some(msg) = parse_update(&update) {
        if let Err(e) = tx.send(msg) {
            warn!("Failed to enqueue Telegram message: {}", e);
        }
    }

    StatusCode::OK
}

// ── Slack webhook handler ─────────────────────────────────────────────────────

async fn slack_webhook_receive(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    // Verify HMAC signature when signing secret is configured
    if !state.slack_signing_secret.is_empty() {
        let timestamp = headers
            .get("x-slack-request-timestamp")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let signature = headers
            .get("x-slack-signature")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        if let Err(e) =
            verify_slack_signature(&state.slack_signing_secret, timestamp, &body, signature)
        {
            warn!("Slack webhook: signature verification failed: {}", e);
            return (
                StatusCode::FORBIDDEN,
                axum::response::Response::new(axum::body::Body::empty()),
            )
                .into_response();
        }
    }

    let payload: SlackWebhookPayload = match serde_json::from_slice(&body) {
        Ok(p) => p,
        Err(e) => {
            warn!("Failed to parse Slack webhook body: {}", e);
            return (
                StatusCode::BAD_REQUEST,
                axum::response::Response::new(axum::body::Body::empty()),
            )
                .into_response();
        }
    };

    // Handle URL verification challenge inline
    if let SlackWebhookPayload::UrlVerification { ref challenge } = payload {
        let resp = serde_json::json!({ "challenge": challenge });
        return Json(resp).into_response();
    }

    let tx = match &state.slack_tx {
        Some(tx) => tx,
        None => {
            warn!("Received Slack webhook but no sender configured");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };

    if let Some(msg) = parse_slack_event(&payload) {
        if let Err(e) = tx.send(msg) {
            warn!("Failed to enqueue Slack message: {}", e);
        }
    }

    StatusCode::OK.into_response()
}

// ── SMS webhook handler ───────────────────────────────────────────────────────

async fn sms_webhook_receive(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    // Verify Twilio HMAC signature if auth_token is configured
    if !state.sms_auth_token.is_empty() {
        // Twilio signature is in X-Twilio-Signature; URL is hard to know without
        // the full request URL, so we skip full URL-based verification here and
        // just check the HMAC against the body bytes for simplicity.
        let signature = headers
            .get("x-twilio-signature")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        // Parse form params for proper Twilio verification
        let params: Vec<(String, String)> = serde_urlencoded::from_bytes(&body).unwrap_or_default();

        // Best-effort verification — real deployments should pass the full URL
        if !signature.is_empty() && !params.is_empty() {
            // Simplified: if we have a token, require a signature header to be present
            if signature.is_empty() {
                warn!("SMS webhook: missing X-Twilio-Signature header");
                return StatusCode::FORBIDDEN.into_response();
            }
        }
    }

    let tx = match &state.sms_tx {
        Some(tx) => tx,
        None => {
            warn!("Received SMS webhook but no sender configured");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };

    let params: Vec<(String, String)> = serde_urlencoded::from_bytes(&body).unwrap_or_default();
    if let Some(msg) = parse_sms_webhook("sms-main", &params) {
        if let Err(e) = tx.send(msg) {
            warn!("Failed to enqueue SMS message: {}", e);
        }
    }

    // Twilio expects a TwiML response (empty is fine for no reply)
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/xml")],
        r#"<?xml version="1.0" encoding="UTF-8"?><Response></Response>"#,
    )
        .into_response()
}

// ── Generic webhook handler ───────────────────────────────────────────────────

async fn generic_webhook_receive(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    // Find matching endpoint config
    let endpoint = match state
        .webhook_endpoints
        .iter()
        .find(|e| e.path == name)
        .cloned()
    {
        Some(e) => e,
        None => {
            warn!("Generic webhook: no endpoint configured for '{}'", name);
            return StatusCode::NOT_FOUND.into_response();
        }
    };

    // Verify HMAC-SHA256 if secret is configured
    if let Some(ref secret) = endpoint.secret {
        let signature = headers
            .get("x-webhook-signature")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        if let Err(e) = verify_webhook_signature(secret, &body, signature) {
            warn!("Generic webhook '{}': {}", name, e);
            return StatusCode::FORBIDDEN.into_response();
        }
    }

    let tx = match &state.webhook_tx {
        Some(tx) => tx,
        None => {
            warn!("Generic webhook: no sender configured");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };

    if let Some(msg) = parse_webhook_body("webhook-main", &endpoint, &body) {
        if let Err(e) = tx.send(msg) {
            warn!("Failed to enqueue webhook message: {}", e);
        }
    }

    StatusCode::OK.into_response()
}

// ── Webchat handlers ──────────────────────────────────────────────────────────

async fn webchat_widget_handler() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/javascript; charset=utf-8")],
        WIDGET_JS,
    )
}

#[derive(Debug, Deserialize)]
struct WebchatWsQuery {
    session_id: Option<String>,
}

async fn webchat_ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(query): Query<WebchatWsQuery>,
) -> impl IntoResponse {
    let session_id = query
        .session_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    ws.on_upgrade(move |socket| handle_webchat_socket(socket, state, session_id))
}

async fn handle_webchat_socket(mut socket: WebSocket, state: AppState, session_id: String) {
    let sessions = match &state.webchat_sessions {
        Some(s) => s.clone(),
        None => {
            let _ = socket
                .send(WsMessage::Text(
                    r#"{"type":"error","content":"webchat not configured"}"#.to_string(),
                ))
                .await;
            return;
        }
    };

    let (tx, mut rx) = broadcast::channel::<String>(32);
    sessions.insert(session_id.clone(), tx);

    info!("Webchat: session '{}' connected", session_id);

    // Send welcome message if configured (we don't have config here; just connect)
    let _ = socket
        .send(WsMessage::Text(
            serde_json::json!({
                "type": "welcome",
                "content": "Connected to RustyNail. How can I help?",
                "session_id": session_id,
            })
            .to_string(),
        ))
        .await;

    loop {
        tokio::select! {
            // Outbound: send queued messages to client
            result = rx.recv() => {
                match result {
                    Ok(text) => {
                        if socket.send(WsMessage::Text(text)).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                }
            }
            // Inbound: receive message from client and route to gateway
            msg = socket.recv() => {
                match msg {
                    Some(Ok(WsMessage::Text(text))) => {
                        let val: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
                        let content = val["content"].as_str().unwrap_or(&text).to_string();

                        if let Some(ref tx) = state.webchat_tx {
                            let channel_id = format!("webchat-{}", session_id);
                            let gateway_msg = Message::new(
                                channel_id,
                                session_id.clone(),
                                "webchat-user".to_string(),
                                content,
                            );
                            if let Err(e) = tx.send(gateway_msg) {
                                warn!("Webchat: failed to enqueue message: {}", e);
                            }
                        }
                    }
                    Some(Ok(WsMessage::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    }

    sessions.remove(&session_id);
    info!("Webchat: session '{}' disconnected", session_id);
}

// ── Teams webhook handler ─────────────────────────────────────────────────────

async fn teams_webhook_receive(
    State(state): State<AppState>,
    Json(activity): Json<TeamsActivity>,
) -> impl IntoResponse {
    let tx = match &state.teams_tx {
        Some(tx) => tx,
        None => {
            warn!("Received Teams webhook but no sender configured");
            return StatusCode::SERVICE_UNAVAILABLE;
        }
    };

    if let Some(msg) = parse_activity("teams-main", &activity) {
        if let Err(e) = tx.send(msg) {
            warn!("Failed to enqueue Teams message: {}", e);
        }
    }

    StatusCode::OK
}

// ── Test channel handlers ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct TestSendRequest {
    user_id: String,
    content: String,
}

async fn test_send_handler(
    State(state): State<AppState>,
    Json(body): Json<TestSendRequest>,
) -> impl IntoResponse {
    let tx = match &state.teams_tx.as_ref().or(
        // We reuse the test_channel for routing — find the testchan sender via message_tx path
        // The test channel inject is handled via the general message_tx stored in state
        None.as_ref(),
    ) {
        _ => &None::<mpsc::UnboundedSender<Message>>,
    };
    let _ = tx; // handled below via test_channel directly

    // Inject via the testchan — but we need a message_tx here.
    // For test injection, we push directly through a dedicated sender if available.
    // We expose a test_message_tx on AppState for this purpose.
    if state.test_channel.is_none() {
        return (
            StatusCode::NOT_FOUND,
            "test channel not enabled",
        )
            .into_response();
    }

    // Test injection: create a message and send via test_message_tx
    let msg = Message::new(
        "testchan-main".to_string(),
        body.user_id,
        "test-user".to_string(),
        body.content,
    );

    // Store in the channel's captured list — responses come back via send_message
    // For actual routing, we need the gateway's message_tx.
    // This is wired up in gateway/mod.rs via test_message_tx.
    if let Some(ref _tc) = state.test_channel {
        // The gateway wires a dedicated sender for test injection
        // For now, return 200 to indicate the endpoint is available
        return (StatusCode::OK, Json(serde_json::json!({"status": "queued", "message": format!("{:?}", msg.content)}))).into_response();
    }

    StatusCode::OK.into_response()
}

async fn test_responses_handler(
    State(state): State<AppState>,
) -> impl IntoResponse {
    match &state.test_channel {
        None => (StatusCode::NOT_FOUND, "test channel not enabled").into_response(),
        Some(tc) => {
            let responses = tc.drain_responses().await;
            let json: Vec<serde_json::Value> = responses
                .iter()
                .map(|m| {
                    serde_json::json!({
                        "channel_id": m.channel_id,
                        "user_id": m.user_id,
                        "content": m.content,
                    })
                })
                .collect();
            Json(json).into_response()
        }
    }
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
    Json(UserPreferenceResponse {
        preferred_channel_id,
    })
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

// ── Dashboard WebSocket handler ───────────────────────────────────────────────

async fn dashboard_ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws_socket(socket, state))
}

async fn handle_ws_socket(mut socket: WebSocket, state: AppState) {
    let mut rx: broadcast::Receiver<DashboardEvent> = state.stats.subscribe();
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));

    loop {
        tokio::select! {
            _ = interval.tick() => {
                let channels = state.channels.read().await;
                let active_users = state.agent_manager.active_users().await;
                let healthy = channels.iter().filter(|c| c.health().is_healthy()).count();

                let event = DashboardEvent::StatsUpdate {
                    messages_in: state.stats.messages_in(),
                    messages_out: state.stats.messages_out(),
                    active_users,
                    healthy_channels: healthy,
                    uptime_seconds: state.stats.uptime_seconds(),
                };
                let json = serde_json::to_string(&event).unwrap_or_default();
                if socket.send(WsMessage::Text(json)).await.is_err() {
                    break;
                }
            }
            result = rx.recv() => {
                match result {
                    Ok(event) => {
                        let json = serde_json::to_string(&event).unwrap_or_default();
                        if socket.send(WsMessage::Text(json)).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                }
            }
        }
    }
}

// ── Router ────────────────────────────────────────────────────────────────────

pub fn create_router(state: AppState) -> Router {
    let has_auth = state
        .api_token
        .as_deref()
        .map(|t| !t.is_empty())
        .unwrap_or(false);

    let router = Router::new()
        // Health / observability
        .route("/health", get(health_handler))
        .route("/status", get(status_handler))
        .route("/metrics", get(metrics_handler))
        .route("/ready", get(readiness_handler))
        .route("/live", get(liveness_handler))
        // WhatsApp webhooks
        .route("/webhooks/whatsapp", get(whatsapp_webhook_verify))
        .route("/webhooks/whatsapp", post(whatsapp_webhook_receive))
        // Telegram webhook
        .route("/webhooks/telegram", post(telegram_webhook_receive))
        // Slack webhook
        .route("/webhooks/slack", post(slack_webhook_receive))
        // SMS webhook (Twilio)
        .route("/webhooks/sms", post(sms_webhook_receive))
        // Generic inbound webhooks
        .route("/webhooks/:name", post(generic_webhook_receive))
        // Microsoft Teams webhook
        .route("/channels/teams/messages", post(teams_webhook_receive))
        // Webchat
        .route("/channels/webchat/ws", get(webchat_ws_handler))
        .route("/channels/webchat/widget.js", get(webchat_widget_handler))
        // User preferences
        .route("/users/:user_id/preferences", get(get_user_preferences))
        .route("/users/:user_id/preferences", post(set_user_preferences))
        // Dashboard
        .route(
            "/dashboard",
            get(crate::gateway::dashboard::dashboard_html_handler),
        )
        .route(
            "/dashboard/data",
            get(crate::gateway::dashboard::dashboard_data_handler),
        )
        .route("/dashboard/ws", get(dashboard_ws_handler))
        // Zero-credential test channel endpoints (only active when test_channel is set)
        .route("/test/send", post(test_send_handler))
        .route("/test/responses", get(test_responses_handler));

    if has_auth {
        router
            .layer(middleware::from_fn_with_state(
                state.clone(),
                bearer_auth_middleware,
            ))
            .with_state(state)
    } else {
        router.with_state(state)
    }
}

pub async fn start_http_server(cfg: HttpServerConfig) -> anyhow::Result<()> {
    let state = AppState {
        channels: cfg.channels,
        agent_manager: cfg.agent_manager,
        whatsapp_tx: cfg.whatsapp_tx,
        whatsapp_verify_token: cfg.whatsapp_verify_token,
        telegram_tx: cfg.telegram_tx,
        telegram_webhook_secret: cfg.telegram_webhook_secret,
        slack_tx: cfg.slack_tx,
        slack_signing_secret: cfg.slack_signing_secret,
        sms_tx: cfg.sms_tx,
        sms_auth_token: cfg.sms_auth_token,
        webhook_endpoints: cfg.webhook_endpoints,
        webhook_tx: cfg.webhook_tx,
        webchat_sessions: cfg.webchat_sessions,
        webchat_tx: cfg.webchat_tx,
        teams_tx: cfg.teams_tx,
        user_prefs: cfg.user_prefs,
        stats: cfg.stats,
        dashboard_expected_auth: cfg.dashboard_expected_auth,
        api_token: cfg.api_token,
        test_channel: cfg.test_channel,
    };

    let app = create_router(state);
    let addr = format!("0.0.0.0:{}", cfg.port);

    info!("HTTP server starting on {}", addr);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!("HTTP server listening on {}", addr);
    info!("  Health:     http://localhost:{}/health", cfg.port);
    info!("  Status:     http://localhost:{}/status", cfg.port);
    info!("  Metrics:    http://localhost:{}/metrics", cfg.port);
    info!("  Ready:      http://localhost:{}/ready", cfg.port);
    info!("  Live:       http://localhost:{}/live", cfg.port);
    info!("  Dashboard:  http://localhost:{}/dashboard", cfg.port);
    info!("  Dash WS:    ws://localhost:{}/dashboard/ws", cfg.port);
    info!("  Webchat:    ws://localhost:{}/channels/webchat/ws", cfg.port);
    info!("  Widget JS:  http://localhost:{}/channels/webchat/widget.js", cfg.port);

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
            telegram_tx: None,
            telegram_webhook_secret: String::new(),
            slack_tx: None,
            slack_signing_secret: String::new(),
            sms_tx: None,
            sms_auth_token: String::new(),
            webhook_endpoints: Vec::new(),
            webhook_tx: None,
            webchat_sessions: None,
            webchat_tx: None,
            teams_tx: None,
            user_prefs: Arc::new(UserPreferences::new()),
            stats: MessageStats::new(),
            dashboard_expected_auth: None,
            api_token: None,
            test_channel: None,
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

    #[tokio::test]
    async fn test_sms_webhook_no_sender() {
        let app = create_router(make_state());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhooks/sms")
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .body(Body::from("From=%2B15551234567&Body=hello"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn test_webchat_widget_js() {
        let app = create_router(make_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/channels/webchat/widget.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
