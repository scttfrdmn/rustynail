# RustyNail 🦀🔨

**"Rust Never Sleeps!"**

[![Version](https://img.shields.io/badge/version-0.15.0-blue)](https://github.com/scttfrdmn/rustynail/releases/tag/v0.15.0)
[![Rust](https://img.shields.io/badge/rust-1.94%2B-orange)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache%202.0-green)](LICENSE)
[![Status](https://img.shields.io/badge/status-beta-yellow)](https://github.com/scttfrdmn/rustynail)
[![CI](https://github.com/scttfrdmn/rustynail/actions/workflows/ci.yml/badge.svg)](https://github.com/scttfrdmn/rustynail/actions/workflows/ci.yml)

RustyNail is a high-performance AI gateway built with Rust. It connects 12 messaging platforms to multiple LLM providers through a single statically-linked binary. Users interact naturally through their chat platform of choice; RustyNail handles routing, memory, tools, and response formatting.

## What Is RustyNail?

RustyNail is a single-binary Rust AI gateway that:

- Routes messages from **12 channels** (Discord, WhatsApp, Telegram, Slack, SMS, Teams, Email, Webchat, and more) to an AI backend
- Supports **7 LLM providers** (Anthropic, OpenAI, Ollama, Gemini, AWS Bedrock, LiteLLM, OpenAI-compat)
- Maintains **per-user conversation memory** across 5 backends (in-memory, Redis, SQLite, Postgres, vector)
- Runs a **tool registry** (calculator, web search, web fetch, filesystem, PDF/image analysis, shell, calendar)
- Exposes an **Admin API**, **cron scheduler**, **agent skills**, and an **OpenAI-compatible `/v1/chat/completions`** endpoint
- Ships as a **distroless Docker image** (~8 MB) with <30 MB RAM idle and <1 ms gateway overhead

Sister project: [BuckTooth](https://github.com/scttfrdmn/bucktooth) (Go implementation). Reference implementation: [OpenClaw](https://github.com/scttfrdmn/openclaw).

## Features

### Channels (12)

| Channel | Mode | Auth |
|---------|------|------|
| Discord | Gateway (serenity) | Bot token |
| WhatsApp | Webhook (Meta Cloud API) | Phone number ID + access token |
| Telegram | Webhook | Bot token + secret header |
| Telegram | Long-poll | Bot token (no public URL) |
| Slack | Events API webhook | Signing secret |
| Slack | Socket Mode | App-level token (`xapp-`) |
| SMS / Twilio | TwiML webhook | Account SID + auth token |
| Microsoft Teams | Bot Framework webhook | App ID + password + optional HMAC |
| Email | IMAP + SMTP | Host + credentials |
| Webchat | WebSocket | Optional CORS origins |
| Generic Webhook | HTTP POST | Optional HMAC-SHA256 |
| Test Channel | HTTP inject/drain | None (dev/test only) |

See [docs/channels.md](docs/channels.md) for per-channel setup instructions.

### LLM Providers (7)

`anthropic` · `openai` · `ollama` · `gemini` · `bedrock` · `litellm` · `openai-compat`

Configurable retry with exponential backoff + jitter, and a provider fallback chain for capacity errors.

### Memory Backends (5)

`inmemory` · `redis` · `sqlite` · `postgres` · `vector` (temporal decay + half-life scoring)

Automatic conversation summarization when history exceeds a message count or token budget threshold.

### Tools

calculator · web search (Tavily) · web fetch · filesystem · PDF analysis · image analysis · shell · calendar · formatter

### Production Features

- **MCP**: expose tools via `rustynail mcp serve` (stdio); consume external MCP servers
- **Admin API**: clear user memory, reload skills, inspect channel health
- **Cron scheduler**: fire synthetic messages on configurable intervals
- **Agent skills**: inject SKILL.md context files into agent system prompts
- **OpenAI-compatible endpoint**: `POST /v1/chat/completions` (non-streaming + SSE), with a `stateless` mode that reports real provider token counts and cost
- **Prometheus metrics** + **OpenTelemetry tracing** + **Grafana dashboard config**
- **Web dashboard** with WebSocket live updates
- **SIGHUP hot-reload**: update log level, API token, rate limits, audit config without restart
- **Bearer token auth**, per-user rate limiting, request body limits, handler timeouts
- **Structured NDJSON audit log**

## Quick Start

**Prerequisites:** Rust 1.94+, an Anthropic API key (or other provider).

RustyNail depends on [agenkit](https://github.com/scttfrdmn/agenkit) as a local
path dependency at `../agenkit/agenkit-rust`, so it must be cloned as a sibling
directory. Without it `cargo build` fails immediately.

```bash
# 1. Clone both repos side by side, pinning agenkit to the tested release
git clone https://github.com/scttfrdmn/agenkit.git
git -C agenkit checkout v0.87.0
git clone https://github.com/scttfrdmn/rustynail.git

# Layout must be:
#   parent/
#   ├── agenkit/agenkit-rust/
#   └── rustynail/

# 2. Build
cd rustynail
cargo build --release

# 3. Set the required environment variable
export ANTHROPIC_API_KEY=sk-ant-...

# 4. Start (env-vars only — no channels except test channel)
./target/release/rustynail

# 5. Or start with a config file
CONFIG_FILE=config.yaml ./target/release/rustynail

# 6. Verify
curl http://localhost:8080/health
# {"status":"ok","version":"0.15.0"}
```

**Minimal `config.yaml`:**

```yaml
gateway:
  http_port: 8080
  websocket_port: 18789
  log_level: info

agents:
  llm_provider: anthropic
  llm_model: claude-3-5-sonnet-20241022
  api_key: ${ANTHROPIC_API_KEY}
  max_history: 20
```

See [docs/configuration.md](docs/configuration.md) for the complete configuration reference.

## Channels

See [docs/channels.md](docs/channels.md) for per-channel prerequisites, credential steps, config snippets, and webhook verification.

## Deployment

```bash
# Docker (pre-built)
docker pull ghcr.io/scttfrdmn/rustynail:latest
docker run --rm -e ANTHROPIC_API_KEY=... -p 8080:8080 ghcr.io/scttfrdmn/rustynail:latest

# Docker Compose (build context must be the parent directory for agenkit)
cd ..
ANTHROPIC_API_KEY=... docker-compose -f rustynail/docker-compose.yml up
```

See [docs/deployment.md](docs/deployment.md) for the Docker Compose setup, Helm/Kubernetes runbook, environment variable cheat sheet, Prometheus/Grafana integration, and production checklist.

## HTTP API

| Group | Routes |
|-------|--------|
| Health & probes | `GET /health` `/ready` `/live` `/status` `/metrics` |
| Dashboard | `GET /dashboard` `/dashboard/data` `GET /dashboard/ws` (WebSocket) |
| Channel webhooks | `POST /webhooks/whatsapp` `/webhooks/telegram` `/webhooks/slack` `/webhooks/sms` `/webhooks/teams` `/webhooks/:name` |
| Webchat | `GET /channels/webchat/ws` (WebSocket) `/channels/webchat/widget.js` |
| Admin | `DELETE /admin/memory/:user_id` `POST /admin/skills/reload` `GET /admin/channels/health` |
| Cron | `GET /cron/jobs` |
| User preferences | `GET /users/:id/preferences` `POST /users/:id/preferences` |
| OpenAI-compat | `POST /v1/chat/completions` |
| Test channel | `POST /test/send` `GET /test/responses` |

See [docs/api.md](docs/api.md) for the complete HTTP API reference including request/response schemas.

## CLI

```
rustynail start                     # Start the gateway (default)
rustynail status [--port N]         # Query a running instance
rustynail version                   # Print version and build info
rustynail config check              # Load config and print summary
rustynail config validate           # Preflight checks; exits 0/1
rustynail completions <shell>       # Print shell completion script
rustynail mcp serve                 # Expose tools as MCP server (stdio)
```

See [docs/cli.md](docs/cli.md) for the full CLI reference.

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────┐
│  RustyNail Gateway (Single Binary)                                       │
│                                                                          │
│  ┌──────────────────────────────────────────────────────────────────┐  │
│  │  HTTP Server (Axum :8080)                                         │  │
│  │  /health /ready /live /status /metrics  ← Health & observability  │  │
│  │  /dashboard /dashboard/ws               ← Web dashboard (WS push) │  │
│  │  /webhooks/*  /channels/webchat/ws      ← Inbound channel hooks   │  │
│  │  /admin/*  /cron/*  /users/*            ← Admin & management      │  │
│  │  /v1/chat/completions                   ← OpenAI-compat SSE       │  │
│  └──────────────────────────────────────────────────────────────────┘  │
│                                                                          │
│  ┌──────────────────────────────────────────────────────────────────┐  │
│  │  Message Pipeline                                                  │  │
│  │  Dedup → Audit → Rate limit → Attachment route →                  │  │
│  │  Memory write → Summarize (async) → Agent (retry+fallback) →      │  │
│  │  Format → Chunk → Channel send                                    │  │
│  └──────────────────────────────────────────────────────────────────┘  │
│                                                                          │
│  ┌────────────┐  ┌──────────────────┐  ┌───────────────────────────┐  │
│  │  Channels  │  │  Agent Manager   │  │  Memory Backends           │  │
│  │  Discord   │  │  Per-user        │  │  inmemory / redis          │  │
│  │  WhatsApp  │  │  Conversational  │  │  sqlite / postgres         │  │
│  │  Telegram  │  │  Agents          │  │  vector (temporal decay)   │  │
│  │  Slack     │  │  Multi-LLM       │  └───────────────────────────┘  │
│  │  SMS       │  │  7 providers     │                                   │
│  │  Teams     │  └──────────────────┘  ┌───────────────────────────┐  │
│  │  Email     │                         │  Tool Registry             │  │
│  │  Webchat   │  ┌──────────────────┐  │  calculator / web-search   │  │
│  │  Webhook   │  │  MCP             │  │  web-fetch / filesystem    │  │
│  │  + Test    │  │  Server (stdio)  │  │  pdf / image / shell       │  │
│  └────────────┘  │  Client          │  │  calendar / formatter      │  │
│                  └──────────────────┘  └───────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────────┘
```

See [docs/architecture.md](docs/architecture.md) for a deep-dive into the message pipeline, channel adapter pattern, memory backend internals, hot-reload mechanics, and observability coverage.

## Development

```bash
cargo build           # Debug build
cargo build --release # Release build
cargo test            # Run all 200 tests
cargo test -- --nocapture  # With output
cargo clippy          # Lint
cargo fmt             # Format

# Zero-credential integration tests (test channel)
RUST_LOG=debug CONFIG_FILE=configs/harness.yaml cargo run
curl -X POST http://localhost:8080/test/send \
  -H 'Content-Type: application/json' \
  -d '{"user_id":"u1","content":"hello"}'
curl http://localhost:8080/test/responses
```

**Criterion benchmarks** are in `benches/gateway_benchmarks.rs` — run with `cargo bench`.

## Performance

- **Binary size**: ~8 MB (distroless release)
- **RAM idle**: <30 MB base + ~1 MB per active agent
- **CPU idle**: <0.5%
- **Gateway overhead**: <1 ms
- **Throughput**: 10,000+ messages/second

Compared to BuckTooth (Go): ~50% smaller binary, ~40% less memory, similar or better throughput with compile-time safety guarantees.

Criterion benchmark suite: `benches/gateway_benchmarks.rs` (`bench_inmemory_store_add`, `bench_config_load`, `bench_message_stats_record`).

## Contributing

Contributions are welcome. Please open an issue or submit a pull request.

- All work tracked on [GitHub Issues](https://github.com/scttfrdmn/rustynail/issues)
- Follow [Conventional Commits](https://www.conventionalcommits.org/)
- Run `cargo fmt` and `cargo clippy` before submitting

## Sister Projects

| Project | Language | Repo |
|---------|----------|------|
| BuckTooth | Go | https://github.com/scttfrdmn/bucktooth |
| OpenClaw | Reference | https://github.com/scttfrdmn/openclaw |
| Agenkit | Rust SDK | https://github.com/scttfrdmn/agenkit |

## Versioning

Follows [Semantic Versioning 2.0.0](https://semver.org/spec/v2.0.0.html). Pre-1.0 minor bumps (`0.X.0`) may include breaking changes.

Current version: **0.15.0 (Beta)**

RustyNail stays on `0.x.x` until further notice. There is no 1.0 timeline; treat
the API and configuration format as subject to change in any minor release.

See [CHANGELOG.md](CHANGELOG.md) for the full history.

## License

Copyright 2026 Scott Friedman

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for the full text.

## Why "RustyNail"?

A rusty nail is strong, enduring, and gets the job done. Rust + Nail = RustyNail. 🦀🔨
