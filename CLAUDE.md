# RustyNail — Claude Code Guide

## Project Overview

- **Name**: RustyNail
- **Version**: 0.15.0
- **GitHub**: https://github.com/scttfrdmn/rustynail
- **License**: Apache 2.0
- **Description**: High-performance personal AI assistant built with Rust and Agenkit-Rust. Connects to messaging platforms (Discord, WhatsApp, Telegram, Slack) where users interact via chat.
- **Sister Project**: [BuckTooth](https://github.com/scttfrdmn/bucktooth) (Go implementation)

## Build & Run Commands

**Prerequisite:** agenkit is a local path dependency (`../agenkit/agenkit-rust`)
and must be cloned as a sibling directory, or `cargo build` fails immediately:

```bash
git clone https://github.com/scttfrdmn/agenkit.git   # in the parent of rustynail/
cd agenkit && git checkout v0.87.0                   # pin to the tested release
```

**Pin to a release tag, not `main`.** A path dependency carries no version
constraint, so Cargo will happily build against whatever is checked out. Both
CI workflows pin `ref: v0.87.0`; when bumping agenkit, update `ci.yml`,
`docker.yml`, and this file together so local, CI, and release builds agree.

Required layout — both CI workflows check out this same structure:

```
parent/
├── agenkit/agenkit-rust/
└── rustynail/
```

Minimum Rust toolchain is **1.94** (`rust-version` in Cargo.toml), set by the AWS
SDK crates that agenkit's Bedrock adapter pulls in. Keep it in sync with the
Dockerfile builder image.

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

## Parity Tracking

Each milestone plan **must** include a parity section that compares RustyNail against both sister projects and identifies gaps to close:

- **BuckTooth** (Go): https://github.com/scttfrdmn/bucktooth
- **OpenClaw** (reference implementation): https://github.com/scttfrdmn/openclaw

### How to track parity

Before drafting a milestone plan:
1. Review the latest releases of BuckTooth and OpenClaw to identify features RustyNail is missing.
2. List each gap with the source project (`BuckTooth`, `OpenClaw`, or `Both`) and a short description.
3. Assign gaps to the milestone or a future one, and create GitHub issues for each.

### Parity status (as of v0.15.0)

| Feature area | BuckTooth | OpenClaw | Notes |
|---|---|---|---|
| Core gateway + HTTP | ✅ | ✅ | |
| Discord channel | ✅ | ✅ | |
| WhatsApp channel | ✅ | ✅ | |
| Telegram channel (webhook + long-poll) | ✅ | ✅ | |
| Slack channel (webhook + socket mode) | ✅ | ✅ | |
| SMS channel (Twilio) | ✅ | ✅ | |
| Webchat channel | ✅ | ✅ | |
| Email channel | ✅ | ✅ | |
| Microsoft Teams channel | ✅ | ✅ | |
| Multi-LLM (Anthropic, OpenAI, Ollama) | ✅ | ✅ | |
| In-memory store | ✅ | ✅ | |
| Redis memory | ✅ | ✅ | |
| SQLite memory | ✅ | ✅ | |
| Postgres memory | ✅ | ✅ | |
| Vector memory | ✅ | ✅ | |
| Conversation summarization | ✅ | ✅ | |
| Prometheus metrics | ✅ | ✅ | |
| OpenTelemetry tracing | ✅ | ✅ | |
| Grafana dashboard config | ✅ | ✅ | |
| Web dashboard (HTTP) | ✅ | ✅ | |
| WebSocket dashboard | ✅ | ✅ | |
| Calculator tool | ✅ | ✅ | |
| Formatter tool | ✅ | ✅ | |
| Filesystem tool | ✅ | ✅ | |
| Web search tool | ✅ | ✅ | |
| Web fetch tool | ✅ | ✅ | |
| Shell tool | ✅ | ✅ | |
| Calendar tool | ✅ | ✅ | |
| MCP server (`mcp serve`) | ✅ | ✅ | |
| MCP client connectivity | ✅ | ✅ | |
| Agent skills | ✅ | ✅ | |
| Bearer token auth | ✅ | ✅ | |
| Token/cost accounting | ⚠️ | ✅ | Partial. Real provider token counts and USD/micro-USD cost on `/v1/chat/completions` with `stateless: true` only. The stateful path routes through wrappers that discard provider usage metadata, so it reports none — absent by design, not estimated. No per-user or per-channel spend ledger. Previously marked ✅ for code that did not exist (#109) |
| Helm chart | ✅ | ✅ | |
| Docker / distroless image | ✅ | ✅ | |
| CI/CD (GitHub Actions) | ✅ | ✅ | |
| Criterion benchmarks | ✅ | — | BuckTooth has none |
| Zero-credential test harness | ✅ | — | BuckTooth has none. Verified working end-to-end as of 2026-08-03 |
| Shell completion | ✅ | ✅ | |
| PDF analysis tool | ✅ | — | |
| Image analysis tool | ✅ | — | |
| Admin API (`/admin/*`) | ✅ | — | |
| Cron scheduler | ✅ | — | |
| `gateway.allowed_ws_origins` | ✅ | — | |
| `gateway.shutdown_timeout_seconds` | ✅ | — | |
| Message chunking (platform limits) | ✅ | — | |
| Message deduplication | ✅ | — | |
| Channel-aware response formatting | ✅ | — | |
| Attachment auto-routing | ✅ | — | |
| LLM retry jitter | ✅ | — | |
| LLM provider fallback chain | ✅ | — | |
| Teams HMAC-SHA256 validation | ✅ | — | |
| Temporal memory decay | ✅ | — | |
| Token-based memory compaction | ✅ | — | |
| WebSocket token streaming | ✅ | — | |
| OpenAI-compatible SSE endpoint | ✅ | — | |

Update this table at the start of each milestone planning session.

A ✅ means the feature exists **and has been verified to work**. Before marking a
row ✅, exercise it — three rows in this table previously claimed parity for code
that was shipped but non-functional (the test harness returned an empty buffer,
Docker builds failed on an MSRV too low for the locked dependencies, and
token/cost accounting was ✅ for code that did not exist at all).

A ⚠️ means partially implemented: the Notes column states exactly what works and
what does not. Prefer ⚠️ with a precise note over a ✅ that overstates — an
inaccurate ✅ is how all three incidents above went unnoticed.

## Milestones

| Milestone | Description | Status |
|-----------|-------------|--------|
| v0.1.0 | Foundation — core types, Discord, Agenkit, HTTP | Closed (released 2026-02-01) |
| v0.2.0 | Tools & Multi-Channel — tool registry, WhatsApp | Closed (released 2026-03-17) |
| v0.3.0 | Platform Expansion — Telegram, Slack, OpenTelemetry | Closed (released 2026-03-17) |
| v0.4.0 | Production Infrastructure — Docker, CI/CD, web dashboard | Closed (released 2026-03-17) |
| v0.4.5 | Config flexibility + integration test suite | Closed (released 2026-03-18) |
| v0.5.0 | BuckTooth parity — Prometheus, Redis, long-poll, tools, WS dashboard, CLI, distroless | Closed (released 2026-03-18) |
| v0.6.0 | OpenClaw parity — multi-LLM, SQLite/Postgres/vector memory, summarization, SMS, webhook, webchat, email, Slack Socket Mode, shell completion, Grafana | Closed (released 2026-03-18) |
| v0.7.0 | MCP integration — `rustynail mcp serve`, MCP client connectivity, agenkit 0.83.0 | Closed (released 2026-03-18) |
| v0.8.0 | BuckTooth Full Parity + Agent Skills — bearer token auth, web fetch tool, shell tool, MS Teams, Helm, benchmarks, zero-credential harness, skills | Closed (released 2026-03-18) |
| v0.9.0 | Production Hardening — rate limiting, audit logging, body limits, timeouts, security metrics, SIGHUP hot-reload, LLM retry resilience | Closed (released 2026-03-18) |
| v0.10.0 | BuckTooth Remaining Gaps — PDF analysis, image analysis, Admin API, Cron scheduler, WS origin restriction, configurable shutdown timeout | Closed (released 2026-03-18) |
| v0.11.0 | Message Quality & Resilience — chunking, deduplication, channel formatting, attachment routing, retry jitter, provider fallback | Closed (released 2026-03-18) |
| v0.12.0 | Streaming & Memory Intelligence — Teams HMAC, vector decay, token compaction, WS streaming, OpenAI SSE | Closed (released 2026-03-18) |
| v0.13.0 | Integration Testing & Operational Maturity — rate limiter/agent/HotConfig/admin API/Teams/pipeline tests, config validate, admin audit logging | Closed (released 2026-03-18) |
| v0.14.0 | Deployment & User Documentation — README overhaul, docs/ reference directory (configuration, deployment, channels, CLI, API, architecture, troubleshooting) | Milestone closed; never tagged — shipped inside v0.15.0 (#94). No `v0.14.0` tag or release exists, by design |
| v0.15.0 | Build & Supply Chain Correctness — shell allowlist hardening, working Docker build (MSRV 1.94), functional test harness, green CI, agenkit pinned to v0.87.0 | Closed (released 2026-08-03) |
| v0.16.0 | Release Process Enforcement & Correctness — pre-tag gating (#97), harness buffer isolation (#92), verify README claims (#93) | Open |
| v1.0.0 | Deferred — work gated on a stable API. **Not a target**; the project stays on `0.x.x` until further notice | Open |
| v1.1.0 | Deferred — channel expansion: Matrix, Signal/IRC/LINE/Viber/WeChat, social DMs | Open |

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

**This project stays on `0.x.x` until further notice.** Do not propose, plan or
cut a `1.0.0` release, and do not treat the `v1.0.0` milestone as a target date
— it is a bucket for work that is genuinely gated on a stable API, not a
timeline. New work belongs in the next `0.x.0` milestone. Breaking changes are
allowed in a minor bump and must be marked **BREAKING** in the CHANGELOG entry
(see the shell allowlist change in `[0.15.0]` for the expected form).

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

### Releasing — a version is not released until every artifact agrees

A version bump is not a release. Three separate drifts got this far unnoticed
because each step was manual and nothing verified the result:

- **v0.14.0** had a version bump, a CHANGELOG entry, and a closed milestone —
  but no tag and no release (#94). The version existed only in files.
- **v0.5.0, v0.6.0, v0.7.0** were tagged but had no GitHub release for four
  months, and v0.7.0 had no milestone at all.
- **Every tag from v0.9.0 to v0.14.0** produced no usable image: the Dockerfile
  builder MSRV was below what `Cargo.lock` required. CI used `stable`, so only
  tag builds broke — and nobody read those logs.

**Run the gate before tagging. It is not optional:**

```bash
scripts/check-release-consistency.sh            # version coherence (also runs in CI)
scripts/check-release-consistency.sh --release  # + tag, release, milestone
```

Release checklist — every box, in this order:

1. `[Unreleased]` → `[X.Y.Z] - YYYY-MM-DD`; add a fresh empty `[Unreleased]`
2. Add the `[X.Y.Z]:` comparison link at the bottom and repoint `[Unreleased]`
3. Bump the version in **all six**: `Cargo.toml`, `Cargo.lock` (via
   `cargo update -p rustynail`), `README.md`, `CLAUDE.md`, `Chart.yaml`
   (`version` **and** `appVersion`), and any `docs/*.md` sample output
4. `scripts/check-release-consistency.sh` — must exit 0
5. Merge to `main` and **wait for CI to go green on the merge commit**, not just
   on the PR
6. `git tag -a vX.Y.Z -m "…"` and `git push origin vX.Y.Z`
7. `gh release create vX.Y.Z --notes-file …` — a tag alone is not a release
8. Close the milestone; create it first if it does not exist
9. `scripts/check-release-consistency.sh --release` — must exit 0
10. Confirm `docker.yml` succeeded **and pull the image** to verify it runs.
    Note the published tag has no `v` prefix (`ghcr.io/scttfrdmn/rustynail:X.Y.Z`),
    and the image is amd64-only — on arm64 pass `--platform linux/amd64`

Never mark a release done on the strength of a green checkmark alone. Job
success means the step exited 0, not that the artifact works — pull it and run
it.

### agenkit dependency pin

agenkit is a **path dependency**, so Cargo cannot constrain its version. The
`ref:` in the workflows is the only pin that exists. Both `ci.yml` and
`docker.yml` must pin the **same release tag** — never a branch — or an
upstream push silently changes what CI validates and what the release image
contains. The consistency gate enforces this.

Note the agenkit crate version does not track its repo tags (the crate reads
`0.83.0` at tag `v0.87.0`, since tags span a polyglot repo). The tag is the
only usable pin point.

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
