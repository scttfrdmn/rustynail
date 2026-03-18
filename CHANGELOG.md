# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/scttfrdmn/rustynail/compare/v0.4.1...HEAD
[0.4.1]: https://github.com/scttfrdmn/rustynail/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/scttfrdmn/rustynail/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/scttfrdmn/rustynail/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/scttfrdmn/rustynail/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/scttfrdmn/rustynail/releases/tag/v0.1.0
