# API Server Deployment Guide

This guide covers deploying the Perps Stats REST API server. For database setup, see the main [Deployment Guide](deployment.md).

## Table of Contents

- [Prerequisites](#prerequisites)
- [Quick Start](#quick-start)
- [Environment Configuration](#environment-configuration)
- [Running the Server](#running-the-server)
- [Monitoring and Health Checks](#monitoring-and-health-checks)

---

## Prerequisites

- **Rust**: 1.75 or later
- **PostgreSQL**: Database already set up and migrated (see [Deployment Guide](deployment.md))
- **Memory**: Minimum 2GB RAM (4GB+ recommended for production)

```bash
# Verify Rust installation
rustc --version
cargo --version
```

---

## Quick Start

### Option 1: Standalone API Server

```bash
# 1. Clone and build
git clone https://github.com/LampardNguyen234/perps-stats.git
cd perps-stats
cargo build --release

# 2. Set environment variable
export DATABASE_URL="postgresql://perps:your_password@localhost:5432/perps_stats"

# 3. Start the API server
cargo run --release -- serve --port 8080
```

The API will be available at `http://127.0.0.1:8080/api/`

### Option 2: Unified Service (API + Data Collection)

For a complete all-in-one solution that both collects data and serves it via API:

```bash
# 1. Build the project
cargo build --release

# 2. Set environment variable
export DATABASE_URL="postgresql://perps:your_password@localhost:5432/perps_stats"

# 3. Start unified service with API enabled
cargo run --release -- start \
  --symbols-file symbols.txt \
  --enable-api \
  --api-port 8080 \
  --pool-size 30
```

This single command starts:
- Data collection tasks (tickers, liquidity depth, orderbooks)
- REST API server on port 8080
- Shared database connection pool

The API will be immediately available at `http://0.0.0.0:8080/api/` while data is being collected in real-time.

---


## Quick Reference

### Start Server

```bash
# Development
cargo run -- serve

# Production
perps-stats serve --host 127.0.0.1 --port 8080
```

### Environment Variables

```bash
# Required
export DATABASE_URL="postgresql://perps:password@localhost:5432/perps_stats"

# Optional
export RUST_LOG="info"
```

### Health Check

```bash
curl http://127.0.0.1:8080/api/v1/health
```

### View Documentation

```bash
open http://127.0.0.1:8080/api/
```

---

## Environment Configuration

### Required Environment Variables

The API server requires the `DATABASE_URL` environment variable to connect to PostgreSQL.

#### Method 1: Using .env File (Recommended)

Create a `.env` file in the project root:

```bash
# .env
DATABASE_URL=postgresql://perps:your_secure_password@localhost:5432/perps_stats
RUST_LOG=perps_stats=info,tower_http=info
```

The application automatically loads variables from `.env` when starting.

#### Method 2: Export in Shell

Set environment variables in your current shell session:

```bash
export DATABASE_URL="postgresql://perps:your_password@localhost:5432/perps_stats"
export RUST_LOG="perps_stats=info,tower_http=info"
```

#### Method 2: Inline with Command

Set variables inline when running the command:

```bash
DATABASE_URL="postgresql://perps:password@localhost:5432/perps_stats" \
  perps-stats serve --port 8080
```

### Environment Variables Reference

| Variable       | Description                  | Default | Required |
|----------------|------------------------------|---------|----------|
| `DATABASE_URL` | PostgreSQL connection string | -       | **Yes**  |
| `RUST_LOG`     | Logging level and targets    | `info`  | No       |

#### DATABASE_URL Format

```
postgresql://[user]:[password]@[host]:[port]/[database]?[options]
```

**Examples:**

```bash
# Basic connection
DATABASE_URL="postgresql://perps:password@localhost:5432/perps_stats"

# With SSL
DATABASE_URL="postgresql://perps:password@localhost:5432/perps_stats?sslmode=require"

# With connection pool settings
DATABASE_URL="postgresql://perps:password@localhost:5432/perps_stats?max_connections=20&min_connections=5"

# Remote database
DATABASE_URL="postgresql://perps:password@db.example.com:5432/perps_stats"
```

#### RUST_LOG Format

Control logging verbosity for different modules:

```bash
# Global info level
RUST_LOG=info

# Module-specific levels
RUST_LOG=perps_stats=debug,tower_http=info,sqlx=warn

# Maximum verbosity
RUST_LOG=trace

# Application-only logs
RUST_LOG=perps_stats=debug
```

**Log Levels (most to least verbose):**
- `trace` - Very detailed trace information
- `debug` - Debugging information
- `info` - Informational messages (default)
- `warn` - Warning conditions
- `error` - Error conditions

---

## Running the Server

### Standalone API Server Mode

Use the `serve` command for API-only deployment (requires existing data):

```bash
# Start with defaults (127.0.0.1:8080)
perps-stats serve

# Custom port
perps-stats serve --port 3000

# Bind to all interfaces (for remote access)
perps-stats serve --host 0.0.0.0 --port 8080

# Custom host and port
perps-stats serve --host 192.168.1.100 --port 8888
```

**Serve Command Options:**

| Option   | Short | Default     | Description             |
|----------|-------|-------------|-------------------------|
| `--host` | -     | `127.0.0.1` | Host address to bind to |
| `--port` | `-p`  | `8080`      | Port to listen on       |

### Unified Service Mode (Recommended)

Use the `start` command with `--enable-api` for all-in-one deployment (data collection + API):

```bash
# Basic unified service
perps-stats start \
  --symbols-file symbols.txt \
  --enable-api

# Production configuration
perps-stats start \
  --symbols-file symbols.txt \
  --enable-api \
  --api-host 0.0.0.0 \
  --api-port 8080 \
  --pool-size 50 \
  --report-interval 30 \
  --enable-backfill

# Development configuration
perps-stats start \
  --symbols-file symbols.txt \
  --enable-api \
  --api-port 3000 \
  --pool-size 20
```

**Start Command API Options:**

| Option        | Default   | Description                              |
|---------------|-----------|------------------------------------------|
| `--enable-api`| `false`   | Enable REST API server                   |
| `--api-host`  | `0.0.0.0` | API server bind address                  |
| `--api-port`  | `8080`    | API server port                          |
| `--pool-size` | `20`      | Database connection pool size (all tasks)|

**Connection Pool Sizing Guidelines:**

- **Development/Testing**: 20 connections (default)
- **Production (API only)**: 20-30 connections
- **Production (API + Data Collection)**: 30-50 connections
- **Formula**: `(num_exchanges × num_timeframes) + 10 for API + buffer`

**Example Sizing Calculation:**
```
Scenario: 5 exchanges, 3 timeframes (5m, 1h, 1d), API enabled
Calculation: (5 × 3) + 10 + 5 buffer = 30 connections
Recommended: --pool-size 30
```

### Verify Server is Running

Once the server starts, you should see:

```
INFO perps_stats::commands::serve: Starting API server on 127.0.0.1:8080
INFO perps_stats::commands::serve::state: Connecting to database: postgresql://perps:***@localhost:5432/perps_stats
INFO perps_stats::commands::serve::state: Database connection established
INFO perps_stats::commands::serve: API server listening on http://127.0.0.1:8080
INFO perps_stats::commands::serve: Health check available at: http://127.0.0.1:8080/api/v1/health
INFO perps_stats::commands::serve: Database stats available at: http://127.0.0.1:8080/api/v1/stats
```

Test the endpoints:

```bash
# Health check
curl http://127.0.0.1:8080/api/v1/health

# Expected response:
# {"status":"ok","database":"healthy","timestamp":"2025-12-11T12:00:00Z","version":"0.1.0"}

# Database statistics
curl http://127.0.0.1:8080/api/v1/stats

# List exchanges
curl http://127.0.0.1:8080/api/v1/exchanges

# List symbols
curl http://127.0.0.1:8080/api/v1/symbols

# Interactive documentation (open in browser)
open http://127.0.0.1:8080/api/
```

---

## Monitoring and Health Checks

### Health Check Endpoint

```bash
# Check if server is healthy
curl http://127.0.0.1:8080/api/v1/health

# Response:
{
  "status": "ok",
  "database": "healthy",
  "timestamp": "2025-12-11T12:00:00Z",
  "version": "0.1.0"
}
```

### Database Statistics

```bash
# Get database stats
curl http://127.0.0.1:8080/api/v1/stats

# Response includes record counts and timestamps
```

### Available Endpoints

| Endpoint                     | Description                   | Method |
|------------------------------|-------------------------------|--------|
| `/api/v1/health`             | Server health status          | GET    |
| `/api/v1/stats`              | Database statistics           | GET    |
| `/api/v1/exchanges`          | List of exchanges             | GET    |
| `/api/v1/symbols`            | List of tracked symbols       | GET    |
| `/api/v1/tickers`            | Latest ticker data            | GET    |
| `/api/v1/tickers/history`    | Historical tickers            | GET    |
| `/api/v1/liquidity`          | Latest liquidity depth        | GET    |
| `/api/v1/liquidity/history`  | Historical liquidity          | GET    |
| `/api/v1/orderbooks`         | Latest orderbooks             | GET    |
| `/api/v1/orderbooks/history` | Historical orderbooks         | GET    |
| `/api/`                      | Interactive API documentation | GET    |

---

