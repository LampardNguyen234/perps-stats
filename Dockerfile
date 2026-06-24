# Multi-stage Dockerfile for perps-stats unified service
# Optimized for production deployment with API server enabled

# Stage 1: Builder - Compile Rust application
FROM rust:1.91.0 AS builder

# Install build dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Create app directory
WORKDIR /app

# Copy manifests first for better layer caching
COPY Cargo.toml Cargo.lock ./
COPY crates/ ./crates/

# Copy source code/cont
COPY src/ ./src/

# Build for release with optimizations
RUN cargo build --release

# Stage 2: Runtime - Minimal image with just the binary
FROM debian:bookworm-slim

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user for security
RUN useradd -m -u 1000 -s /bin/bash perps

# Create necessary directories
RUN mkdir -p /app /etc/perps-stats /var/log/perps-stats && \
    chown -R perps:perps /app /etc/perps-stats /var/log/perps-stats

# Set working directory
WORKDIR /app

# Copy binary from builder
COPY --from=builder /app/target/release/perps-stats /app/perps-stats

# Copy migrations directory (required for db migrate command)
COPY --chown=perps:perps migrations /app/migrations

# Copy configuration files
COPY --chown=perps:perps equities.txt /app/symbols_equity.txt
COPY --chown=perps:perps exchanges_equity.txt /app/exchanges_equity.txt

# Switch to non-root user
USER perps

# Environment variables with defaults
ENV DATABASE_URL="" \
    RUST_LOG="info" \
    SYMBOLS_FILE="/app/symbols_equity.txt" \
    EXCHANGES_FILE="/app/exchanges_equity.txt" \
    API_PORT="9999" \
    API_HOST="0.0.0.0" \
    POOL_SIZE="100" \
    REPORT_INTERVAL="30" \
    ENABLE_BACKFILL="false"

# Expose API port
EXPOSE 9999

# Health check
HEALTHCHECK --interval=30s --timeout=10s --start-period=40s --retries=3 \
    CMD curl -f http://localhost:9999/api/v1/health || exit 1

# Default command: start unified service with API enabled
# Reads exchanges from exchanges.txt file instead of hardcoding
CMD ["/bin/sh", "-c", "EXCHANGES=$(cat ${EXCHANGES_FILE}) && /app/perps-stats start \
    --symbols-file ${SYMBOLS_FILE} \
    --exchanges \"${EXCHANGES}\" \
    --report-interval ${REPORT_INTERVAL}"]
