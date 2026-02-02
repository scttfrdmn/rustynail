# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Planned
- End-to-end testing with live Discord bot
- Performance profiling and optimization
- WhatsApp channel integration
- Telegram channel integration
- Tool system (Calculator, FileSystem, Message)
- Docker image and Kubernetes manifests

## [0.1.0] - 2026-02-01

### Added
- Initial release of RustyNail
- Multi-channel architecture with pluggable channel trait
- Discord integration using Serenity library
  - Message receiving and sending
  - Attachment support
  - Health monitoring
- Agenkit-Rust integration with Claude 3.5 Sonnet
  - AnthropicAgent adapter
  - ConversationalAgent wrapper for history management
  - Per-user agent instances for conversation isolation
- Agent Manager for per-user agent lifecycle
  - Automatic agent creation on first message
  - Thread-safe HashMap for user isolation
  - Conversation history maintained per user
- HTTP server with 5 production endpoints (Axum)
  - `GET /health` - Basic health check
  - `GET /status` - Detailed system status with channel health and active users
  - `GET /metrics` - Operational metrics (Prometheus-compatible)
  - `GET /ready` - Kubernetes readiness probe
  - `GET /live` - Kubernetes liveness probe
- In-memory conversation history store
  - MemoryStore trait for extensibility
  - InMemoryStore implementation
  - Automatic history trimming to configured limits
  - Per-user history tracking
- Configuration system with YAML and environment variable support
  - Nested configuration structures
  - Sensible defaults
  - Environment-first loading for container deployments
- Gateway core with message routing
  - Channel registry and lifecycle management
  - Event bus using tokio broadcast
  - Graceful shutdown with signal handling
- Comprehensive error handling with Result types throughout
- Async/await using Tokio runtime
- Structured logging with tracing
- Test script for validating HTTP endpoints

### Technical Specifications
- Binary size: 19 MB (optimized release build)
- Memory per user: ~100 KB
- Development time: 5.25 hours
- Lines of code: ~1,100
- Unit tests: 4 passing

### Documentation
- Comprehensive README with architecture diagrams
- Configuration examples (.env.example, config.yaml.example)
- Quick start guide
- HTTP endpoint documentation
- Test script (test-health.sh)

[Unreleased]: https://github.com/scttfrdmn/rustynail/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/scttfrdmn/rustynail/releases/tag/v0.1.0
