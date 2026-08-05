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
{"status": "ok", "version": "0.15.0"}
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
  "version": "0.15.0",
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
  "version": "0.15.0",
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

Get a user's stored preferences. Unset fields are omitted, and an unknown user
returns an empty object rather than a 404 — "no preferences" and "no such user"
are the same state here.

**Response 200:**
```json
{"preferred_channel_id": "discord-main", "timezone": "America/New_York"}
```

| Field | Meaning |
|-------|---------|
| `preferred_channel_id` | Responses are routed here regardless of where the message arrived |
| `timezone` | IANA zone quarry deadlines resolve in. Absent = the `quarry.default_timezone` operator setting applies, then UTC |

---

### `POST /users/:user_id/preferences`

Set one or more preferences. **Both fields are optional and an absent field leaves
the stored value alone** — this is a partial update, so setting a timezone does not
require knowing (or risk clobbering) the channel preference.

**Body:**
```json
{"preferred_channel_id": "slack-main", "timezone": "Asia/Tokyo"}
```

`timezone` must be an IANA name from the tz database. An unrecognised name is
rejected rather than stored: a bad zone would fall back silently at resolve time
and move a sender's deadline — and therefore their budget — without telling them.

**Response 200:** empty body.

**Response 400:**
```json
{"error": "unknown timezone", "detail": "\"Mars/Olympus_Mons\" is not an IANA timezone name. Use a name from the tz database, e.g. \"America/New_York\"."}
```

---

## OpenAI-Compatible Endpoint

### `POST /v1/chat/completions`

OpenAI-compatible chat completions. Accepts OpenAI SDK clients.

**Auth required:** yes (when token configured) — use the same bearer token
**Content-Type:** `application/json`

**Request fields:**

| Field | Type | Notes |
|-------|------|-------|
| `model` | string | See [Model resolution](#model-resolution). Aliases are accepted; a different model is refused. |
| `messages` | array | `{role, content}`. All roles and all turns are used, including `system`. |
| `stream` | bool | SSE streaming. Not supported with `stateless: true`. |
| `user` | string | Conversation identity for the stateful path. Ignored when `stateless: true`. |
| `max_tokens` | int | Output-token ceiling. Applied to the primary provider *and* every fallback, so a failover cannot lift it. Not supported on Ollama (agenkit's config exposes no equivalent). Must be positive. |
| `stateless` | bool | Run with no conversation state. Not an OpenAI field — see below. |

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
  "created": 1770000000,
  "model": "claude-3-5-sonnet-20241022",
  "choices": [
    {
      "index": 0,
      "message": {"role": "assistant", "content": "Hello! How can I help?"},
      "finish_reason": "stop"
    }
  ],
  "usage": {"prompt_tokens": 10, "completion_tokens": 8, "total_tokens": 18},
  "cost": {"amount_usd": 0.00015, "micro_usd": 150, "currency": "USD"}
}
```

**SSE streaming (`"stream": true`):**

```
data: {"id":"chatcmpl-...","object":"chat.completion.chunk","choices":[{"delta":{"content":"Hello"},"index":0}]}

data: {"id":"chatcmpl-...","object":"chat.completion.chunk","choices":[{"delta":{"content":"!"},"index":0}]}

data: [DONE]
```

The SSE body is currently assembled in full before the response is written, so it
is a valid event stream but not incrementally delivered.

#### Stateless mode

`"stateless": true` runs the completion with **no conversation state**: no history
is read, none is written, and no per-user agent is created. Two calls with the
same `user` are fully independent.

Use it when independent requests must not contaminate each other — for example
when a caller decomposes a problem into sub-problems and treats agreement between
sibling sub-answers as a signal. If the siblings shared a conversation history,
each would see its predecessors' answers and agree for the wrong reason.

Differences from the default stateful path:

| | Stateful (default) | `stateless: true` |
|---|---|---|
| Conversation memory | read and written | untouched |
| System prompt | RustyNail's persona + skills context | **only what you send** |
| Tools | available (ReAct loop) | never attached |
| `usage` / `cost` | always absent (see below) | present when the provider reports counts |
| `stream: true` | supported | refused |

Tools are excluded deliberately: a ReAct loop makes several provider calls and
discards each response's metadata, so token counts — and therefore cost — would be
unrecoverable.

#### Usage and cost

Both objects are **omitted entirely when the provider did not report the
underlying numbers**. They are never estimated.

- `usage` is present only when the upstream provider returned token counts. The
  stateful path routes through wrappers that do not preserve provider metadata,
  so it never reports usage. Use `stateless: true` if you need metering.
- `cost` is present only when `usage` is present *and* the resolved model has a
  pricing entry. An unpriced model yields **no cost field** rather than a zero
  one — a zero is indistinguishable from a free call, so a caller debiting a
  ledger against it would under-count spend with no way to notice.

`cost.micro_usd` is authoritative: integer micro-dollars, computed as
`round(amount_usd × 1_000_000)` — **rounded to nearest, never truncated**. A
caller converting from `amount_usd` itself must use the same rule; truncating
desyncs a local debit from the real charge by up to one micro-unit per call,
which accumulates silently over a long run.

Providers differ in what they report. Anthropic, OpenAI, LiteLLM and
openai-compatible return token counts on every call; Ollama returns them only
when it has them; Gemini and Bedrock report counts but echo the *configured*
model name rather than a provider-resolved one.

#### Model resolution

`model` in the response names the model that **actually ran** — taken from the
provider's own response where the provider reports it — never the alias the
caller asked for. Ask for `rustynail` and the response names the pinned version
that served it.

Accepted in a request:

- the configured model name exactly
- `rustynail`, `default`, `gateway` (case-insensitive), or an empty string —
  "whatever this gateway serves"
- a prefix of the configured name, so `claude-3-5-sonnet` is servable by
  `claude-3-5-sonnet-20241022`

Any other model name is **refused** with `model_not_available`. Silently serving
Claude for a `gpt-4` request would make the response's `model` field untrue, and
a caller replaying against that record would replay against the wrong model.

#### Errors

Error bodies carry a stable machine-readable `code`. Classify on `code`, never by
parsing `message` — `message` is for humans and may change.

```json
{"error": {"code": "model_not_available", "message": "...", "type": "invalid_request_error"}}
```

| Code | Status | Cause | Retryable |
|------|--------|-------|-----------|
| `no_messages` | 400 | `messages` empty or all entries blank | no |
| `model_not_available` | 400 | requested model is not served here | no |
| `invalid_max_tokens` | 400 | `max_tokens` not a positive integer | no |
| `upstream_provider_error` | 502 | the LLM provider failed or is unreachable | yes |

Each code maps to exactly one cause, and no status is shared between a
request-shape error and a transport fault — a caller that cannot tell them apart
either retries a malformed request forever or gives up on a transient fault.

#### Using this endpoint as a metered provider

This endpoint is designed to serve as the single upstream for a client that
meters its own spend and holds no provider credentials of its own — the gateway
is the chokepoint, and its provider config, retry/fallback chain and metering
apply to every call.

Such a client should:

1. POST to `http://localhost:<http_port>/v1/chat/completions` with
   `"stateless": true`.
2. Send a pinned, explicitly versioned `model`, not an alias — replay needs a
   name that resolves to one thing.
3. Read `cost.micro_usd` for ledger debits, falling back to a pre-call estimate
   only when the field is absent, and recording that it did so.
4. Classify failures on `error.code`.
5. Include the bearer token when one is configured — `/v1` sits behind auth
   (only `/live` and `/ready` are exempt), so a localhost subprocess needs the
   token in its environment.

The wire contract is pinned by `tests/quarry_provider_contract.rs`, which serves
the real router on a real socket against `llm_provider: stub` — no credentials,
no egress.

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
