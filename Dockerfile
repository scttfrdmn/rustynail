# Multi-stage build — context must be the parent directory (../), because
# agenkit is at ../agenkit/agenkit-rust (a local path dependency).
#
# Build:   docker buildx build -f rustynail/Dockerfile -t rustynail:latest ..
# Compose: docker-compose.yml sets context: .. and dockerfile: rustynail/Dockerfile

# ── Stage 1: Builder ──────────────────────────────────────────────────────────
FROM rust:1.82-slim-bookworm AS builder

# Install C toolchain and OpenSSL dev headers (required by reqwest / openssl-sys)
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Copy the entire workspace (parent dir) so local path deps resolve
COPY . .

# Build only the rustynail binary in release mode
RUN cargo build --release --manifest-path rustynail/Cargo.toml

# ── Stage 2: Runtime ──────────────────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

# Install runtime dependencies: CA certificates + OpenSSL runtime
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

# Create a non-root user
RUN useradd --system --no-create-home --uid 1001 rustynail

WORKDIR /app

# Copy the compiled binary from the builder stage
COPY --from=builder /build/rustynail/target/release/rustynail /app/rustynail

# Set ownership
RUN chown rustynail:rustynail /app/rustynail

USER rustynail

EXPOSE 8080

ENTRYPOINT ["/app/rustynail"]
