# RustyNail — Claude Code Guide

## Project Overview

- **Name**: RustyNail
- **Version**: 0.1.0
- **GitHub**: https://github.com/scttfrdmn/rustynail
- **License**: Apache 2.0
- **Description**: High-performance personal AI assistant built with Rust and Agenkit-Rust. Connects to messaging platforms (Discord, WhatsApp, Telegram, Slack) where users interact via chat.
- **Sister Project**: [BuckTooth](https://github.com/scttfrdmn/bucktooth) (Go implementation)

## Build & Run Commands

```bash
# Build (debug)
cargo build

# Build (release)
cargo build --release

# Run (requires env vars set)
cargo run

# Run with config file
CONFIG_FILE=config.yaml cargo run

# Run with debug logging
RUST_LOG=debug cargo run

# Run tests
cargo test

# Run tests with output
cargo test -- --nocapture

# Run specific test
cargo test test_name

# Check (fast, no codegen)
cargo check

# Lint
cargo clippy

# Format
cargo fmt
```

## Task Tracking — GITHUB ONLY

**ALL work tracking happens on GitHub. Never use local files.**

- Issues: https://github.com/scttfrdmn/rustynail/issues
- Milestones: https://github.com/scttfrdmn/rustynail/milestones
- Project Board: https://github.com/scttfrdmn/rustynail/projects

**NEVER create:**
- TODO.md, TASKS.md, STATUS.md, ROADMAP.md, or any local task-tracking files
- Local status tracking of any kind

**Always use:**
- `gh issue create` to create new issues
- `gh issue list` to see open work
- `gh issue close` when work is done
- Assign milestones and labels to every issue

## Label Taxonomy

### type
| Label | Description |
|-------|-------------|
| `type:bug` | Something isn't working |
| `type:feature` | New functionality |
| `type:enhancement` | Improvement to existing feature |
| `type:docs` | Documentation changes |
| `type:chore` | Maintenance, dependencies, tooling |
| `type:test` | Test coverage |

### area
| Label | Description |
|-------|-------------|
| `area:gateway` | Gateway core and routing |
| `area:channels` | Channel adapters |
| `area:agents` | Agent management and AI |
| `area:memory` | Memory/conversation store |
| `area:config` | Configuration system |
| `area:observability` | Logging, metrics, tracing |
| `area:tools` | Tool integrations |
| `area:api` | HTTP API endpoints |

### priority
| Label | Description |
|-------|-------------|
| `priority:critical` | Blocking, must fix immediately |
| `priority:high` | Important, next to work on |
| `priority:medium` | Normal priority |
| `priority:low` | Nice to have |

### status
| Label | Description |
|-------|-------------|
| `status:blocked` | Waiting on something external |
| `status:in-progress` | Actively being worked on |
| `status:needs-review` | PR ready for review |

### platform
| Label | Description |
|-------|-------------|
| `platform:discord` | Discord-specific work |
| `platform:whatsapp` | WhatsApp-specific work |
| `platform:telegram` | Telegram-specific work |
| `platform:slack` | Slack-specific work |

## Milestones

| Milestone | Description | Status |
|-----------|-------------|--------|
| v0.1.0 | Foundation — core types, Discord, Agenkit, HTTP | Closed (released 2026-02-01) |
| v0.2.0 | Tools & Multi-Channel — tool registry, WhatsApp | Open |
| v0.3.0 | Platform Expansion — Telegram, Slack, OpenTelemetry | Closed (released 2026-03-17) |
| v0.4.0 | Production Infrastructure — Docker, CI/CD, web dashboard | Closed (released 2026-03-17) |
| v0.4.5 | Config flexibility + integration test suite | Closed (released 2026-03-18) |
| v0.5.0 | BuckTooth parity — Prometheus, Redis, long-poll, tools, WS dashboard, CLI, distroless | Closed (released 2026-03-18) |
| v0.6.0 | OpenClaw parity — multi-LLM, SQLite/Postgres/vector memory, summarization, SMS, webhook, webchat, email, Slack Socket Mode, shell completion, Grafana | Closed (released 2026-03-18) |
| v0.7.0 | MCP integration — `rustynail mcp serve`, MCP client connectivity, agenkit 0.83.0 | Closed (released 2026-03-18) |
| v0.8.0 | BuckTooth Full Parity + Agent Skills — bearer token auth, web fetch tool, shell tool, MS Teams, Helm, benchmarks, zero-credential harness, skills | Closed (released 2026-03-18) |
| v1.0.0 | Production Ready — full hardening, docs, dashboard v2 | Open |

## Architecture Overview

```
src/
├── main.rs          # Entry point: loads config, starts gateway + HTTP server
├── lib.rs           # Library root, re-exports
├── types.rs         # Core types: Message, Channel, AgentResponse, Error enums
├── config/          # Config loading (file + env vars via config + dotenvy)
├── gateway/         # Gateway: channel registry, message router, event bus
├── channels/        # Channel adapters: Discord (serenity), future: WhatsApp/Telegram/Slack
├── memory/          # In-memory conversation store, per-user history
└── agents/          # Agent manager: per-user ConversationalAgent via Agenkit-Rust
```

### Key Traits

- `Channel` — implemented by Discord, WhatsApp, etc. Handles send/receive lifecycle
- `AgentManager` — manages per-user ConversationalAgent instances
- `MemoryStore` — conversation history with configurable window

### Key Dependencies

- **agenkit** — local path `../agenkit/agenkit-rust` — Anthropic Claude integration
- **tokio** — async runtime
- **serenity** — Discord bot framework
- **axum** — HTTP server (health, metrics, readiness endpoints)
- **tracing** — structured logging

## Configuration

### Required Environment Variables

```bash
ANTHROPIC_API_KEY=your_anthropic_api_key   # From console.anthropic.com
```

### Optional

```bash
DISCORD_BOT_TOKEN=your_discord_bot_token   # From Discord Developer Portal; absent = no Discord channel
CONFIG_FILE=config.yaml    # Path to YAML config file
RUST_LOG=info              # Log level: trace, debug, info, warn, error
ANTHROPIC_API_BASE=https://api.anthropic.com  # Override API base URL (for mock servers/proxies)
```

### Config File (config.yaml)

```yaml
gateway:
  websocket_port: 18789
  http_port: 8080
  log_level: info

channels:
  discord:
    enabled: true
    auth:
      token: ${DISCORD_BOT_TOKEN}

agents:
  llm_provider: anthropic
  llm_model: claude-3-5-sonnet-20241022
  api_key: ${ANTHROPIC_API_KEY}
  max_history: 20
  temperature: 0.7
```

## HTTP Endpoints

| Endpoint | Purpose |
|----------|---------|
| `GET /health` | Basic health check (load balancer) |
| `GET /status` | Detailed system status |
| `GET /metrics` | Prometheus-compatible metrics |
| `GET /ready` | Readiness probe (503 if not ready) |
| `GET /live` | Liveness probe (Kubernetes) |

## Conventions

### Error Handling

- Use `anyhow::Result` for application-level errors
- Use `thiserror` for domain-specific error types
- Propagate errors with `?`, avoid `.unwrap()` outside tests
- Log errors with `tracing::error!` before returning

### Async Patterns

- All I/O is async via tokio
- Use `tokio::spawn` for background tasks
- Channels (`tokio::sync::mpsc`) for cross-task communication
- `Arc<Mutex<T>>` for shared mutable state (prefer `RwLock` when reads dominate)

### Versioning (Semantic Versioning 2.0.0)

Follow [semver.org](https://semver.org/spec/v2.0.0.html) strictly:

- **MAJOR** (`X.0.0`): incompatible API or config changes
- **MINOR** (`0.X.0`): new backwards-compatible functionality
- **PATCH** (`0.0.X`): backwards-compatible bug fixes only

Pre-1.0: minor bumps (`0.X.0`) may include breaking changes.

### Changelog (Keep a Changelog 1.1.0)

Follow [keepachangelog.com](https://keepachangelog.com/en/1.1.0/) strictly:

- Every user-visible change goes in `CHANGELOG.md` before merging
- `[Unreleased]` accumulates changes since the last release — **never leave it empty for long**
- On release: rename `[Unreleased]` → `[X.Y.Z] - YYYY-MM-DD`, add fresh empty `[Unreleased]`, update comparison links at the bottom
- **Only these section headers are valid** inside a release block:
  - `### Added` — new features
  - `### Changed` — changes to existing functionality
  - `### Deprecated` — features to be removed in a future release
  - `### Removed` — features removed in this release
  - `### Fixed` — bug fixes
  - `### Security` — security vulnerability fixes
- **Never use**: `### Planned`, `### Technical Specifications`, `### Documentation`, or any other custom headers
- Do NOT list planned future work in `[Unreleased]` — that belongs in GitHub issues

### Commit Convention (Conventional Commits)

```
feat: add WhatsApp channel integration
fix: resolve Discord reconnection race condition
docs: update CLAUDE.md with new labels
chore: update dependencies
test: add integration tests for gateway routing
refactor: extract channel lifecycle into trait
```

### Code Style

- `cargo fmt` before every commit
- `cargo clippy` — fix all warnings before merging
- No `#[allow(dead_code)]` without a comment explaining why
- Integration tests in `tests/`, unit tests in `#[cfg(test)]` modules
