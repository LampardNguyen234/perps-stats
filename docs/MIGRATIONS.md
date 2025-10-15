# Database Migrations Guide

## Overview

This project uses **sqlx migrations** for database schema management. This is the standard and recommended approach for Rust applications using sqlx.

## Why Separate Migration Files?

### ✅ Advantages of sqlx Migrations (Current Approach)

1. **Automatic Tracking**: sqlx maintains a `_sqlx_migrations` table to track applied migrations
2. **Incremental Updates**: Only new migrations are applied in production
3. **Version Control**: Each migration is timestamped and tracked in git
4. **Team Collaboration**: Multiple developers can create migrations independently
5. **CI/CD Integration**: Works seamlessly with `sqlx-cli` and build-time query checking
6. **Production Safety**: Safe to run `db migrate` repeatedly without side effects
7. **Rollback Support**: Individual migrations can be rolled back if needed

### ❌ Why NOT to Use Single schema.sql

1. **No Migration Tracking**: Must manually track what's been applied
2. **Dangerous in Production**: Would require DROP DATABASE or manual diff
3. **Merge Conflicts**: Single file creates more git conflicts
4. **No Incremental Updates**: Must recreate entire database
5. **Breaks sqlx Tools**: `sqlx-cli` and compile-time checks won't work
6. **No Rollback**: Can't undo specific changes

## Migration File Structure

```
migrations/
├── 20251010000001_create_exchanges_and_markets.sql
├── 20251010000002_create_timeseries_tables.sql
├── 20251010000003_create_initial_partitions_and_indexes.sql
├── 20251010000004_seed_exchanges.sql
├── 20251011000001_create_liquidity_depth_table.sql
├── 20251011000002_create_open_interest_table.sql
├── 20251011000004_remove_market_id.sql
├── 20251011000005_create_klines_table.sql
├── 20251011120000_rename_klines_turnover_to_quote_volume.sql
├── 20251011120001_add_klines_missing_columns.sql
├── 20251011120002_fix_klines_partitioning.sql
└── 20251012000001_normalize_klines_intervals.sql
```

## Naming Convention

Format: `YYYYMMDDHHMMSS_description.sql`

- **YYYYMMDD**: Date (year, month, day)
- **HHMMSS**: Time (hour, minute, second) for ordering multiple migrations on same day
- **description**: Snake_case description of the migration

Example: `20251010000001_create_exchanges_and_markets.sql`

## Usage

### Initialize Database (First Time)

```bash
# Set database URL
export DATABASE_URL=postgres://localhost/perps

# Initialize database (runs all migrations + creates partitions)
cargo run -- db migrate

# Or with custom partition days
cargo run -- db migrate --create-partitions-days 14
```

### Apply New Migrations (Production)

```bash
# Run only new migrations
cargo run -- db migrate
```

This command is **idempotent** - safe to run multiple times. Only unapplied migrations will be executed.

### Create New Migration

```bash
# Using sqlx-cli (recommended)
sqlx migrate add create_new_table

# Or manually create file
touch migrations/$(date +%Y%m%d%H%M%S)_create_new_table.sql
```

### Check Migration Status

```bash
# View applied migrations
psql $DATABASE_URL -c "SELECT * FROM _sqlx_migrations ORDER BY installed_on;"

# View pending migrations (using sqlx-cli)
sqlx migrate info
```

## Migration Best Practices

### 1. Make Migrations Idempotent

Use `IF NOT EXISTS` / `IF EXISTS`:

```sql
-- Good
CREATE TABLE IF NOT EXISTS my_table (...);
CREATE INDEX IF NOT EXISTS idx_name ON my_table(column);
DROP TABLE IF EXISTS old_table;

-- Bad
CREATE TABLE my_table (...); -- Fails if table exists
```

### 2. Use Transactions

Most DDL in PostgreSQL supports transactions:

```sql
BEGIN;

CREATE TABLE new_table (...);
CREATE INDEX idx_new ON new_table(column);

COMMIT;
```

### 3. Handle Data Migrations Safely

For large data migrations, consider:

```sql
-- Add column as nullable first
ALTER TABLE my_table ADD COLUMN new_col TEXT;

-- Backfill in batches (run separately)
-- UPDATE my_table SET new_col = ... WHERE new_col IS NULL LIMIT 10000;

-- Make NOT NULL after backfill
ALTER TABLE my_table ALTER COLUMN new_col SET NOT NULL;
```

### 4. Test Migrations

Always test migrations on a copy of production data:

```bash
# Create test database
createdb perps_test

# Copy production data
pg_dump $PROD_DATABASE_URL | psql perps_test

# Test migration
DATABASE_URL=postgres://localhost/perps_test cargo run -- db migrate

# Verify results
psql perps_test -c "SELECT COUNT(*) FROM my_table;"
```

## Documentation-Only schema.sql

For documentation purposes, you can generate a complete schema:

```bash
# Generate complete schema for documentation
./docs/generate_schema.sh > docs/schema.sql
```

**Important**: This file is for documentation only. Do NOT use it for database initialization. Always use `cargo run -- db migrate`.

## Troubleshooting

### Migration Failed Midway

If a migration fails, sqlx marks it as "dirty":

```bash
# Check status
psql $DATABASE_URL -c "SELECT * FROM _sqlx_migrations WHERE success = false;"

# Fix the issue, then manually mark as applied
# (or rollback and fix the migration file)
```

### Need to Rollback

```bash
# sqlx doesn't have built-in rollback, but you can:
# 1. Create a new "down" migration that reverses changes
sqlx migrate add rollback_previous_migration

# 2. Or manually revert in database
psql $DATABASE_URL -c "DROP TABLE new_table;"
psql $DATABASE_URL -c "DELETE FROM _sqlx_migrations WHERE version = 20251012000001;"
```

### Reset Development Database

```bash
# Drop and recreate (DEVELOPMENT ONLY!)
dropdb perps
createdb perps
cargo run -- db migrate
```

## Comparison: Migrations vs schema.sql

| Feature | sqlx Migrations | Single schema.sql |
|---------|----------------|-------------------|
| **Incremental Updates** | ✅ Yes | ❌ No (must recreate) |
| **Production Safe** | ✅ Yes (idempotent) | ❌ No (dangerous) |
| **Version Tracking** | ✅ Automatic | ❌ Manual |
| **Team Collaboration** | ✅ Easy (separate files) | ❌ Merge conflicts |
| **Rollback Support** | ✅ Yes | ❌ No |
| **CI/CD Integration** | ✅ Native | ❌ Custom scripts needed |
| **sqlx-cli Support** | ✅ Full support | ❌ No support |
| **Compile-time Checks** | ✅ Works | ❌ Breaks |
| **Use Case** | Production | Development/Docs only |

## Conclusion

**Keep using sqlx migrations!** This is the conventional and recommended approach for Rust projects. A single `schema.sql` would break many sqlx features and make production deployments dangerous.

If you need a complete schema view for documentation, use the `generate_schema.sh` script, but never use the generated file for actual database initialization.

## Further Reading

- [sqlx Migrations Documentation](https://github.com/launchbadge/sqlx/blob/main/sqlx-cli/README.md#migrations)
- [Database Migrations Best Practices](https://www.prisma.io/dataguide/types/relational/migration-strategies)
- [PostgreSQL Documentation](https://www.postgresql.org/docs/current/ddl.html)
