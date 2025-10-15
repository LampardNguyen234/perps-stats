# Fresh Database Setup Guide

This guide explains how to set up the database from scratch using the consolidated schema file.

## Overview

The project now uses a **single consolidated schema file** (`schema_fresh.sql`) instead of incremental migrations. This approach is simpler for fresh setups and provides a complete view of the database structure.

## Files

| File | Purpose |
|------|---------|
| `schema_fresh.sql` | Complete database schema (tables, indexes, functions, partitions) |
| `scripts/fresh_db_setup.sh` | Automated setup script (drops and recreates database) |
| `migrations_backup/` | Backup of old migration files (for reference only) |

## Quick Start

### Option 1: Automated Setup (Recommended)

```bash
# From project root directory
./scripts/fresh_db_setup.sh
```

This script will:
1. Drop the existing `perps_stats` database (after confirmation)
2. Create a fresh `perps_stats` database
3. Load the complete schema
4. Create initial partitions for the next 7 days

**Note**: You will be prompted to type "yes" to confirm the destructive operation.

### Option 2: Manual Setup

If you prefer manual control or the script doesn't work:

```bash
# 1. Connect to PostgreSQL as superuser
psql postgres://localhost/postgres

# 2. Drop existing database (if any)
DROP DATABASE IF EXISTS perps_stats;

# 3. Create new database
CREATE DATABASE perps_stats OWNER perps;

# 4. Exit and load schema
\q

# 5. Load schema file
psql postgresql://perps:<password>@localhost:5432/perps_stats -f schema_fresh.sql
```

### Option 3: Using Environment Variables

```bash
# Set connection parameters
export DB_NAME=perps_stats
export DB_USER=perps
export DB_HOST=localhost
export DB_PORT=5432

# Run setup (auto-confirms with 'yes')
echo "yes" | ./scripts/fresh_db_setup.sh
```

## Database Structure

### Core Tables

| Table | Purpose | Partitioned |
|-------|---------|-------------|
| `exchanges` | Supported exchanges | No |
| `markets` | Market/contract specifications | No |

### Time-Series Tables (Partitioned by Day)

| Table | Purpose | Partition Key |
|-------|---------|---------------|
| `tickers` | Price and 24h statistics | `ts` |
| `orderbooks` | Order book snapshots (JSON) | `ts` |
| `trades` | Individual trade records | `ts` |
| `funding_rates` | Funding rate snapshots | `ts` |
| `open_interest` | Open interest snapshots | `ts` |
| `klines` | OHLCV candlestick data | `open_time` |
| `liquidity_depth` | Liquidity depth statistics | `ts` |

### Pre-Seeded Data

The schema automatically seeds the `exchanges` table with:
- Binance Futures (`binance`)
- Bybit (`bybit`)
- KuCoin Futures (`kucoin`)
- Hyperliquid (`hyperliquid`)
- Lighter (`lighter`)
- Paradex (`paradex`)

## Partition Management

### Automatic Partition Creation

The schema includes functions for partition management:

```sql
-- Create partition for a specific table and date
SELECT create_partition('tickers', '2025-10-20');

-- Create partitions for the next N days for all tables
SELECT create_future_partitions(30);  -- 30 days ahead
```

### Initial Partitions

The setup automatically creates partitions for **7 days ahead** (today + 6 future days). This ensures the database is ready for immediate data ingestion.

### Adding More Partitions

If you plan to run for longer periods, create additional partitions:

```bash
psql postgresql://perps:<password>@localhost:5432/perps_stats -c "SELECT create_future_partitions(30);"
```

## Verification

After setup, verify the database structure:

```bash
# List all tables
psql postgresql://perps:<password>@localhost:5432/perps_stats -c "\dt"

# Check tickers table structure
psql postgresql://perps:<password>@localhost:5432/perps_stats -c "\d tickers"

# Verify exchanges were seeded
psql postgresql://perps:<password>@localhost:5432/perps_stats -c "SELECT * FROM exchanges;"

# Count partitions
psql postgresql://perps:<password>@localhost:5432/perps_stats -c "
SELECT
    schemaname,
    tablename
FROM pg_tables
WHERE tablename LIKE '%_2025_%'
ORDER BY tablename;
"
```

## Configuration

### Environment Variables

Set the `DATABASE_URL` environment variable for the application:

```bash
# In .env file or shell
export DATABASE_URL=postgresql://perps:<password>@localhost:5432/perps_stats
```

Or create a `.env` file:

```bash
cp .env.example .env
# Edit .env and set DATABASE_URL
```

### Testing Connection

```bash
# Test with db stats command
cargo run -- db stats

# Expected output: database statistics for all tables
```

## Schema Details

### Symbol Normalization

All symbols are stored in **normalized global format**:
- Exchange format: `BTCUSDT` (Binance), `XBTUSDTM` (KuCoin), `BTC-USD-PERP` (Paradex)
- Database storage: `BTC` (normalized)
- Enables easy cross-exchange queries

### Ticker Fields

The `tickers` table includes all fields from the Ticker type:

| Field | Type | Description |
|-------|------|-------------|
| `best_bid` | NUMERIC | Best bid price |
| `best_bid_qty` | NUMERIC | Best bid quantity |
| `best_ask` | NUMERIC | Best ask price |
| `best_ask_qty` | NUMERIC | Best ask quantity |
| `price_change_pct` | NUMERIC | 24h price change % (decimal, e.g., 0.05 = 5%) |
| `volume_24h` | NUMERIC | 24h trading volume |
| `turnover_24h` | NUMERIC | 24h turnover (quote volume) |
| `high_24h` | NUMERIC | 24h high price |
| `low_24h` | NUMERIC | 24h low price |

### Klines Interval Format

Intervals are normalized to standard format:
- Standard: `1m`, `5m`, `15m`, `1h`, `4h`, `1d`, `1w`
- Ensures consistency across exchanges

## Troubleshooting

### Permission Denied

If you get "permission denied" errors:

```bash
# Option 1: Use PostgreSQL superuser
psql postgres://postgres@localhost/postgres -f schema_fresh.sql

# Option 2: Grant privileges
psql postgres://postgres@localhost/postgres -c "
GRANT ALL PRIVILEGES ON DATABASE perps_stats TO perps;
ALTER DATABASE perps_stats OWNER TO perps;
"
```

### Active Connections

If database drop fails due to active connections:

```bash
# Terminate all connections
psql postgres://localhost/postgres -c "
SELECT pg_terminate_backend(pid)
FROM pg_stat_activity
WHERE datname = 'perps_stats'
AND pid <> pg_backend_pid();
"

# Then retry drop
psql postgres://localhost/postgres -c "DROP DATABASE perps_stats;"
```

### Missing Partitions

If you see "no partition of relation" errors:

```bash
# Create partitions for more days
psql postgresql://perps:<password>@localhost:5432/perps_stats -c "SELECT create_future_partitions(30);"
```

### Schema Out of Date

If the schema file doesn't match your needs:

```bash
# Export current schema from running database
pg_dump postgresql://perps:<password>@localhost:5432/perps_stats \
  --schema-only --no-owner --no-privileges > schema_fresh_new.sql
```

## Migrating from Old Setup

If you're migrating from the old migration-based setup:

### Option 1: Fresh Start (Data Loss)

```bash
# Backup data if needed
pg_dump postgresql://perps:<password>@localhost:5432/perps_stats > backup.sql

# Run fresh setup
./scripts/fresh_db_setup.sh
```

### Option 2: Keep Existing Data

If you already have data and the schema matches, **no action needed**. The old migration files are backed up in `migrations_backup/` for reference.

## Next Steps

After setup, you're ready to start collecting data:

```bash
# Test ticker command
cargo run -- ticker --exchange binance -s BTC

# Start data collection
cargo run -- start --exchanges binance --symbols-file symbols.txt

# View database statistics
cargo run -- db stats
```

## Reference

### Schema Location

- **Main schema**: `schema_fresh.sql` (project root)
- **Backup migrations**: `migrations_backup/*.sql` (for reference only)

### Related Documentation

- **General setup**: `docs/FRESH_SETUP.md` (this file)
- **Migration guide**: `docs/MIGRATIONS.md` (old approach, deprecated)
- **KuCoin details**: `docs/kucoin_start.md`
- **Ticker report**: `docs/explain_ticker_start.md`

### Support

If you encounter issues:

1. Check PostgreSQL is running: `psql --version`
2. Verify user permissions: `psql -l`
3. Check logs: Look for ERROR messages in schema load output
4. Review schema file: `schema_fresh.sql` for table definitions

## Important Notes

1. **Data Loss Warning**: The fresh setup script **DROPS THE ENTIRE DATABASE**. Always backup important data first.

2. **Partition Management**: Remember to create future partitions if running for extended periods. Without partitions, inserts will fail.

3. **Symbol Format**: Always use normalized symbols (e.g., `BTC`) in the database, not exchange-specific formats.

4. **Timezone**: All timestamps use `TIMESTAMP WITH TIME ZONE` for UTC storage.

5. **Idempotency**: Use `INSERT ... ON CONFLICT DO NOTHING` for idempotent inserts to handle retries safely.
