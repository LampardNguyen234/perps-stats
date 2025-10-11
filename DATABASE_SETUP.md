# Database Setup and Usage Guide

This guide explains how to set up and use the PostgreSQL database with the `perps-stats` application.

## Prerequisites

- PostgreSQL 12 or higher installed
- Rust toolchain with `cargo` available

## Quick Start

### 1. Install PostgreSQL

**macOS (using Homebrew)**:
```bash
brew install postgresql@14
brew services start postgresql@14
```

**Ubuntu/Debian**:
```bash
sudo apt-get update
sudo apt-get install postgresql postgresql-contrib
sudo systemctl start postgresql
```

**Docker**:
```bash
docker run --name perps-postgres -e POSTGRES_PASSWORD=password -e POSTGRES_DB=perps -p 5432:5432 -d postgres:14
```

### 2. Create Database

```bash
# Using psql (replace 'username' with your postgres username)
createdb -U username perps

# Or using SQL
psql -U username -c "CREATE DATABASE perps;"
```

### 3. Set Environment Variable

You can configure the database URL either via environment variable or using a `.env` file (recommended).

**Option 1: Using .env file (Recommended)**

```bash
# Copy the example file
cp .env.example .env

# Edit .env and set your DATABASE_URL
# The file will be automatically loaded on startup
echo "DATABASE_URL=postgres://username:password@localhost:5432/perps" > .env
```

**Option 2: Using environment variable**

```bash
export DATABASE_URL="postgres://username:password@localhost:5432/perps"

# For production, use connection pooling and SSL:
export DATABASE_URL="postgres://username:password@localhost:5432/perps?sslmode=require"
```

**Note**: The `.env` file is automatically loaded and takes precedence. If both are set, the application uses the value from `.env`.

### 4. Initialize Database

```bash
# Run migrations and create initial partitions (7 days ahead by default)
cargo run -- db init

# Create more partitions ahead (e.g., 30 days)
cargo run -- db init --create-partitions-days 30
```

## Database Commands

### `db init`

Initializes the database schema, runs all migrations, and creates daily partitions.

```bash
# Basic initialization with 7 days of partitions
cargo run -- db init

# Create 30 days of partitions ahead
cargo run -- db init --create-partitions-days 30

# Use a specific database URL
cargo run -- db init --database-url "postgres://user:pass@localhost/perps"
```

**What it does:**
- Creates all tables (exchanges, markets, tickers, orderbooks, trades, funding_rates, ingest_events)
- Seeds the `exchanges` table with known exchanges (binance, bybit, kucoin, lighter, paradex, hyperliquid, cryptocom)
- Creates daily partitions for time-series tables for the next N days
- Sets up indexes for optimal query performance

### `db migrate`

Runs only the database migrations (useful after pulling new migration files).

```bash
cargo run -- db migrate
```

### `db stats`

Displays comprehensive database statistics including table sizes, row counts, and recent ingestion activity.

```bash
# Display statistics in table format (default)
cargo run -- db stats

# Output in JSON format for programmatic access
cargo run -- db stats --format json
```

**Output includes:**
- Total database size
- Per-table statistics (row count, table size, index size, total size)
- Recent ingest activity (last hour) per symbol and table
- Latest timestamps for each data type

### `db clean`

Manages data retention by cleaning old data or truncating tables.

```bash
# Delete data older than 90 days from all time-series tables
cargo run -- db clean --older-than 90

# Drop partitions older than 180 days (more efficient than deleting rows)
cargo run -- db clean --drop-partitions-older-than 180

# Truncate all tables (WARNING: deletes ALL data)
cargo run -- db clean --truncate

# Combine operations
cargo run -- db clean --older-than 30 --drop-partitions-older-than 90
```

**Options:**
- `--older-than N`: Deletes rows from time-series tables where timestamp is older than N days
- `--drop-partitions-older-than N`: Drops entire partitions older than N days (faster and reclaims disk space)
- `--truncate`: Truncates all tables (use with extreme caution!)

## Database Schema

### Tables

1. **exchanges** - Supported exchange metadata
   - Columns: id, name, created_at
   - Stores: binance, bybit, kucoin, lighter, paradex, hyperliquid, cryptocom

2. **markets** - Normalized market/symbol information
   - Columns: id, exchange_id, symbol, base_currency, quote_currency, contract_type, tick_size, lot_size, metadata, created_at
   - Unique constraint on (exchange_id, symbol)

3. **tickers** - Ticker snapshots (partitioned by timestamp)
   - Columns: id, exchange_id, market_id, symbol, last_price, mark_price, index_price, best_bid, best_ask, volume_24h, turnover_24h, price_change_24h, high_24h, low_24h, ts, received_at
   - Partitioned by: ts (daily partitions)

4. **orderbooks** - Liquidity snapshots (partitioned by timestamp)
   - Columns: id, exchange_id, market_id, symbol, bids_notional, asks_notional, raw_book (JSONB), spread_bps, ts, received_at
   - Partitioned by: ts (daily partitions)

5. **trades** - Individual trade records (partitioned by timestamp)
   - Columns: id, exchange_id, market_id, symbol, trade_id, price, size, side, ts, received_at, raw (JSONB)
   - Partitioned by: ts (daily partitions)

6. **funding_rates** - Funding rate history (partitioned by timestamp)
   - Columns: id, exchange_id, market_id, symbol, rate, next_rate, ts, received_at, raw (JSONB)
   - Partitioned by: ts (daily partitions)

7. **ingest_events** - Audit table for ingestion tracking
   - Columns: id, exchange_id, market_id, symbol, event_type, status, message, ts
   - Not partitioned (relatively small)

### Partitioning Strategy

Time-series tables (tickers, orderbooks, trades, funding_rates) use **daily partitioning** by the `ts` (timestamp) column.

**Benefits:**
- Improved query performance when filtering by time ranges
- Easier data retention management (drop old partitions instead of deleting rows)
- Better disk space reclamation
- Parallel query execution across partitions

**Partition Naming:**
- Format: `{table_name}_{YYYY}_{MM}_{DD}`
- Example: `tickers_2025_10_10`, `orderbooks_2025_10_11`

**Partition Maintenance:**
- Initial partitions are created during `db init`
- Create future partitions: `cargo run -- db init --create-partitions-days 30`
- Drop old partitions: `cargo run -- db clean --drop-partitions-older-than 180`

### Indexes

Each partitioned table has indexes on:
- `(symbol, ts DESC)` - For querying by symbol with time ordering
- `(exchange_id, ts DESC)` - For querying by exchange with time ordering

Additional indexes on:
- `exchanges.name` - For exchange lookups
- `markets.exchange_id, markets.symbol` - For market lookups
- `ingest_events.ts, ingest_events.status` - For audit queries

## Production Recommendations

### Connection Pooling

The application uses `sqlx::PgPool` with sensible defaults:
- Max connections: 10
- Min connections: 2
- Acquire timeout: 30 seconds
- Idle timeout: 600 seconds

Adjust via `DatabaseConfig` if needed for your workload.

### Security

1. **Use SSL/TLS for connections:**
   ```bash
   export DATABASE_URL="postgres://user:pass@hostname/perps?sslmode=require"
   ```

2. **Create a dedicated database user:**
   ```sql
   CREATE USER perps_app WITH PASSWORD 'secure_password';
   GRANT CONNECT ON DATABASE perps TO perps_app;
   GRANT USAGE ON SCHEMA public TO perps_app;
   GRANT SELECT, INSERT ON ALL TABLES IN SCHEMA public TO perps_app;
   GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO perps_app;
   ```

3. **Store credentials securely:**
   - Use environment variables or secret management services (AWS Secrets Manager, HashiCorp Vault)
   - Never commit `DATABASE_URL` to version control

### Performance Optimization

1. **Partition Management:**
   - Create partitions ahead of time (recommended: 7-30 days)
   - Drop old partitions regularly to reclaim disk space
   - Monitor partition sizes: `cargo run -- db stats`

2. **Query Optimization:**
   - Always filter by `symbol` and/or `ts` when querying time-series tables
   - Use appropriate time ranges to leverage partition pruning
   - Monitor query performance with `EXPLAIN ANALYZE` in psql

3. **Maintenance Tasks:**
   - Schedule periodic `VACUUM ANALYZE` (weekly recommended)
   - Monitor database size growth
   - Set up automated backups

### Monitoring

Use `db stats` to monitor:
- Table sizes and growth rates
- Ingest activity (rows per hour)
- Latest data timestamps per symbol

Example monitoring script:
```bash
#!/bin/bash
# Save stats to JSON for monitoring system
cargo run -- db stats --format json > /var/log/perps-stats/db-stats-$(date +%Y%m%d-%H%M%S).json
```

## Troubleshooting

### Connection Errors

**Error**: "Failed to create database pool"

**Solution**:
1. Verify PostgreSQL is running: `pg_isready`
2. Check `DATABASE_URL` is set correctly
3. Verify user permissions: `psql $DATABASE_URL -c "SELECT 1"`

### Migration Errors

**Error**: "Failed to run migrations"

**Solution**:
1. Check migration files exist in `migrations/` folder
2. Verify database user has CREATE TABLE permissions
3. Run migrations manually: `psql $DATABASE_URL -f migrations/20251010000001_create_exchanges_and_markets.sql`

### Partition Creation Fails

**Error**: "Failed to create partition"

**Solution**:
1. Check if partition already exists (idempotent operation)
2. Verify database user has CREATE TABLE permissions
3. Ensure parent table exists: `\d tickers` in psql

### Slow Queries

**Issue**: Queries are slow

**Solutions**:
1. Verify indexes exist: `\di` in psql
2. Use `EXPLAIN ANALYZE` to identify bottlenecks
3. Ensure queries filter by indexed columns (symbol, ts)
4. Consider increasing `work_mem` in postgresql.conf for complex queries

## Testing

The implementation includes:
- Compile-time SQL query checking via `sqlx`
- Migration idempotency (can run multiple times safely)
- Graceful error handling with detailed context

**Manual Testing Steps:**

1. **Test initialization:**
   ```bash
   cargo run -- db init
   # Verify output shows successful migration and partition creation
   ```

2. **Test stats:**
   ```bash
   cargo run -- db stats
   # Should show all tables with 0 rows (except exchanges which is seeded)
   ```

3. **Test cleanup:**
   ```bash
   cargo run -- db clean --older-than 1
   # Should complete without errors (no data to delete yet)
   ```

## Example Workflows

### Initial Setup for Development

```bash
# 1. Start PostgreSQL
brew services start postgresql@14

# 2. Create database
createdb perps

# 3. Configure environment (using .env file)
cp .env.example .env
# Edit .env and set: DATABASE_URL=postgres://$(whoami)@localhost/perps

# 4. Initialize schema
cargo run -- db init

# 5. Verify setup
cargo run -- db stats
```

### Production Deployment

```bash
# 1. Set production configuration in .env file
cat > .env << EOF
DATABASE_URL=postgres://perps_user:${DB_PASSWORD}@db.prod.example.com:5432/perps?sslmode=require
RUST_LOG=perps_stats=info
EOF

# 2. Run migrations
cargo run -- db migrate

# 3. Create partitions for the next 30 days
cargo run -- db init --create-partitions-days 30

# 4. Set up monitoring
cargo run -- db stats --format json | jq '.'
```

### Regular Maintenance

```bash
# Weekly: Clean data older than 90 days
cargo run -- db clean --older-than 90

# Monthly: Drop partitions older than 180 days
cargo run -- db clean --drop-partitions-older-than 180

# Daily: Check database statistics
cargo run -- db stats
```

## Additional Resources

- PostgreSQL Documentation: https://www.postgresql.org/docs/
- sqlx Documentation: https://docs.rs/sqlx/
- Partitioning Guide: https://www.postgresql.org/docs/current/ddl-partitioning.html
