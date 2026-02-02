# RustyNail 🦀🔨

**"Rust Never Sleeps!"**

RustyNail is a high-performance personal AI assistant built with Rust and Agenkit-Rust. It connects to messaging platforms (Discord, WhatsApp, Telegram, Slack) where users interact with it naturally through chat.

[![Version](https://img.shields.io/badge/version-0.1.0-blue)](https://github.com/scttfrdmn/rustynail/releases/tag/v0.1.0)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache%202.0-green)](LICENSE)
[![Status](https://img.shields.io/badge/status-alpha-yellow)](https://github.com/scttfrdmn/rustynail)

## Features

- **Multi-Channel Support**: Discord (Phase 1), WhatsApp, Telegram, Slack (coming soon)
- **Conversational AI**: Powered by Claude via Agenkit-Rust
- **Memory**: Maintains conversation context across channels
- **Performance**: Built with Rust for maximum speed and safety
- **Production-Ready**: Built-in observability, metrics, and health checks
- **Single Binary**: Easy deployment with minimal dependencies

## Quick Start

### Prerequisites

- Rust 1.75 or higher
- Discord bot token (get from [Discord Developer Portal](https://discord.com/developers/applications))
- Anthropic API key (get from [Anthropic Console](https://console.anthropic.com))

### Installation

```bash
# Clone the repository
git clone <repository-url>
cd rustynail

# Build
cargo build --release

# Or run directly
cargo run
```

### Configuration

Create a `.env` file or export environment variables:

```bash
export DISCORD_BOT_TOKEN=your_discord_bot_token
export ANTHROPIC_API_KEY=your_anthropic_api_key
```

Or create a configuration file:

```yaml
# config.yaml
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

### Running

```bash
# Using environment variables
./target/release/rustynail

# Using config file
CONFIG_FILE=config.yaml ./target/release/rustynail

# With custom log level
RUST_LOG=debug cargo run
```

## Usage

1. Invite your Discord bot to a server
2. Start RustyNail
3. Send a message to the bot in Discord
4. The bot will respond using Claude AI with conversation memory

## HTTP Endpoints

RustyNail provides comprehensive health and monitoring endpoints:

- **`GET /health`** - Basic health check (for load balancers)
  ```json
  {"status": "ok", "version": "0.1.0"}
  ```

- **`GET /status`** - Detailed system status
  ```json
  {
    "status": "running",
    "version": "0.1.0",
    "channels": [...],
    "active_users": 42
  }
  ```

- **`GET /metrics`** - Operational metrics (Prometheus-compatible)
  ```json
  {
    "active_users": 42,
    "channels_count": 1,
    "healthy_channels": 1
  }
  ```

- **`GET /ready`** - Readiness probe (returns 503 if not ready)
- **`GET /live`** - Liveness probe (for Kubernetes)

### Testing Endpoints

```bash
# Test all endpoints
./test-health.sh

# Or individually
curl http://localhost:8080/health
curl http://localhost:8080/status
curl http://localhost:8080/metrics
```

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│  RustyNail Gateway (Single Binary)                        │
│  ┌─────────────────────────────────────────────────┐   │
│  │ HTTP Server (Axum) - Port 8080                  │   │
│  │ ├─ /health  - Health check                      │   │
│  │ ├─ /status  - Detailed status                   │   │
│  │ ├─ /metrics - Operational metrics               │   │
│  │ ├─ /ready   - Readiness probe (K8s)             │   │
│  │ └─ /live    - Liveness probe (K8s)              │   │
│  └─────────────────────────────────────────────────┘   │
│  ┌─────────────────────────────────────────────────┐   │
│  │ Gateway Core                                     │   │
│  │ - Channel Registry & Lifecycle                  │   │
│  │ - Message Router & Event Bus                    │   │
│  │ - Agent Manager (Per-User)                      │   │
│  └─────────────────────────────────────────────────┘   │
│           │                                             │
│  ┌────────┴────────┬──────────┬──────────┐            │
│  │ Discord Channel │ WhatsApp │ Telegram │ Slack...   │
│  └────────┬────────┴──────────┴──────────┘            │
│           │                                             │
│  ┌────────▼────────────────────────────────────────┐  │
│  │ Agent Manager (Per-User ConversationalAgents)   │  │
│  │ - Anthropic Claude 3.5 Sonnet                   │  │
│  │ - Agenkit-Rust Integration                      │  │
│  └────────┬────────────────────────────────────────┘  │
│           │                                             │
│  ┌────────▼────────────────────────────────────────┐  │
│  │ Memory Store (In-Memory)                         │  │
│  └──────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────┘
```

## Development

### Project Structure

```
rustynail/
├── src/
│   ├── main.rs              # Application entry point
│   ├── lib.rs               # Library root
│   ├── types.rs             # Core types and enums
│   ├── config/              # Configuration management
│   ├── gateway/             # Gateway implementation
│   ├── channels/            # Channel adapters (Discord, etc.)
│   ├── memory/              # Memory management
│   └── agents/              # Agent configurations
├── diary/                   # Development diary (not in git)
├── Cargo.toml               # Rust dependencies
└── README.md                # This file
```

### Building

```bash
# Debug build
cargo build

# Release build (optimized)
cargo build --release

# Build with all optimizations
cargo build --release --features production
```

### Testing

```bash
# Run all tests
cargo test

# Run tests with output
cargo test -- --nocapture

# Run specific test
cargo test test_name
```

## Roadmap

### Phase 1: Foundation ✅ (95% Complete)
- [x] Project setup
- [x] Core types and traits
- [x] Configuration system
- [x] Memory store
- [x] Discord channel
- [x] Agenkit integration (Claude 3.5 Sonnet)
- [x] Per-user conversation agents
- [x] HTTP health & metrics endpoints
- [ ] End-to-end testing

### Phase 2: Tools & Multi-Channel
- [ ] Tool registry
- [ ] Calculator, Message, FileSystem tools
- [ ] WhatsApp channel
- [ ] Cross-channel messaging

### Phase 3: Expansion
- [ ] Telegram channel
- [ ] Slack channel
- [ ] Planning agent
- [ ] Calendar & Web Search tools
- [ ] OpenTelemetry tracing

### Phase 4: Polish
- [ ] Web dashboard
- [ ] Enhanced CLI
- [ ] Docker image
- [ ] Kubernetes manifests
- [ ] Migration tool from OpenClaw

## Performance

Expected performance characteristics:

- **Binary Size**: ~5-10 MB (optimized release)
- **Memory Usage**: ~20-30 MB base + 1 MB per agent
- **CPU Usage**: <0.5% idle, <3% under load
- **Latency**: <1ms overhead
- **Throughput**: 10,000+ messages/second

Compared to BuckTooth (Go):
- **Binary Size**: ~50% smaller
- **Memory**: ~40% less
- **Performance**: Similar or better
- **Safety**: Compile-time guarantees

## Contributing

Contributions are welcome! Please open an issue or submit a pull request.

## Versioning

This project follows [Semantic Versioning 2.0.0](https://semver.org/spec/v2.0.0.html).

Current version: **0.1.0** (Alpha)

See [CHANGELOG.md](CHANGELOG.md) for a detailed history of changes.

## License

Copyright 2026 Scott Friedman

Licensed under the Apache License, Version 2.0 (the "License");
you may not use this file except in compliance with the License.
You may obtain a copy of the License at

    http://www.apache.org/licenses/LICENSE-2.0

Unless required by applicable law or agreed to in writing, software
distributed under the License is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
See the License for the specific language governing permissions and
limitations under the License.

See [LICENSE](LICENSE) for the full license text.

## Acknowledgments

- Built with [Agenkit-Rust](https://github.com/scttfrdmn/agenkit)
- Inspired by [OpenClaw](https://github.com/openclaw/openclaw)
- Sister project: [BuckTooth](https://github.com/scttfrdmn/bucktooth) (Go implementation)
- Powered by [Anthropic Claude](https://www.anthropic.com)

## Why "RustyNail"?

A rusty nail is strong, enduring, and gets the job done. Plus, Rust + Nail = RustyNail! 🦀🔨
