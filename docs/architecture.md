# Architecture

## Overview

RustyNail is a single statically-linked binary built on async Rust (Tokio). It runs as an event-driven message gateway: inbound messages from any channel flow through an 8-stage pipeline into per-user AI agents, then back out through the originating (or preferred) channel.

## Architecture Diagram

```
┌─────────────────────────────────────────────────────────────────────────────┐
│  RustyNail Gateway (Single Binary, Tokio Runtime)                            │
│                                                                              │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │  HTTP Server (Axum :8080)                                               │ │
│  │                                                                          │ │
│  │  Health & Observability                                                  │ │
│  │    GET  /health /ready /live /status /metrics                           │ │
│  │                                                                          │ │
│  │  Web Dashboard                                                           │ │
│  │    GET  /dashboard /dashboard/data                                      │ │
│  │    WS   /dashboard/ws  ← live push (stats_update, message_event)       │ │
│  │                                                                          │ │
│  │  Channel Webhooks                                                        │ │
│  │    POST /webhooks/whatsapp  /webhooks/telegram  /webhooks/slack         │ │
│  │    POST /webhooks/sms  /webhooks/teams  /webhooks/:name                 │ │
│  │                                                                          │ │
│  │  Webchat                                                                 │ │
│  │    WS   /channels/webchat/ws   GET /channels/webchat/widget.js          │ │
│  │                                                                          │ │
│  │  Admin & Management                                                      │ │
│  │    DELETE /admin/memory/:id  POST /admin/skills/reload                  │ │
│  │    GET    /admin/channels/health                                         │ │
│  │    GET    /cron/jobs                                                     │ │
│  │    GET/POST /users/:id/preferences                                       │ │
│  │                                                                          │ │
│  │  OpenAI-Compatible                                                       │ │
│  │    POST /v1/chat/completions  (non-streaming + SSE)                     │ │
│  │                                                                          │ │
│  │  Test Channel                                                            │ │
│  │    POST /test/send  GET /test/responses                                  │ │
│  └────────────────────────────────────────────────────────────────────────┘ │
│                                                                              │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │  Gateway Core                                                           │ │
│  │  ┌─────────────┐  ┌──────────────────┐  ┌────────────────────────┐   │ │
│  │  │  Channel     │  │  Message Router  │  │  Hot Config            │   │ │
│  │  │  Registry    │  │  (tokio::mpsc)   │  │  (Arc<RwLock<...>>)   │   │ │
│  │  └─────────────┘  └──────────────────┘  └────────────────────────┘   │ │
│  └────────────────────────────────────────────────────────────────────────┘ │
│                                                                              │
│  ┌──────────────────────────────────────────────────────────────────┐       │
│  │  Channels (12)              │  Mode        │ Direction            │       │
│  │  Discord                    │  Gateway WS  │ bidirectional        │       │
│  │  WhatsApp                   │  Webhook     │ bidirectional        │       │
│  │  Telegram                   │  Webhook     │ bidirectional        │       │
│  │  Telegram Long-Poll         │  Long-poll   │ bidirectional        │       │
│  │  Slack                      │  Webhook     │ bidirectional        │       │
│  │  Slack Socket Mode          │  Socket WS   │ bidirectional        │       │
│  │  SMS / Twilio               │  Webhook     │ bidirectional        │       │
│  │  Microsoft Teams            │  Webhook     │ bidirectional        │       │
│  │  Email                      │  IMAP+SMTP   │ bidirectional        │       │
│  │  Webchat                    │  WebSocket   │ bidirectional        │       │
│  │  Generic Webhook            │  Webhook     │ inbound only         │       │
│  │  Test Channel               │  HTTP inject │ bidirectional        │       │
│  └──────────────────────────────────────────────────────────────────┘       │
│                                                                              │
│  ┌──────────────────────────────────────────────────────────────────┐       │
│  │  Message Pipeline (8 stages)                                      │       │
│  │  1. Deduplication (SHA-256 ring buffer)                           │       │
│  │  2. Audit log (NDJSON, async mpsc writer)                         │       │
│  │  3. Rate limiting (per-user sliding window, DashMap)              │       │
│  │  4. Attachment routing (PDF/image → agent prompt prefix)          │       │
│  │  5. Memory write (backend store)                                  │       │
│  │  6. Summarization (async, fire-and-forget)                        │       │
│  │  7. Agent processing (retry + jitter + fallback chain)            │       │
│  │  8. Response formatting + chunking + channel send                 │       │
│  └──────────────────────────────────────────────────────────────────┘       │
│                                                                              │
│  ┌────────────────────────┐   ┌──────────────────┐   ┌─────────────────┐  │
│  │  Agent Manager          │   │  Memory Backends │   │  Tool Registry  │  │
│  │  Per-user Conversational│   │  inmemory        │   │  calculator     │  │
│  │  Agents (agenkit-rust)  │   │  redis           │   │  formatter      │  │
│  │  7 LLM providers        │   │  sqlite          │   │  filesystem     │  │
│  │  Retry + jitter         │   │  postgres        │   │  web-search     │  │
│  │  Fallback chain         │   │  vector          │   │  web-fetch      │  │
│  │  Token streaming        │   │  (temporal decay)│   │  pdf-analysis   │  │
│  └────────────────────────┘   └──────────────────┘   │  image-analysis │  │
│                                                        │  shell          │  │
│  ┌────────────────────────┐   ┌──────────────────┐   │  calendar       │  │
│  │  MCP                   │   │  Skills          │   └─────────────────┘  │
│  │  Server (stdio)         │   │  SKILL.md files  │                        │
│  │  Client (stdio/http)    │   │  injected into   │   ┌─────────────────┐  │
│  └────────────────────────┘   │  system prompts  │   │  Cron Scheduler │  │
│                                └──────────────────┘   │  Synthetic msgs │  │
│                                                        └─────────────────┘  │
└─────────────────────────────────────────────────────────────────────────────┘
```

## Message Pipeline (8 Stages)

Every inbound message from any channel flows through `handle_message_inner()` in this order:

### Stage 1: Deduplication

`MessageDeduplicator` maintains a SHA-256 ring buffer of `(user_id, content_hash)` pairs. Duplicate messages within the configured window are dropped before any further processing.

- Controlled by: `gateway.deduplication.enabled` / `gateway.deduplication.window_size`
- When a message is dropped: silent discard, no agent call, no response

### Stage 2: Audit Log

`AuditLogger` writes a `message_received` event to the NDJSON audit log before any processing. This ensures all inbound messages are recorded even if they are later rate-limited or cause an error.

- Controlled by: `audit.enabled` / `audit.path`

### Stage 3: Rate Limiting

`RateLimiter` enforces a per-user sliding-window limit using `DashMap`. Users who exceed the limit receive a friendly warning message instead of an agent response; the pipeline exits here.

- Controlled by: `gateway.rate_limit.*`
- Emits: `rate_limit_hit` audit event + increments `rustynail_rate_limit_hits_total`

### Stage 4: Attachment Routing

When `gateway.auto_route_attachments` is enabled and the message contains `Attachment` structs:
- `pdf` attachment → prepends `"Please analyze this PDF: {url}"` to the agent prompt
- `image` attachment → prepends `"Please describe this image: {url}"` to the agent prompt

### Stage 5: Memory Write

The inbound message is written to the configured memory backend for this user. This happens before the agent call so the conversation context is always up to date.

### Stage 6: Summarization (async, fire-and-forget)

After writing to memory, `MemorySummarizer::maybe_summarize()` is called as a background task (`tokio::spawn`). It checks whether the history exceeds `summarization.trigger_at` (message count) or `summarization.trigger_token_budget` (token estimate). If triggered, it calls the configured LLM to generate a `[Summary: ...]` entry and replaces the oldest messages.

### Stage 7: Agent Processing

`AgentManager::process_message()` looks up or creates the per-user `ConversationalAgent` and calls the LLM. Retry and fallback logic runs here:

- **Retry**: exponential backoff with optional ±20% jitter; up to `agents.retry.max_attempts` attempts
- **Fallback chain**: on capacity/overload errors, `FallbackAgent` tries each `agents.fallback_providers` entry in order
- **Streaming**: `process_message_stream()` emits 5-byte chunks via mpsc channel (used by webchat WS and `/v1/chat/completions` SSE)

### Stage 8: Response Formatting + Chunking + Send

1. **`ResponseFormatter`**: converts markdown to platform-native syntax based on `channel_id` prefix:
   - Slack: `**bold**` → `*bold*`, links → `<url|text>`
   - Telegram: `**bold**` → `*bold*`, MarkdownV2 special-char escaping
   - WhatsApp: `**bold**` → `*bold*`, links → `text (url)`
   - Discord/Teams: pass-through
   - Code blocks are protected from inline substitution
2. **`MessageChunker`**: splits responses exceeding the platform character limit at whitespace boundaries (built-in defaults: Discord→2000, Slack→4000, Teams→1024, Telegram/WhatsApp→4096)
3. **Channel send**: each chunk is sent to the user's preferred channel via the channel's `send()` method

## Channel Adapter Pattern

Each channel implements the `Channel` trait:

```rust
#[async_trait]
pub trait Channel: Send + Sync {
    fn name(&self) -> &str;
    async fn start(&mut self, tx: mpsc::Sender<Message>) -> Result<()>;
    async fn send(&self, msg: &Message) -> Result<()>;
    async fn stop(&self) -> Result<()>;
    async fn health(&self) -> ChannelHealth;
}
```

**Webhook channels** (WhatsApp, Telegram, Slack, SMS, Teams, Generic, Test): registered with the gateway and receive messages via Axum handlers which push into an `mpsc::Sender<Message>`.

**Active channels** (Discord, Telegram long-poll, Slack Socket Mode, Email): spawn background tasks that independently receive messages and push them into the gateway's message sender.

## Agent Manager

`AgentManager` maintains a `DashMap<String, ConversationalAgent>` — one agent per `user_id`. Agents are created on first message and reused for all subsequent messages from the same user.

`create_llm()` builds the LLM provider from config:
1. Creates the primary provider
2. Wraps it with `FallbackAgent` if `fallback_providers` is configured
3. Applies retry logic to the resulting provider

`process_message_stream()` requires `Arc<Self>` and returns an `mpsc::Receiver<StreamEvent>` that yields `Token(String)` and `Done` variants. The webchat WS handler and OpenAI-compat SSE endpoint consume this channel.

## Memory Backends

All backends implement the `MemoryStore` trait:

```rust
#[async_trait]
pub trait MemoryStore: Send + Sync {
    async fn add_message(&self, user_id: &str, message: Message) -> Result<()>;
    async fn get_history(&self, user_id: &str) -> Result<Vec<Message>>;
    async fn clear(&self, user_id: &str) -> Result<()>;
}
```

| Backend | Implementation | Notes |
|---------|---------------|-------|
| `inmemory` | `DashMap<String, VecDeque<Message>>` | Lost on restart; default |
| `redis` | `redis` blocking client via `spawn_blocking` | TTL-based expiry |
| `sqlite` | `rusqlite` single-threaded runtime bridge | WAL mode; history trimmed on insert |
| `postgres` | `sqlx` async pool | Auto-creates `rustynail_messages` table |
| `vector` | agenkit `VectorMemory` + ring buffer | Temporal decay scoring + semantic search |

**Vector memory temporal decay**: each message is stored with a timestamp. `get_history()` returns messages sorted by `recency_weight = exp(-age_seconds / half_life)`. At `half_life` age, weight ≈ 0.5; at `2 × half_life`, weight ≈ 0.25.

## Hot-Reload (SIGHUP)

`HotConfig` wraps the subset of config that can be updated at runtime:

| Field | Notes |
|-------|-------|
| `log_level` | Affects new log records |
| `api_token` | Bearer auth reads from HotConfig on every request |
| `rate_limit.*` | Applied to subsequent windows; does not reset existing counters |
| `audit.*` | Enables/disables audit logging; path changes take effect immediately |

The bearer auth middleware holds an `Arc<RwLock<HotConfig>>` and acquires a read lock on every request — no restart needed to update the token.

Fields not in HotConfig (memory backend, channels, LLM provider, tool config) require a full restart.

**SIGHUP sequence:**
1. `main.rs` catches SIGHUP via `tokio::signal::unix`
2. Calls `Config::load()` to read the current config file / env vars
3. Calls `hot.write().await.apply(&new_cfg)` which returns a list of changed field names
4. Logs the changed fields (or "no hot-reloadable changes" if none)

## Observability

### Prometheus Metrics

All metrics are registered at startup and exposed at `GET /metrics`.

Key counters: `messages_in_total`, `messages_out_total`, `auth_failures_total`, `rate_limit_hits_total`, `llm_errors_total`, `llm_retries_total`, `tokens_in_total`, `tokens_out_total`

Key gauges: `active_users`, `healthy_channels`

Key histograms: `message_duration_seconds` (end-to-end pipeline latency)

### OpenTelemetry

When `otel.endpoint` is set, tracing spans are emitted to the OTLP gRPC endpoint:

- `gateway.handle_message` — spans the full pipeline execution
- `agent.process` — spans the LLM call including retries

### Audit Log

NDJSON events written by `AuditLogger` (async background writer via `mpsc::UnboundedSender`):

| Event | Trigger |
|-------|---------|
| `auth_rejected` | Bearer token mismatch |
| `rate_limit_hit` | User exceeds rate window |
| `message_received` | Every inbound message |
| `tool_executed` | After each tool call |
| `config_reloaded` | SIGHUP with changed fields |
| `agent_created` | New per-user agent instantiated |
| `llm_error` | LLM call fails (all attempts) |
| `AdminAction` | Any Admin API call |

### Dashboard WebSocket Push

`MessageStats` holds a `broadcast::Sender<DashboardEvent>`. On each inbound/outbound message, a `MessageEvent` is broadcast. The dashboard WS handler receives these and pushes `message_event` frames to all connected clients. A background task pushes `stats_update` frames every 5 seconds.

## Source Map

| Path | Purpose |
|------|---------|
| `src/main.rs` | CLI parsing, startup, SIGHUP handler, graceful shutdown |
| `src/lib.rs` | Library root, public re-exports |
| `src/types.rs` | `Message`, `Attachment`, `Channel`, `AgentResponse`, error enums |
| `src/config/mod.rs` | All config structs, env var loading, `Config::load()` |
| `src/gateway/mod.rs` | `Gateway`, `HotConfig`, `handle_message_inner()` |
| `src/gateway/http.rs` | All Axum route handlers, `AppState`, `HttpServerConfig` |
| `src/gateway/dashboard.rs` | `MessageStats`, `DashboardEvent`, WS push task |
| `src/gateway/rate_limiter.rs` | `RateLimiter` (DashMap sliding window) |
| `src/gateway/deduplicator.rs` | `MessageDeduplicator` (SHA-256 ring buffer) |
| `src/gateway/chunker.rs` | `MessageChunker` (platform character limits) |
| `src/gateway/formatter.rs` | `ResponseFormatter` (platform-native markdown) |
| `src/gateway/openai_compat.rs` | `/v1/chat/completions` handler (SSE streaming) |
| `src/gateway/user_prefs.rs` | Cross-channel routing preferences |
| `src/agents/manager.rs` | `AgentManager`, LLM provider factory, streaming |
| `src/agents/fallback.rs` | `FallbackAgent` (capacity-error fallback chain) |
| `src/memory/` | All `MemoryStore` implementations |
| `src/channels/` | All channel adapters |
| `src/tools/` | All tool implementations |
| `src/skills/mod.rs` | `SkillRegistry` (SKILL.md discovery) |
| `src/audit/mod.rs` | `AuditLogger`, `AuditEvent` |
| `src/cron/` | `CronScheduler` |
| `benches/` | Criterion benchmark suite |
| `tests/` | Integration tests |
| `deploy/` | Helm chart, Prometheus alerts, Grafana dashboard |
