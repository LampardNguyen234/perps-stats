# Fresh Database Setup Guide

This guide explains how to set up a completely fresh database using the consolidated `schema.sql` file.

## When to Use This Approach

Use the fresh setup approach when:

✅ **Development/Testing**: Setting up a new local development database
✅ **Fresh Start**: You want to drop everything and start clean
✅ **Initial Setup**: First-time project setup
✅ **Experimentation**: Testing schema changes in a sandbox environment

❌ **DO NOT use for production migrations** - Use `cargo run -- db migrate` instead

## Quick Start

### Option 1: Using the Helper Script (Recommended)

```bash
# Run the interactive setup script
./scripts/fresh_setup.sh

# Or with custom parameters
./scripts/fresh_setup.sh --db-name perps_dev --db-user myuser
```

The script will:
1. Prompt for confirmation (safety check)
2. Drop existing database
3. Create new database
4. Apply complete schema
5. Verify setup
6. Show you the DATABASE_URL to use

### Option 2: Manual Setup

```bash
# Step 1: Drop and create database
dropdb perps 2>/dev/null || true
createdb perps

# Step 2: Apply schema
psql -d perps -f schema.sql

# Step 3: Set environment variable
export DATABASE_URL=postgres://localhost/perps

# Step 4: Create additional partitions (optional)
cargo run -- db migrate --create-partitions-days 7
```

## What's Included in schema.sql

The consolidated schema includes:

1. **Reference Tables**
   - `exchanges` - Exchange metadata (pre-seeded with 7 exchanges)
   - `markets` - Market/symbol information

2. **Time-Series Tables** (All partitioned for performance)
   - `tickers` - Price snapshots
   - `orderbooks` - Liquidity depth snapshots
   - `trades` - Individual trade records
   - `funding_rates` - Funding rate history
   - `liquidity_depth` - Calculated liquidity statistics
   - `open_interest` - Open interest snapshots
   - `klines` - OHLCV candlestick data

3. **Audit Tables**
   - `ingest_events` - Data ingestion tracking

4. **Indexes** - All optimized indexes for query performance

5. **Partition Management**
   - Helper function `create_partition_if_not_exists()`
   - Initial partition for today's date

## Schema Details

### Database Structure

```
perps (database)
├── Reference Tables
│   ├── exchanges (7 exchanges pre-seeded)
│   └── markets
│
├── Time-Series Tables (Partitioned Daily)
│   ├── tickers (partitioned by ts)
│   ├── orderbooks (partitioned by ts)
│   ├── trades (partitioned by ts)
│   ├── funding_rates (partitioned by ts)
│   ├── liquidity_depth (partitioned by ts)
│   ├── open_interest (partitioned by ts)
│   └── klines (partitioned by open_time)
│
└── Audit Tables
    └── ingest_events
```

### Key Features

- **Partitioning**: All time-series tables are partitioned by date for performance
- **Indexes**: Optimized indexes on symbol, exchange_id, and timestamps
- **Idempotent**: Can be run multiple times safely (uses `IF NOT EXISTS`)
- **Seeded Data**: Exchanges table pre-populated with 7 exchanges

## After Setup

### Verify Installation

```bash
# Set DATABASE_URL
export DATABASE_URL=postgres://localhost/perps

# Check tables
psql $DATABASE_URL -c "\dt"

# Check exchanges
psql $DATABASE_URL -c "SELECT * FROM exchanges;"

# Check partitions
psql $DATABASE_URL -c "SELECT tablename FROM pg_tables WHERE tablename ~ '_[0-9]{4}_[0-9]{2}_[0-9]{2}$';"

# Or use the built-in stats command
cargo run -- db stats
```

### Create Additional Partitions

The schema only creates today's partition. Create more partitions for upcoming days:

```bash
# Create partitions for the next 7 days
cargo run -- db migrate --create-partitions-days 7

# Or for the next 30 days
cargo run -- db migrate --create-partitions-days 30
```

### Start Collecting Data

```bash
# Stream real-time data
cargo run -- stream -s BTC --data-types ticker,trade

# Or start the unified collection service
cargo run -- start --exchanges binance
```

## Comparison: schema.sql vs Migrations

| Feature | schema.sql | sqlx Migrations |
|---------|-----------|-----------------|
| **Use Case** | Fresh setup | Production upgrades |
| **Data Loss** | ⚠️ Drops everything | ✅ Preserves data |
| **Speed** | ⚡ Fast (single file) | Slower (incremental) |
| **Version Control** | ❌ No tracking | ✅ Automatic tracking |
| **Idempotent** | ✅ Yes (IF NOT EXISTS) | ✅ Yes |
| **Rollback** | ❌ No | ✅ Yes |
| **Best For** | Development/Testing | Production |

## Troubleshooting

### Error: database "perps" already exists

```bash
# Drop the database first
dropdb perps
# Then run the setup again
./scripts/fresh_setup.sh
```

### Error: permission denied

```bash
# Make sure you have the right PostgreSQL user
psql -U postgres -c "CREATE DATABASE perps;"

# Or use the script with custom user
./scripts/fresh_setup.sh --db-user postgres
```

### Error: schema.sql not found

```bash
# Make sure you're in the project root directory
cd /path/to/perps-stats
./scripts/fresh_setup.sh
```

### Partitions not created

The schema creates only today's partition. Create more:

```bash
cargo run -- db migrate --create-partitions-days 7
```

## Switching Between Approaches

### From Migrations to Fresh Setup

If you've been using migrations but want a fresh start:

```bash
# Back up data if needed
pg_dump $DATABASE_URL > backup.sql

# Use fresh setup
./scripts/fresh_setup.sh

# Restore specific data if needed
psql $DATABASE_URL < backup.sql
```

### From Fresh Setup to Migrations

If you used `schema.sql` and now want to use migrations:

```bash
# Create the migration tracking table
psql $DATABASE_URL -c "CREATE TABLE IF NOT EXISTS _sqlx_migrations (
    version BIGINT PRIMARY KEY,
    description TEXT NOT NULL,
    installed_on TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT now(),
    success BOOLEAN NOT NULL,
    checksum BYTEA NOT NULL,
    execution_time BIGINT NOT NULL
);"

# Mark all migrations as applied
# (This prevents re-running migrations you already have via schema.sql)
# Note: You'll need to do this manually or just continue with fresh setups
```

## Best Practices

### Development Workflow

```bash
# Daily development
export DATABASE_URL=postgres://localhost/perps

# When schema changes, just recreate
./scripts/fresh_setup.sh

# Or use docker for isolation
docker run --name perps-db -e POSTGRES_PASSWORD=password -p 5432:5432 -d postgres
./scripts/fresh_setup.sh --db-user postgres
```

### Testing Workflow

```bash
# Create test database
./scripts/fresh_setup.sh --db-name perps_test

# Run tests
DATABASE_URL=postgres://localhost/perps_test cargo test

# Clean up
dropdb perps_test
```

### Production Setup

**For production, DO NOT use schema.sql!** Use the migration-based approach:

```bash
# Set production DATABASE_URL
export DATABASE_URL=postgres://user:pass@production-host:5432/perps

# Run migrations (safe and incremental)
cargo run -- db migrate

# Create partitions
cargo run -- db migrate --create-partitions-days 30
```

## FAQ

**Q: Should I use schema.sql or migrations?**
A: Use **schema.sql for fresh development setups**, and **migrations for production**.

**Q: Will schema.sql stay in sync with migrations?**
A: Yes, it's generated from all migrations. If migrations change, regenerate it:
```bash
./docs/generate_schema.sh > schema.sql
```

**Q: Can I modify schema.sql directly?**
A: No, always modify the migration files and regenerate schema.sql.

**Q: What if I already have data?**
A: Export your data first, run fresh setup, then re-import. Or use migrations instead.

**Q: How do I update schema.sql when migrations change?**
A: Run `./docs/generate_schema.sh > schema.sql` to regenerate it.

## Summary

✅ **Use `schema.sql` + `fresh_setup.sh` for**: Development, testing, fresh starts
✅ **Use `cargo run -- db migrate` for**: Production, incremental updates, preserving data

For most development work, the fresh setup approach is faster and simpler. For production deployments, always use the migration-based approach for safety.
