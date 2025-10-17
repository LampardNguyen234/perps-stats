# ====================================================================================================
# Multi-stage Dockerfile for perps-stats
# ====================================================================================================
# Stage 1: Build dependencies (cached layer)
# Stage 2: Build application
# Stage 3: Runtime image (minimal)
# ====================================================================================================

# ====================================================================================================
# Stage 1: Dependencies Builder
# ====================================================================================================
# This stage builds and caches dependencies separately from application code
# Leverages Docker layer caching to speed up rebuilds when only source code changes

FROM rust:1.75-slim-bookworm AS deps-builder

# Install build dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    libpq-dev \
    && rm -rf /var/lib/apt/lists/*

# Create app directory
WORKDIR /app

# Copy only dependency manifests
COPY Cargo.toml Cargo.lock ./
COPY crates/perps-core/Cargo.toml ./crates/perps-core/
COPY crates/perps-exchanges/Cargo.toml ./crates/perps-exchanges/
COPY crates/perps-database/Cargo.toml ./crates/perps-database/
COPY crates/perps-aggregator/Cargo.toml ./crates/perps-aggregator/

# Create dummy source files to build dependencies
RUN mkdir -p src crates/perps-core/src crates/perps-exchanges/src \
    crates/perps-database/src crates/perps-aggregator/src && \
    echo "fn main() {}" > src/main.rs && \
    echo "pub fn dummy() {}" > crates/perps-core/src/lib.rs && \
    echo "pub fn dummy() {}" > crates/perps-exchanges/src/lib.rs && \
    echo "pub fn dummy() {}" > crates/perps-database/src/lib.rs && \
    echo "pub fn dummy() {}" > crates/perps-aggregator/src/lib.rs

# Build dependencies (this layer will be cached)
RUN cargo build --release && \
    rm -rf src crates/*/src target/release/perps-stats*


# ====================================================================================================
# Stage 2: Application Builder
# ====================================================================================================
# Builds the actual application using cached dependencies from Stage 1

FROM rust:1.75-slim-bookworm AS app-builder

# Install build dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    libpq-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy dependency cache from previous stage
COPY --from=deps-builder /app/target /app/target
COPY --from=deps-builder /usr/local/cargo /usr/local/cargo

# Copy workspace files
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY src ./src
COPY migrations ./migrations

# Build application in release mode
RUN cargo build --release

# Verify binary was created
RUN test -f /app/target/release/perps-stats


# ====================================================================================================
# Stage 3: Runtime Image
# ====================================================================================================
# Minimal runtime image with only necessary dependencies

FROM debian:bookworm-slim AS runtime

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    libpq5 \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user for security
RUN useradd -m -u 1000 -s /bin/bash perps && \
    mkdir -p /app /data && \
    chown -R perps:perps /app /data

WORKDIR /app

# Copy binary from builder stage
COPY --from=app-builder --chown=perps:perps /app/target/release/perps-stats /usr/local/bin/perps-stats

# Copy migrations and schema
COPY --chown=perps:perps migrations ./migrations

# Copy default symbols file (if exists)
COPY --chown=perps:perps symbols.txt ./symbols.txt 2>/dev/null || echo "BTC\nETH\nSOL" > ./symbols.txt

# Switch to non-root user
USER perps

# Environment variables (can be overridden)
ENV RUST_LOG=perps_stats=info,perps_core=info
ENV DATABASE_URL=postgres://perps:perps@postgres:5432/perps
ENV DATA_DIR=/data

# Health check (verify binary works)
HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD perps-stats --version || exit 1

# Default command: show help
CMD ["perps-stats", "--help"]

# ====================================================================================================
# LABELS
# ====================================================================================================

LABEL org.opencontainers.image.title="perps-stats"
LABEL org.opencontainers.image.description="Perpetual futures market data collection and analysis"
LABEL org.opencontainers.image.authors="perps-stats team"
LABEL org.opencontainers.image.version="0.3.0"
LABEL org.opencontainers.image.licenses="MIT"
