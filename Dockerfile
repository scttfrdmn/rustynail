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

# ── Stage 2: Runtime (distroless) ─────────────────────────────────────────────
# gcr.io/distroless/cc-debian12 provides glibc + libssl without a shell or
# package manager, significantly reducing the attack surface.
FROM gcr.io/distroless/cc-debian12 AS runtime

# Copy CA certificates from the builder so outbound HTTPS calls work
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt

WORKDIR /app

# Copy the compiled binary from the builder stage
COPY --from=builder /build/rustynail/target/release/rustynail /app/rustynail

EXPOSE 8080

# distroless images run as a non-root user (uid 65532 "nonroot") by default
ENTRYPOINT ["/app/rustynail"]
