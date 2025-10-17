# Deployment Guide

This guide covers deploying `perps-stats` in production environments.

## Prerequisites

- Rust 1.75+
- PostgreSQL 15+

## Installation
```bash
git clone https://github.com/LampardNguyen234/perps-stats.git
cd perps-stats
cargo build --release
```

The binary will be at `target/release/perps-stats`.

## Install Binary (Optional)

```bash
sudo cp target/release/perps-stats /usr/local/bin/
```

## Database Setup

### Database Setup
- Configure a database `perps_stats` using PostgreSQL 15+
- Create an account and password (e.g, `perps`) and grant all privileges to this database (and public schema).
  ```sql
   CREATE USER perps WITH PASSWORD 'your_secure_password';
   CREATE DATABASE perps_stats OWNER perps;
   GRANT ALL PRIVILEGES ON DATABASE perps_stats TO perps;
   # switch to the database `perps_stats`
   GRANT ALL PRIVILEGES ON SCHEMA public to perps;
  ```
  
### Database Migration
- Configure `.env` file pointing to the previously-created database.
```text
# Database configuration
DATABASE_URL=postgresql://perps_user:password@localhost:5432/perps_stats

# Logging level (trace, debug, info, warn, error)
RUST_LOG=perps_stats=info
```
- Run the following command to create/migrate table.
```bash
cargo run -- db migrate

INFO perps_stats::commands::db: Running database migrations
INFO perps_stats::commands::db: Database URL: postgresql://perps:***@localhost:5432/perps
INFO perps_stats::commands::db: ✓ Migrations completed successfully
```
- Run the following command to test if the database has been initialized.
```bash
cargo run -- db stats

Database Statistics
==================
+-----------------+-----------+--------+----------+---------------------+---------------------+
| Table           | Row Count | Size   | Data Age | Min Timestamp       | Max Timestamp       |
+-----------------+-----------+--------+----------+---------------------+---------------------+
| exchanges       |         9 |  64 kB |        - | -                   | -                   |
| markets         |         0 |  48 kB |        - | -                   | -                   |
| tickers         |         0 |  40 kB |        - | -                   | -                   |
| orderbooks      |         0 |  40 kB |        - | -                   | -                   |
| trades          |         0 |  40 kB |        - | -                   | -                   |
| funding_rates   |         0 |  40 kB |        - | -                   | -                   |
| liquidity_depth |        49 |  80 kB |      39m | -                   | -                   |
| klines          |         0 |  64 kB |        - | -                   | -                   |
| slippage        |       343 | 168 kB |      39m | -                   | -                   |
+-----------------+-----------+--------+----------+---------------------+---------------------+
```

## Run `start` Command

### Configure Symbols

- Create `symbols.txt` with the following symbols.
```bash
BTC,ETH,SOL,XRP,BNB,SUI,HYPE
```

### Run the `start` Command
- Note that the program automatically loads environment variables in `.env`.
```bash
cargo run -- start \
  -e "extended,aster,pacifica,lighter,hyperliquid,paradex,binance" \
  --report-interval 30 \
  --klines-timeframes 1h
  
  
INFO perps_stats::commands::start: Starting unified data collection service
INFO perps_stats::commands::start: Exchanges: ["extended", "aster", "pacifica", "lighter", "hyperliquid", "paradex", "binance"]
INFO perps_stats::commands::start: Symbols file: symbols.txt
INFO perps_stats::commands::start: Batch size: 100
INFO perps_stats::commands::start: Klines backfill enabled: false
INFO perps_stats::commands::start: Klines interval: 60s (timeframes: ["1h"])
INFO perps_stats::commands::start: Report interval: 30s
INFO perps_stats::commands::start: Loaded 7 symbols: ["BTC", "ETH", "SOL", "XRP", "BNB", "SUI", "HYPE"]
INFO perps_stats::commands::start: Validating 7 symbols for exchange: extended
INFO perps_stats::commands::start: Validated 7/7 symbols for exchange extended
INFO perps_stats::commands::start: Validating 7 symbols for exchange: aster
INFO perps_stats::commands::start: Validated 7/7 symbols for exchange aster
INFO perps_stats::commands::start: Connecting to database: postgresql://perps:Perps1231234@localhost:5432/perps
INFO perps_stats::commands::start: Klines backfill disabled, skipping klines tasks
INFO perps_stats::commands::start: All tasks spawned successfully. Running 2 tasks total
INFO perps_stats::commands::start: Press Ctrl+C to stop
INFO perps_stats::commands::start: Starting liquidity depth report generation task (interval: 30s)
INFO perps_stats::commands::start: Starting ticker report generation task (interval: 30s)
INFO perps_stats::commands::start: Generating liquidity depth report for 7 exchanges
INFO perps_stats::commands::start: Generating ticker report for 7 exchanges
```

