# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.10.0] - 2026-03-18

### Added
- PDF analysis tool (`pdf_analysis`): fetches or reads a PDF, base64-encodes it, and posts it to the Anthropic API as a `document` content block with the `pdfs-2024-09-25` beta header; configured via `tools.pdf_enabled` (env: `TOOLS_PDF_ENABLED`; default false); supports path or URL source, custom prompt, configurable `max_bytes` (default 32 MB)
- Image analysis tool (`image_analysis`): fetches or reads a jpeg/png/gif/webp image, detects media type from Content-Type header or file extension, base64-encodes it, and posts it to the Anthropic vision API; configured via `tools.image_enabled` (env: `TOOLS_IMAGE_ENABLED`; default false); supports path or URL source, custom prompt, configurable `max_bytes` (default 5 MB)
- Admin API endpoints under `/admin/*` (protected by bearer auth middleware): `DELETE /admin/memory/:user_id` clears a user's conversation history; `POST /admin/skills/reload` hot-reloads skills from disk without restart and returns `{"skills_loaded": N}`; `GET /admin/channels/health` returns per-channel health status with `health_detail` string for degraded/unhealthy channels
- Cron scheduler (`CronScheduler` in `src/cron/`): fires synthetic messages on configurable intervals (suffix: `s`, `m`, `h`, `d`); configured via `cron.jobs` YAML list (name, schedule, message, channel_id, user_id, enabled); invalid schedule strings log a warning and are skipped; `GET /cron/jobs` returns a snapshot of all job statuses
- WebSocket origin restriction: `gateway.allowed_ws_origins` config (env: `GATEWAY_ALLOWED_WS_ORIGINS`, comma-separated); when non-empty, both `/dashboard/ws` and `/channels/webchat/ws` upgrade handlers return `403 Forbidden` for unlisted origins; empty list allows all (backward compatible)
- Configurable shutdown timeout: `gateway.shutdown_timeout_seconds` (env: `GATEWAY_SHUTDOWN_TIMEOUT_SECONDS`; default 30); `gateway.stop()` is wrapped with `tokio::time::timeout`; logs a warning if exceeded; displayed in `rustynail config check` output
- `AgentManager::reload_skills_context()` async method to replace the active skills context without restart; `skills_context` field changed from `Option<String>` to `Arc<RwLock<Option<String>>>` to enable concurrent hot-reload

### Changed
- `Cargo.toml` bumped to `0.10.0`
- `ToolsConfig` extended with `pdf_enabled: bool` and `image_enabled: bool`
- `GatewayConfig` extended with `allowed_ws_origins: Vec<String>` and `shutdown_timeout_seconds: u64`
- `Config` extended with top-level `cron: CronConfig` (default empty)
- `AppState` and `HttpServerConfig` extended with `skills_config`, `cron_jobs`, and `allowed_ws_origins` fields
- `dashboard_ws_handler` and `webchat_ws_handler` return type changed to `Response` to support conditional 403 on origin mismatch
- Router gains `delete` import; new routes: `DELETE /admin/memory/:user_id`, `POST /admin/skills/reload`, `GET /admin/channels/health`, `GET /cron/jobs`
- `rustynail config check` now prints WS origins, shutdown timeout, cron job count, PDF tool state, and image tool state

## [0.9.0] - 2026-03-18

### Added
- Per-user sliding-window rate limiting: `RateLimiter` in `src/gateway/rate_limiter.rs` using `DashMap`; configured via `gateway.rate_limit.enabled`, `messages_per_window`, `window_seconds` (env: `RATE_LIMIT_ENABLED`, `RATE_LIMIT_MESSAGES`, `RATE_LIMIT_WINDOW_SECONDS`); off by default (backward compatible); users who exceed the limit receive a friendly warning message
- Structured NDJSON audit logging: `AuditLogger` + `AuditEvent` in `src/audit/mod.rs`; async background writer via `mpsc::UnboundedSender`; events: `auth_rejected`, `rate_limit_hit`, `message_received`, `tool_executed`, `config_reloaded`, `agent_created`, `llm_error`; configured via `audit.enabled`, `audit.path` (env: `AUDIT_ENABLED`, `AUDIT_PATH`); off by default
- Request body size limit: `DefaultBodyLimit::max()` applied globally (WebSocket upgrade routes exempt); configured via `gateway.max_body_bytes` (env: `GATEWAY_MAX_BODY_BYTES`; default 1 MB); returns `413 Payload Too Large` when exceeded
- Handler timeout: `TimeoutLayer` wraps all routes via `HandleErrorLayer`; configured via `gateway.request_timeout_seconds` (env: `GATEWAY_REQUEST_TIMEOUT_SECONDS`; default 30s); returns `408 Request Timeout`
- Security Prometheus counters: `rustynail_auth_failures_total`, `rustynail_rate_limit_hits_total`, `rustynail_llm_errors_total`, `rustynail_llm_retries_total`; exposed via `/metrics`; corresponding `record_*` methods on `MessageStats`
- Config hot-reload via SIGHUP: `HotConfig` struct wraps hot-reloadable config subset (`log_level`, `api_token`, `rate_limit.*`, `audit.*`); `Gateway::hot_config_handle()` returns an `Arc<RwLock<HotConfig>>`; bearer auth middleware reads token from `HotConfig` at request time; SIGHUP handler in `main.rs` reloads config and logs changed field names (Unix only)
- LLM retry with exponential backoff in `AgentManager::process_message()`: configurable via `agents.retry.enabled`, `max_attempts`, `base_delay_ms` (env: `AGENTS_RETRY_ENABLED`, `AGENTS_RETRY_MAX_ATTEMPTS`, `AGENTS_RETRY_BASE_DELAY_MS`; defaults: enabled, 3 attempts, 100 ms base); each retry increments `rustynail_llm_retries_total`; after all attempts exhausted, a friendly fallback message is returned to the user

### Changed
- `Cargo.toml` bumped to `0.9.0`
- `GatewayConfig` extended with `rate_limit: RateLimitConfig`, `max_body_bytes: usize`, `request_timeout_seconds: u64`
- `AgentsConfig` extended with `retry: AgentRetryConfig`
- `Config` extended with top-level `audit: AuditConfig`
- `AppState` and `HttpServerConfig` extended with `rate_limiter`, `audit`, `hot_config` fields
- `create_router()` now accepts `max_body_bytes` and `request_timeout_seconds` parameters
- `AgentManager` constructor chain extended with `with_tools_skills_and_stats()` accepting optional `Arc<MessageStats>` for retry metrics
- Bearer auth middleware reads expected token from `HotConfig` (hot-reloadable) instead of static `AppState.api_token`

## [0.8.0] - 2026-03-18

### Added
- Gateway API bearer token authentication: `GatewayConfig.api_token` (env: `GATEWAY_API_TOKEN`); Axum middleware uses `subtle::ConstantTimeEq` for timing-safe comparison; `/live` and `/ready` always exempt (K8s probes); disabled when token is absent (backward compatible)
- Token/cost accounting: `tokens_in` and `tokens_out` atomic counters on `MessageStats`; `record_tokens(input, output)` called after each LLM completion; exposed via `/metrics` as `rustynail_tokens_in_total` / `rustynail_tokens_out_total` and `/dashboard/data` JSON
- Web fetch tool (`web_fetch`): fetches a URL with 15s timeout via `reqwest`; HTML stripped via `scraper` crate (skips `<script>`, `<style>`, `<noscript>`); respects `max_bytes` parameter (default 512 KB); registered when `tools.enabled = true`
- Shell execution tool (`shell`): executes commands via `sh -c` with `tokio::process::Command`; two-step approval gate (returns `"Pending approval: ..."` unless `approved=true`); optional `allowed_commands` prefix allowlist; configurable via `tools.shell.enabled`, `tools.shell.require_approval`, `tools.shell.allowed_commands`
- Agent skills system: `SkillRegistry` in `src/skills/mod.rs` discovers `SKILL.md` files from configured paths; selected skills injected into agent system prompts (up to `skills.max_active`); configured via `skills.enabled`, `skills.paths`, `skills.max_active` (env: `SKILLS_ENABLED`, `SKILLS_PATHS`, `SKILLS_MAX_ACTIVE`)
- Bundled skills: `skills/rustynail-assistant/SKILL.md` (surfaces available channels and tools) and `skills/formatting/SKILL.md` (channel-aware output formatting guidance)
- Microsoft Teams channel (`TeamsChannel`): Bot Framework Activity webhook at `POST /channels/teams/messages`; OAuth2 client credentials token cache with 60s pre-refresh; outbound send to `{serviceUrl}/v3/conversations/{id}/activities/{activityId}`; configured via `TEAMS_APP_ID`, `TEAMS_APP_PASSWORD`
- Zero-credential test harness: `StubAgent` (echo mode or fixed response, selected by `llm_provider = "stub"`); `TestChannel` with `POST /test/send` inject and `GET /test/responses` drain endpoints; `configs/harness.yaml` minimal config; integration tests in `tests/harness/` (skip unless `HARNESS_URL` is set)
- Helm chart at `deploy/helm/rustynail/`: Chart.yaml, values.yaml, deployment, service, configmap, secret, ingress, HPA, service account, helpers, NOTES.txt; liveness → `/live`, readiness → `/ready`; optional Redis subchart
- Criterion benchmark suite in `benches/gateway_benchmarks.rs`: `bench_inmemory_store_add`, `bench_inmemory_store_get`, `bench_config_load`, `bench_message_stats_record`
- `config check` prints `Gateway auth: enabled/disabled` and `Skills: enabled (N paths, M skills loaded) / disabled`
- `stub` provider option for `agents.llm_provider` in `AgentManager`

### Changed
- `Cargo.toml` bumped to `0.8.0`; added `scraper = "0.19"` (HTML stripping), `subtle = "2"` (constant-time comparison), `criterion = "0.5"` (dev dep for benchmarks)
- `GatewayConfig` extended with `api_token: Option<String>`
- `ChannelsConfig` extended with `teams: Option<TeamsConfig>` and `test_channel: bool`
- `ToolsConfig` extended with `shell: ShellToolConfig`
- `Config` extended with `skills: SkillsConfig`
- `AgentManager` gains `skills_context: Option<String>` field; new constructor `with_tools_and_skills()`; skills context appended to system prompt when set
- `HttpServerConfig` and `AppState` extended with `teams_tx`, `api_token`, `test_channel` fields
- `DashboardData` extended with `tokens_in` and `tokens_out` fields
- Gateway `start()` registers Teams channel, test channel, web fetch tool, and shell tool

## [0.7.0] - 2026-03-18

### Added
- MCP (Model Context Protocol) support via agenkit 0.82.0
- `rustynail mcp serve` subcommand: exposes RustyNail's registered tools (calculator, formatter, filesystem, web search, calendar) as an MCP server over stdio, compatible with Claude Code, Cursor, and any MCP client
- MCP client connectivity in gateway `start()`: configure `mcp.servers` in YAML to connect to external MCP servers at startup and register their tools into the agent tool registry; supports both `stdio` (subprocess) and `http` transports; gracefully skips misconfigured or unreachable servers with an error log
- `McpConfig` and `McpServerEntry` structs in config: `mcp.servers` list with `name`, `transport`, `command`, `args`, `env` (stdio), and `url` (http) fields

## [0.6.0] - 2026-03-18

### Added
- Shell completion generation via `clap_complete`; `rustynail completions <bash|zsh|fish|powershell|elvish>` prints the completion script for the requested shell
- Grafana dashboard (`deploy/grafana/dashboard.json`) with panels for messages_in/out rate, active_users, healthy_channels, p50/p95/p99 latency histogram, and active users over time; auto-load provisioning config at `deploy/grafana/provisioning/dashboards/rustynail.yml`
- Prometheus alert rules (`deploy/prometheus/alerts.yaml`): `RustyNailDown`, `HighMessageLatency`, `ChannelUnhealthy`, `HighErrorRate`, `NoActiveUsers`
- SQLite memory backend (`SqliteStore`) implementing `MemoryStore`; configured via `SQLITE_PATH` env var or `memory.sqlite_path` YAML key; uses a dedicated single-threaded tokio runtime to bridge the sync trait; history trimmed to `max_history` on every insert
- PostgreSQL memory backend (`PostgresStore`) implementing `MemoryStore`; configured via `DATABASE_URL` env var or `memory.postgres_url` YAML key; creates `rustynail_messages` table with `(user_id, ts)` index automatically
- Vector memory backend (`VectorMemoryStore`) implementing `MemoryStore`; wraps agenkit's `VectorMemory` with an in-process `SimpleEmbeddingProvider` (64-dim character bigram); maintains a secondary ring buffer for ordered `get_history()` access; semantic search available via underlying VectorMemory
- Memory summarization (`MemorySummarizer`); fires asynchronously when history exceeds `summarization.trigger_at` (default 40); replaces oldest `(len - keep_recent)` messages with a `[Summary: ...]` entry via the configured LLM; configured via `SUMMARIZATION_ENABLED`, `SUMMARIZATION_TRIGGER_AT`, `SUMMARIZATION_KEEP_RECENT`, `SUMMARIZATION_MODEL` env vars
- Multi-LLM provider support in `AgentManager::create_agent()`; `llm_provider` can now be `"anthropic"` (default), `"openai"`, `"ollama"`, `"gemini"`, `"bedrock"`, `"litellm"`, or `"openai-compat"`; configured via `LLM_PROVIDER` env var or `agents.llm_provider` YAML key
- AWS Bedrock support: `aws_region` field on `AgentsConfig` (env: `AWS_REGION`; default `us-east-1`)
- SMS channel (`SmsChannel`) via Twilio: webhook receive at `POST /webhooks/sms` (TwiML response), outbound send via Twilio Messages REST API; configured via `TWILIO_ACCOUNT_SID`, `TWILIO_AUTH_TOKEN`, `TWILIO_FROM_NUMBER`
- Generic inbound webhook channel (`WebhookChannel`): `POST /webhooks/:name` matches against `channels.webhook.endpoints` config; HMAC-SHA256 verification if `secret` set; JSONPath text extraction via `jsonpath-rust`; configured via YAML `channels.webhook.endpoints`
- Web chat widget channel (`WebchatChannel`): WebSocket endpoint at `GET /channels/webchat/ws?session_id=<uuid>`; static auto-reconnecting vanilla JS widget at `GET /channels/webchat/widget.js` (~3KB, no dependencies); per-session routing via `DashMap`; configured via `WEBCHAT_ENABLED`, `WEBCHAT_ALLOWED_ORIGINS`, `WEBCHAT_WELCOME_MESSAGE`
- Email channel (`EmailChannel`): IMAP polling receive (sync `imap` crate via `spawn_blocking`, 30-second poll interval, `~` home dir expansion, HTML/quoted-text stripping); SMTP send via `lettre` with tokio1; configured via `EMAIL_IMAP_HOST`, `EMAIL_SMTP_HOST`, `EMAIL_USERNAME`, `EMAIL_PASSWORD`, and optional `EMAIL_IMAP_PORT`, `EMAIL_SMTP_PORT`, `EMAIL_INBOX`, `EMAIL_FROM_ADDRESS`
- Slack Socket Mode channel (`SlackSocketModeChannel`): self-connecting WebSocket via `tokio-tungstenite`; calls `apps.connections.open` to get WSS URL; handles `hello`, `events_api` (with envelope ack), and `disconnect` frames with exponential backoff reconnection; configured via `SLACK_APP_TOKEN` + `SLACK_MODE=socket`
- `config check` now prints `llm_provider` and summarization status

### Changed
- `ChannelsConfig` extended with `sms`, `webhook`, `webchat`, and `email` optional fields
- `SlackConfig` extended with `app_token: Option<String>` and `mode: String` (default `"webhook"`)
- `MemoryConfig` extended with `sqlite_path`, `postgres_url`, `vector_store`, `vector_store_url`, `embedding_provider`, `embedding_model`, and `summarization` fields
- `AgentsConfig` extended with `aws_region: Option<String>`
- Gateway `start()` now wires SMS, webhook, webchat, email, and Slack Socket Mode channels
- HTTP `AppState` and `HttpServerConfig` extended with `sms_tx`, `sms_auth_token`, `webhook_endpoints`, `webhook_tx`, `webchat_sessions`, `webchat_tx`
- `handle_message_inner` now calls `MemorySummarizer::maybe_summarize` (fire-and-forget) after adding the user message

## [0.5.0] - 2026-03-18

### Added
- Real Prometheus `/metrics` endpoint replacing the hand-rolled JSON response; exposes `rustynail_messages_in_total`, `rustynail_messages_out_total`, `rustynail_active_users`, `rustynail_healthy_channels`, and `rustynail_message_duration_seconds` (histogram with default buckets); content-type `text/plain; version=0.0.4`
- Redis memory store (`RedisStore`) implementing `MemoryStore` via `redis` blocking client; configured via `REDIS_URL` + `REDIS_TTL_SECONDS` env vars or `memory.redis_url` + `memory.redis_ttl_seconds` YAML keys; graceful fallback to in-memory on connection failure
- `MemoryConfig` section in config (`memory.backend`, `memory.redis_url`, `memory.redis_ttl_seconds`); `MEMORY_BACKEND=redis` switches backends at runtime
- Telegram long-poll mode (`TELEGRAM_MODE=longpoll` or `channels.telegram.mode: longpoll`); `TelegramLongPollChannel` spawns a `getUpdates?timeout=30` loop with automatic offset tracking and 5-second backoff on error
- Calendar tool (`CalendarTool`): `create`, `list`, `get`, `delete`, `upcoming` operations backed by a local JSON file in `RUSTYNAIL_DATA_DIR` (default `~/.rustynail/calendar.json`)
- Message formatter tool (`FormatterTool`): pure-Rust `to_markdown`, `to_plain`, `truncate`, `wrap`, `summarize_header` operations; useful for adapting content across Discord/WhatsApp/Slack formatting conventions
- Dashboard live WebSocket at `GET /dashboard/ws`; streams `stats_update` (every 5 s) and `message_event` (on each inbound/outbound message) JSON payloads; dashboard HTML updated with auto-reconnecting WebSocket JS block
- CLI subcommands via `clap` derive macros: `rustynail start` (default), `rustynail status [--port N]`, `rustynail version`, `rustynail config check`
- Message processing duration fed into the `rustynail_message_duration_seconds` Prometheus histogram in `handle_message_inner`

### Changed
- `Dockerfile` runtime stage switched from `debian:bookworm-slim` to `gcr.io/distroless/cc-debian12` (no shell, no package manager); CA certificates copied explicitly from builder stage; image now runs as distroless `nonroot` user (uid 65532)
- `axum` dependency updated to enable the `ws` feature for WebSocket support
- `MessageStats` extended with Prometheus metric handles and a `broadcast::Sender<DashboardEvent>`; `record_inbound_async` and `record_outbound_async` now also increment Prometheus counters and broadcast `MessageEvent`

## [0.4.5] - 2026-03-18

### Added
- Integration test suite: 25 new tests covering all HTTP endpoints (`/health`, `/status`, `/metrics`, `/ready`, `/live`), all three webhook paths (WhatsApp, Telegram, Slack), dashboard endpoints with auth, user preferences, and a full inbound→mock-Anthropic→outbound message-flow pipeline
- `tests/common/mod.rs`: shared test helpers (`make_test_state`, `make_test_state_with_webhooks`, `RecordingChannel`) and `tests/fixtures/test_config.yaml`
- `rustynail::gateway::handle_message_for_test` public entry point for integration tests
- `api_base: Option<String>` field on `AgentsConfig`; loaded from `ANTHROPIC_API_BASE` env var; overrides production Anthropic URL in both `ConversationalAgent` and `PlanningAgent` constructions

### Changed
- `DISCORD_BOT_TOKEN` is now optional in `Config::load()` — absent means no Discord channel; all other channels remain optional; only `ANTHROPIC_API_KEY` is required for a production start
- `config.yaml.example`: added commented `api_base` field under `agents`
- CI workflow: `cargo test` step now passes `ANTHROPIC_API_KEY=test_unused` so Tier 1 integration tests run in CI without secrets

## [0.4.1] - 2026-03-17

### Fixed
- `DiscordChannel::health()` always returned `ChannelHealth::Healthy`; it now reads the actual stored health state via `blocking_read()`

### Changed
- `DiscordChannel::message()` handler no longer clones the `Message` before passing it to `mpsc::send()` (needless allocation)
- `parse_update()` in `telegram.rs` simplifies `text.as_ref()?.clone()` to the idiomatic `text.clone()?`
- `ToolRegistry` now derives `Default`; `new()` delegates to `Self::default()`
- `MessageStats` fields (`messages_in`, `messages_out`, `start_instant`, `start_time`) are now private; public accessor methods `messages_in()`, `messages_out()`, and `start_time()` replace direct field access
- Duplicate `ChannelStatus` struct in `dashboard.rs` removed; the module now re-uses `http::ChannelStatus`
- `WebSearchTool::execute()` no longer allocates a `String` for the `search_depth` parameter (kept as `&str`)
- `test_success_response` in `web_search.rs` replaced with `test_response_parsing` that directly tests the JSON-parsing logic without a dead mockito server
- Channel adapters (discord, whatsapp, telegram, slack) use named `const NAME` rather than inline string literals in `name()` implementations
- WhatsApp Graph API version extracted to `const GRAPH_API_VERSION` in `whatsapp.rs`
- Agent system prompt extracted to `const SYSTEM_PROMPT` in `agents/manager.rs`
- Doc comments added to `HttpServerConfig`, `AppState`, `MessageStats`, `DashboardData`, `RecentMessage`, `ToolRegistry`, `CalculatorTool`, `FileSystemTool`, `WebSearchTool`

## [0.4.0] - 2026-03-17

### Added
- Multi-stage `Dockerfile` (`rust:1.82-slim-bookworm` builder → `debian:bookworm-slim` runtime) for producing a minimal production image
- `docker-compose.yml` for local development; build context is the parent directory to resolve the local `agenkit` path dependency
- `.dockerignore` excluding `target/`, `.env`, `.git/`, and `diary/` from the build context
- GitHub Actions CI workflow (`.github/workflows/ci.yml`): runs `cargo fmt --check`, `cargo clippy -D warnings`, and `cargo test` on push/PR to `main`; dual checkout of `rustynail` + `agenkit` sibling repos
- GitHub Actions Docker workflow (`.github/workflows/docker.yml`): builds and pushes to `ghcr.io/scttfrdmn/rustynail` on `v*` tag push
- Web monitoring dashboard at `GET /dashboard` (embedded HTML/CSS/JS, no CDN) with 30-second auto-refresh
- `GET /dashboard/data` JSON endpoint returning version, uptime, message counters, active users, channel health, and a recent-messages ring buffer (last 50)
- `MessageStats` struct with atomic counters (`messages_in`, `messages_out`) and a `RwLock<VecDeque<RecentMessage>>` ring buffer; threaded through the gateway message loop and `handle_message_inner`
- Optional HTTP basic auth on dashboard endpoints via `DASHBOARD_AUTH_PASSWORD` env var; credentials are `rustynail:<password>`
- `DashboardConfig` struct in the configuration system with `DASHBOARD_AUTH_PASSWORD` env var support
- `HttpServerConfig` struct replacing the 10-positional-argument `start_http_server` signature

## [0.3.0] - 2026-03-17

### Added
- Telegram Bot API channel adapter: webhook receive (POST with `X-Telegram-Bot-Api-Secret-Token` auth) and `sendMessage` REST send
- Slack Events API channel adapter: webhook receive (HMAC-SHA256 signature verification, `url_verification` challenge handling) and `chat.postMessage` REST send
- Web search tool via Tavily API (`web_search`): `query`, `max_results`, and `search_depth` parameters; registered when `TAVILY_API_KEY` is set
- Planning agent (`/plan <task>` prefix routes to `PlanningAgent`); activated by `agents.planning_enabled = true` / `AGENTS_PLANNING_ENABLED=true`
- Optional OpenTelemetry distributed tracing via OTLP exporter; enabled by `OTEL_EXPORTER_OTLP_ENDPOINT`; `gateway.handle_message` and `agent.process` spans emitted

## [0.2.0] - 2026-03-17

### Added
- Tool registry system backed by agenkit `Tool` trait; agents upgraded to `ReActAgent` when tools are registered and `tools.enabled = true`
- Calculator tool: add, sub, mul, div, pow, sqrt, abs, floor, ceil, round operations
- FileSystem tool: read, write, list, exists — sandboxed to a configured root path (rejects path traversal)
- WhatsApp Cloud API channel adapter: webhook receive (GET verify + POST events) and REST send via Graph API v18.0
- Cross-channel message routing: per-user preferred response channel via `POST /users/:id/preferences`; query via `GET /users/:id/preferences`
- Gateway now owns its internal message channel; `message_sender()` method for external channel adapters (Discord, etc.)
- `ToolsConfig` and `WhatsAppConfig` added to the configuration system (env vars + YAML)

## [0.1.0] - 2026-02-01

### Added
- Multi-channel architecture with pluggable `Channel` trait
- Discord integration using Serenity library (message receive/send, attachments, health monitoring)
- Agenkit-Rust integration with Claude 3.5 Sonnet via `AnthropicAgent` adapter
- `ConversationalAgent` wrapper for per-user conversation history management
- `AgentManager` for per-user agent lifecycle (automatic creation, thread-safe, isolated history)
- HTTP server with 5 production endpoints via Axum:
  - `GET /health` — basic health check for load balancers
  - `GET /status` — detailed system status with channel health and active user count
  - `GET /metrics` — operational metrics (Prometheus-compatible)
  - `GET /ready` — Kubernetes readiness probe (503 when not ready)
  - `GET /live` — Kubernetes liveness probe
- `MemoryStore` trait with `InMemoryStore` implementation (per-user history, auto-trimming)
- Configuration system with YAML file and environment variable support, sensible defaults
- Gateway core: channel registry, message router, event bus (tokio broadcast), graceful shutdown
- Structured logging with `tracing` and `tracing-subscriber`
- README with architecture diagrams, quick start, and HTTP endpoint documentation

[Unreleased]: https://github.com/scttfrdmn/rustynail/compare/v0.7.0...HEAD
[0.7.0]: https://github.com/scttfrdmn/rustynail/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/scttfrdmn/rustynail/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/scttfrdmn/rustynail/compare/v0.4.5...v0.5.0
[0.4.5]: https://github.com/scttfrdmn/rustynail/compare/v0.4.1...v0.4.5
[0.4.1]: https://github.com/scttfrdmn/rustynail/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/scttfrdmn/rustynail/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/scttfrdmn/rustynail/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/scttfrdmn/rustynail/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/scttfrdmn/rustynail/releases/tag/v0.1.0
