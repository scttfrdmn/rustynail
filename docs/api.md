# HTTP API Reference

RustyNail exposes a single HTTP server on `gateway.http_port` (default 8080). This document covers all routes.

## Authentication

When `GATEWAY_API_TOKEN` is set, all routes except `/live` and `/ready` require:

```
Authorization: Bearer <token>
```

Requests without a valid token return `401 Unauthorized`. The token is checked with constant-time comparison (no timing oracle). Tokens can be updated at runtime via SIGHUP without restart.

**Auth-exempt routes:** `/live`, `/ready`

---

## Health & Probes

### `GET /health`

Basic health check for load balancers.

**Auth required:** no (if token is set, auth is required — use `/live` for probe-exempt check)

**Response 200:**
```json
{"status": "ok", "version": "0.13.0"}
```

---

### `GET /ready`

Kubernetes readiness probe. Returns 503 until the gateway has fully started.

**Auth required:** never (always exempt)

**Response 200:** `{"status": "ready"}`
**Response 503:** `{"status": "not_ready"}`

---

### `GET /live`

Kubernetes liveness probe. Returns 200 while the process is running.

**Auth required:** never (always exempt)

**Response 200:** `{"status": "alive"}`

---

### `GET /status`

Detailed system status including channel health and active users.

**Auth required:** yes (when token configured)

**Response 200:**
```json
{
  "status": "running",
  "version": "0.13.0",
  "uptime_seconds": 3600,
  "channels": [
    {"name": "discord-main", "status": "healthy", "detail": ""},
    {"name": "slack-main",   "status": "degraded", "detail": "reconnecting"}
  ],
  "active_users": 42
}
```

---

### `GET /metrics`

Prometheus metrics endpoint.

**Auth required:** yes (when token configured)
**Content-Type:** `text/plain; version=0.0.4`

```
# HELP rustynail_messages_in_total Total inbound messages
# TYPE rustynail_messages_in_total counter
rustynail_messages_in_total 1234
...
```

Key metrics: `rustynail_messages_in_total`, `rustynail_messages_out_total`, `rustynail_active_users`, `rustynail_healthy_channels`, `rustynail_message_duration_seconds`, `rustynail_tokens_in_total`, `rustynail_tokens_out_total`, `rustynail_auth_failures_total`, `rustynail_rate_limit_hits_total`, `rustynail_llm_errors_total`, `rustynail_llm_retries_total`.

---

## Dashboard

### `GET /dashboard`

Web monitoring dashboard (embedded HTML/CSS/JS, no CDN dependency).

**Auth required:** yes if `DASHBOARD_AUTH_PASSWORD` set (HTTP Basic Auth, username `rustynail`)
**Response:** HTML page with real-time stats, channel health table, and recent messages ring buffer.

---

### `GET /dashboard/data`

Dashboard JSON data endpoint.

**Auth required:** same as `/dashboard`

**Response 200:**
```json
{
  "version": "0.13.0",
  "uptime_seconds": 3600,
  "messages_in": 1234,
  "messages_out": 1230,
  "tokens_in": 45000,
  "tokens_out": 38000,
  "active_users": 42,
  "channels": [...],
  "recent_messages": [
    {"ts": "2026-03-18T12:00:00Z", "user_id": "u1", "direction": "in", "content": "hello"}
  ]
}
```

---

### `GET /dashboard/ws`

WebSocket endpoint for live dashboard push updates.

**Auth required:** same as `/dashboard`
**Upgrade:** WebSocket

**Server-sent frame types:**

```json
// Stats update (every 5 seconds)
{"type": "stats_update", "data": { ...same as /dashboard/data... }}

// Message event (on each inbound/outbound message)
{"type": "message_event", "data": {"ts": "...", "user_id": "...", "direction": "in", "content": "..."}}
```

Origin restriction: if `gateway.allowed_ws_origins` is set, upgrade requests from unlisted origins return `403 Forbidden`.

---

## Channel Webhooks

### `GET /webhooks/whatsapp`

Meta webhook verification (challenge-response).

**Query params:** `hub.mode=subscribe`, `hub.verify_token=<token>`, `hub.challenge=<value>`
**Response 200:** echoes `hub.challenge` if verify token matches
**Response 403:** verify token mismatch

---

### `POST /webhooks/whatsapp`

Inbound WhatsApp messages.

**Body:** Meta Cloud API webhook JSON
**Response 200:** `"ok"`

---

### `POST /webhooks/telegram`

Inbound Telegram updates.

**Headers:** `X-Telegram-Bot-Api-Secret-Token: <secret>` (when `webhook_secret` is configured)
**Body:** Telegram Update JSON
**Response 200:** `"ok"`

---

### `POST /webhooks/slack`

Inbound Slack events (Events API + `url_verification` challenge).

**Headers:** `X-Slack-Signature`, `X-Slack-Request-Timestamp` (HMAC-SHA256 verification)
**Body:** Slack event JSON
**Response 200:** `""` (empty) or `{"challenge": "..."}` for url_verification

---

### `POST /webhooks/sms`

Inbound Twilio SMS messages (TwiML webhook).

**Body:** URL-encoded form (`Body`, `From`, `To`)
**Response 200:** Empty TwiML `<Response/>`

---

### `POST /channels/teams/messages`

Inbound Microsoft Teams Bot Framework activities.

**Headers:** `Authorization: HMAC <hex>` (when `hmac_secret` is configured)
**Body:** Bot Framework Activity JSON
**Response 200:** `"ok"`
**Response 401:** HMAC validation failed

---

### `POST /webhooks/:name`

Generic inbound webhook. `:name` must match a configured `channels.webhook.endpoints[].path`.

**Headers:** `X-Hub-Signature-256: sha256=<hex>` (when endpoint `secret` is configured)
**Body:** Any JSON or text
**Response 200:** `"ok"`
**Response 401:** HMAC validation failed
**Response 404:** No endpoint matches `:name`

---

## Webchat

### `GET /channels/webchat/ws`

WebSocket chat session for the webchat widget.

**Query params:** `session_id=<uuid>` (required)
**Upgrade:** WebSocket
**Origin restriction:** `channels.webchat.allowed_origins` and/or `gateway.allowed_ws_origins`

**Client → server frames:** plain text message content

**Server → client frame types:**
```json
// Token streaming (arrives in ~5-byte chunks)
{"type": "token", "content": "Hello"}
// Stream complete
{"type": "done"}
// Error
{"type": "error", "message": "..."}
```

---

### `GET /channels/webchat/widget.js`

Serves the self-contained chat widget JavaScript (~3KB, no dependencies).

**Response:** `application/javascript`

Embed with:
```html
<script src="https://rustynail.example.com/channels/webchat/widget.js"></script>
```

---

## Admin API

All admin routes require bearer auth when `GATEWAY_API_TOKEN` is set.

### `DELETE /admin/memory/:user_id`

Clear a user's conversation history from the memory backend.

**Path param:** `user_id` — the user whose history to delete
**Response 200:** `{"cleared": true, "user_id": "..."}`

```bash
curl -X DELETE http://localhost:8080/admin/memory/user123 \
  -H 'Authorization: Bearer <token>'
```

---

### `POST /admin/skills/reload`

Hot-reload skills from disk without restarting the server.

**Body:** none
**Response 200:**
```json
{"skills_loaded": 4}
```

```bash
curl -X POST http://localhost:8080/admin/skills/reload \
  -H 'Authorization: Bearer <token>'
```

---

### `GET /admin/channels/health`

Per-channel health status with detail strings for degraded/unhealthy channels.

**Response 200:**
```json
[
  {"name": "discord-main", "status": "healthy",  "detail": ""},
  {"name": "slack-main",   "status": "degraded", "detail": "reconnecting, last error: ..."}
]
```

```bash
curl http://localhost:8080/admin/channels/health \
  -H 'Authorization: Bearer <token>'
```

---

## Cron

### `GET /cron/jobs`

Snapshot of all configured cron job statuses.

**Auth required:** yes (when token configured)

**Response 200:**
```json
[
  {
    "name": "daily-digest",
    "schedule": "24h",
    "enabled": true,
    "last_run": "2026-03-18T00:00:00Z",
    "next_run": "2026-03-19T00:00:00Z",
    "run_count": 7
  }
]
```

---

## User Preferences

### `GET /users/:user_id/preferences`

Get cross-channel routing preferences for a user.

**Response 200:**
```json
{"user_id": "u1", "preferred_channel": "discord-main"}
```

---

### `POST /users/:user_id/preferences`

Set the preferred response channel for a user.

**Body:**
```json
{"preferred_channel": "slack-main"}
```

**Response 200:** `{"user_id": "u1", "preferred_channel": "slack-main"}`

---

## OpenAI-Compatible Endpoint

### `POST /v1/chat/completions`

OpenAI-compatible chat completions. Accepts OpenAI SDK clients.

**Auth required:** yes (when token configured) — use the same bearer token
**Content-Type:** `application/json`

**Request body:**
```json
{
  "model": "rustynail",
  "messages": [
    {"role": "user", "content": "Hello!"}
  ],
  "stream": false
}
```

**Non-streaming response (200):**
```json
{
  "id": "chatcmpl-...",
  "object": "chat.completion",
  "model": "rustynail",
  "choices": [
    {
      "index": 0,
      "message": {"role": "assistant", "content": "Hello! How can I help?"},
      "finish_reason": "stop"
    }
  ],
  "usage": {"prompt_tokens": 10, "completion_tokens": 8, "total_tokens": 18}
}
```

**SSE streaming (`"stream": true`):**

```
data: {"id":"chatcmpl-...","object":"chat.completion.chunk","choices":[{"delta":{"content":"Hello"},"index":0}]}

data: {"id":"chatcmpl-...","object":"chat.completion.chunk","choices":[{"delta":{"content":"!"},"index":0}]}

data: [DONE]
```

---

## Test Channel

Only available when `channels.test_channel: true`. Never enable in production.

### `POST /test/send`

Inject a message into the test channel.

**Body:**
```json
{"user_id": "u1", "content": "hello world"}
```

**Response 200:** `{"queued": true}`

```bash
curl -X POST http://localhost:8080/test/send \
  -H 'Content-Type: application/json' \
  -d '{"user_id":"u1","content":"hello"}'
```

---

### `GET /test/responses`

Drain all pending bot responses from the test channel.

**Response 200:**
```json
[
  {"user_id": "u1", "content": "Hello! I received: hello"}
]
```

```bash
curl http://localhost:8080/test/responses
```
