# `perps-stats` CLI Tutorials

This document provides a comprehensive guide to using the `perps-stats` command-line interface (CLI). Each section details a specific command, its purpose, available options, and practical examples.

## Global Options

These options are available for all commands:

-   `--help`: Displays a help message for the command.
-   `--version`: Shows the current version of the `perps-stats` application.

## Table of Contents

1. [Database Commands](#database-commands)
   - [db migrate](#db-init)
   - [db migrate](#db-migrate)
   - [db stats](#db-stats)
   - [db clean](#db-clean)
2. [Data Ingestion Commands](#data-ingestion-commands)
   - [backfill](#backfill)
   - [stream](#stream)
   - [start](#start)
3. [Data Collection Commands](#data-collection-commands)
   - [run](#run)
   - [ticker](#ticker)
   - [liquidity](#liquidity)
   - [market](#market)

---

## Database Commands

### `db migrate`

Initialize the database schema and run all migrations. This command creates all necessary tables, indexes, and seeds initial data (exchanges table).

**Usage:**

```bash
DATABASE_URL=postgres://localhost/perps_stats cargo run -- db migrate
```

**What it does:**
1. Creates all tables (tickers, klines, trades, orderbooks, liquidity_depth, slippage, funding_rates, etc.)
2. Creates indexes for optimal query performance
3. Runs all migration files from `migrations/` directory
4. Seeds `exchanges` table with supported exchanges

**Examples:**

```bash
# Initialize database on localhost
DATABASE_URL=postgres://localhost/perps_stats cargo run -- db migrate

# Initialize remote database
DATABASE_URL=postgres://user:password@remote-host:5432/perps cargo run -- db migrate
```

**First-time setup:**
```bash
# 1. Create database
createdb perps_stats

# 2. Initialize schema
DATABASE_URL=postgres://localhost/perps_stats cargo run -- db migrate
```

---

### `db migrate`

Run database migrations only (without re-initializing schema).

**Usage:**

```bash
cargo run -- db migrate
```

**When to use:**
- Upgrading to a new version with schema changes
- Applying new migrations without dropping existing data

---

### `db stats`

Display database statistics including row counts, disk usage, and per-exchange breakdowns.

**Usage:**

```bash
cargo run -- db stats [OPTIONS]
```

**Arguments:**

- `-f, --format <FORMAT>`: Output format (`table` or `json`). Default: `table`
- `--database-url <URL>`: Database connection URL. Default: from `DATABASE_URL` env var

**Examples:**

```bash
# Show statistics in table format
cargo run -- db stats

# Show statistics in JSON format
cargo run -- db stats --format json

# Output to file
cargo run -- db stats --format json > stats.json
```

**Sample output:**

```
Database Statistics
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Overall Statistics:
  Total Tables: 10
  Total Rows: 1,192,692
  Total Size: 125.3 MB

Per-Table Statistics:
  Table              Rows        Size      Latest Record
  ─────────────────────────────────────────────────────────
  tickers            528         48 KB     2025-10-14 18:03:36
  klines             1,191,431   120.5 MB  2025-10-14 18:00:00
  liquidity_depth    533         56 KB     2025-10-14 18:03:36
  slippage           3,731       124 KB    2025-10-14 18:03:36
  funding_rates      0           8 KB      -

Per-Exchange Statistics:
  Exchange    Tickers  Klines     Liquidity  Slippage
  ──────────────────────────────────────────────────────
  aster       264      595,715    266        1,865
  extended    264      595,716    267        1,866

Top Symbols by Data Volume:
  Symbol  Tickers  Klines     Trades  Funding Rates
  ────────────────────────────────────────────────────
  BTC     264      595,715    0       0
  ETH     132      297,858    0       0
  SOL     132      297,858    0       0
```

---

### `db clean`

Clean old data from the database or truncate all tables.

**Usage:**

```bash
cargo run -- db clean [OPTIONS]
```

**Arguments:**

- `--older-than <DAYS>`: Delete data older than N days
- `--truncate`: **WARNING:** Truncate all tables (deletes ALL data)
- `--database-url <URL>`: Database connection URL

**Examples:**

```bash
# Delete data older than 30 days
cargo run -- db clean --older-than 30

# Delete data older than 7 days
cargo run -- db clean --older-than 7

# WARNING: Delete ALL data
cargo run -- db clean --truncate
```

**Use cases:**
- Free up disk space by removing old historical data
- Reset database during development/testing
- Implement data retention policies

---

## Data Ingestion Commands

### `backfill`

Backfill historical market data from exchange REST APIs. Supports intelligent auto-discovery of earliest available data and gap detection to avoid re-fetching existing data.

**Usage:**

```bash
cargo run -- backfill [OPTIONS] --symbols <SYMBOLS>
```

**Arguments:**

- `--exchanges <EXCHANGES>`: Comma-separated list of exchanges. Default: all exchanges
- `-s, --symbols <SYMBOLS>`: **Required.** Comma-separated list of symbols (e.g., `BTC,ETH,SOL`)
- `--data-types <TYPES>`: Data types to backfill (`klines`, `trades`, `funding_rates`). Default: `klines,funding_rates`
- `--intervals <INTERVALS>`: Klines intervals (`5m`, `15m`, `30m`, `1h`, `4h`, `1d`, etc.). Default: `5m,15m,30m,1h,1d`
- `--start-date <DATE>`: Start date (YYYY-MM-DD). If omitted, auto-discovers earliest available data
- `--end-date <DATE>`: End date (YYYY-MM-DD). Default: today
- `--batch-size <SIZE>`: Batch size for database inserts. Default: `1000`
- `--force-refetch`: Force re-fetch all data (disables gap detection)
- `--database-url <URL>`: Database connection URL

**Key Features:**

1. **Auto Start Date Discovery**: If `--start-date` is omitted, automatically discovers earliest available data by searching backwards up to 5 years
2. **Intelligent Gap Detection**: Queries database for existing data and only fetches missing periods
3. **Multi-Exchange Parallel Processing**: All exchanges processed concurrently for maximum speed
4. **Discovery Caching**: Caches earliest available timestamp in `kline_discovery_cache` table to avoid redundant API calls

**Examples:**

**Basic usage - Auto-discover and backfill all available data:**
```bash
# Backfill BTC across all exchanges (auto-discovers start date)
cargo run -- backfill -s BTC

# Backfill multiple symbols
cargo run -- backfill -s BTC,ETH,SOL
```

**Specify exchanges:**
```bash
# Backfill from specific exchanges only
cargo run -- backfill --exchanges binance,hyperliquid -s BTC

# Backfill from single exchange
cargo run -- backfill --exchanges aster -s BTC,ETH
```

**Custom date range:**
```bash
# Backfill specific date range (manual control)
cargo run -- backfill -s BTC --start-date 2024-01-01 --end-date 2024-12-31

# Backfill last 30 days
cargo run -- backfill -s BTC --start-date 2024-09-14 --end-date 2024-10-14
```

**Custom intervals and data types:**
```bash
# Backfill only hourly and daily klines
cargo run -- backfill -s BTC --intervals 1h,1d

# Backfill only funding rates
cargo run -- backfill -s BTC --data-types funding_rates

# Backfill trades (if exchange supports)
cargo run -- backfill -s BTC --data-types trades

# Backfill everything
cargo run -- backfill -s BTC --data-types klines,trades,funding_rates --intervals 5m,15m,30m,1h,4h,1d
```

**Force re-fetch (disable gap detection):**
```bash
# Re-fetch all data even if it exists in database
cargo run -- backfill -s BTC --force-refetch

# Useful for data quality issues or API bugs
cargo run -- backfill -s BTC --start-date 2024-01-01 --end-date 2024-01-31 --force-refetch
```

**With database URL:**
```bash
DATABASE_URL=postgres://localhost/perps_stats cargo run -- backfill -s BTC
```

**How it works:**

1. **Symbol Validation**: Validates symbols against each exchange
2. **Start Date Discovery** (if not specified):
   - Checks `kline_discovery_cache` for cached earliest timestamp
   - If not cached, searches backwards from today up to 5 years
   - Caches result for future backfills
3. **Gap Detection** (if not `--force-refetch`):
   - Queries database for existing klines
   - Identifies missing date ranges
   - Only fetches missing data
4. **Parallel Fetching**:
   - Each exchange runs in parallel tokio task
   - Each data type (klines, funding_rates) runs concurrently
   - Progress tracking shows exchange/data type context
5. **Batch Insertion**:
   - Groups data into batches (default 1000)
   - Uses `INSERT ... ON CONFLICT DO NOTHING` for idempotency

**Performance tips:**

- Use fewer intervals for faster backfill: `--intervals 1h,1d`
- Reduce batch size if hitting memory limits: `--batch-size 500`
- Use `--force-refetch` sparingly (slower, more API calls)
- Let auto-discovery find earliest data (saves manual configuration)

---

### `stream`

Stream real-time market data via WebSocket connections.

**Usage:**

```bash
cargo run -- stream [OPTIONS] --symbols <SYMBOLS>
```

**Arguments:**

- `--exchange <EXCHANGE>`: Exchange to stream from. Default: `binance`
- `-s, --symbols <SYMBOLS>`: **Required.** Comma-separated list of symbols
- `--data-types <TYPES>`: Data types to stream (`ticker`, `trade`, `orderbook`, `fundingrate`, `kline`). Default: `ticker,trade`
- `--klines-interval <INTERVAL>`: Klines interval for kline streaming (required if streaming klines)
- `--batch-size <SIZE>`: Batch size for database inserts. Default: `100`
- `--max-duration <SECONDS>`: Maximum duration in seconds (0 = unlimited). Default: `0`
- `--database-url <URL>`: Database connection URL

**Supported data types by exchange:**

| Exchange | ticker | trade | orderbook | fundingrate | kline |
|----------|:------:|:-----:|:---------:|:-----------:|:-----:|
| Aster | ✓ | ✓ | ✓ | ✓ | - |
| Binance | ✓ | ✓ | ✓ | ✓ | - |
| Bybit | ✓ | ✓ | ✓ | - | - |
| Hyperliquid | ✓ | ✓ | ✓ | - | - |
| KuCoin | ✓ | ✓ | - | - | ✓ |
| Lighter | ✓ | ✓ | ✓ | - | - |
| Paradex | ✓ | ✓ | ✓ | ✓ | - |

**Examples:**

**Basic streaming:**
```bash
# Stream tickers and trades for BTC from Binance
cargo run -- stream -s BTC --data-types ticker,trade

# Stream orderbooks
cargo run -- stream -s BTC --data-types ticker,orderbook
```

**Multi-symbol streaming:**
```bash
# Stream multiple symbols
cargo run -- stream -s BTC,ETH,SOL --data-types ticker,trade
```

**Exchange-specific streaming:**
```bash
# Stream from Hyperliquid
cargo run -- stream --exchange hyperliquid -s BTC --data-types ticker,trade,orderbook

# Stream from Bybit
cargo run -- stream --exchange bybit -s BTC,ETH --data-types ticker,trade

# Stream funding rates from Binance
cargo run -- stream --exchange binance -s BTC --data-types ticker,fundingrate
```

**Klines streaming (KuCoin only):**
```bash
# Stream 1-hour klines from KuCoin
cargo run -- stream --exchange kucoin -s BTC --data-types kline --klines-interval 1h

# Stream 5-minute klines
cargo run -- stream --exchange kucoin -s BTC --data-types kline --klines-interval 5m
```

**With database storage:**
```bash
# Stream and store in database
DATABASE_URL=postgres://localhost/perps_stats cargo run -- stream -s BTC --data-types ticker,trade,orderbook

# Stream for 1 hour then stop
DATABASE_URL=postgres://localhost/perps_stats cargo run -- stream -s BTC --max-duration 3600
```

**Time-limited streaming:**
```bash
# Stream for 60 seconds
cargo run -- stream -s BTC --max-duration 60

# Stream for 1 hour
cargo run -- stream -s BTC --max-duration 3600
```

**Production example:**
```bash
# Stream tickers, trades, and orderbooks for multiple symbols
# Store in database, run indefinitely
DATABASE_URL=postgres://localhost/perps_stats \
  cargo run -- stream \
  --exchange binance \
  -s BTC,ETH,SOL \
  --data-types ticker,trade,orderbook \
  --batch-size 100
```

---

### `start`

**Recommended for production.** Unified data collection service that runs continuously, fetching klines, tickers, liquidity depth, and slippage at regular intervals. Uses REST APIs exclusively for data quality.

**Usage:**

```bash
cargo run -- start [OPTIONS]
```

**Arguments:**

- `--exchanges <EXCHANGES>`: Comma-separated list of exchanges. Default: all exchanges
- `--symbols-file <FILE>`: Path to file containing symbols (one per line). Default: `symbols.txt`
- `--klines-interval <SECONDS>`: Interval for klines fetching in seconds. Default: `60`
- `--report-interval <SECONDS>`: Interval for liquidity/slippage reports in seconds. Default: `30`
- `--klines-timeframes <TIMEFRAMES>`: Comma-separated klines timeframes. Default: `5m,15m,1h`
- `--database-url <URL>`: Database connection URL

**What it does:**

The `start` command runs **three parallel tasks** for each exchange:

1. **Klines Fetching Task** (per timeframe):
   - On first run: Performs intelligent backfill to fill gaps
   - Ongoing: Fetches latest klines at `--klines-interval` (default 60s)
   - Supports multiple timeframes running in parallel

2. **Liquidity & Slippage Task**:
   - Fetches orderbook
   - Calculates liquidity depth at multiple spreads (1, 2.5, 5, 10, 20 bps)
   - Calculates slippage for trade sizes ($1K, $10K, $50K, $100K, $500K)
   - Runs every `--report-interval` (default 30s)

3. **Ticker Task**:
   - Fetches complete 24h ticker statistics
   - Includes best bid/ask, volume, price change, open interest
   - Runs every `--klines-interval` (default 60s)

**Why REST API instead of WebSocket?**
- Complete 24h statistics (WebSocket tickers often have incomplete data)
- More reliable for aggregated metrics
- Easier to handle reconnections and errors
- Consistent data quality across exchanges

**Examples:**

**Basic usage:**
```bash
# Create symbols file
echo -e "BTC\nETH\nSOL" > symbols.txt

# Start data collection for all exchanges
DATABASE_URL=postgres://localhost/perps_stats cargo run -- start
```

**Specific exchanges:**
```bash
# Collect data from Binance and Hyperliquid only
cargo run -- start --exchanges binance,hyperliquid

# Single exchange
cargo run -- start --exchanges aster
```

**Custom timeframes:**
```bash
# Collect hourly, 4-hour, and daily klines only
cargo run -- start --klines-timeframes 1h,4h,1d

# Collect only 15-minute klines
cargo run -- start --klines-timeframes 15m

# Collect many timeframes
cargo run -- start --klines-timeframes 5m,15m,30m,1h,4h,1d
```

**Custom intervals:**
```bash
# Faster klines fetching (every 30 seconds)
cargo run -- start --klines-interval 30

# Faster liquidity reports (every 15 seconds)
cargo run -- start --report-interval 15

# Slower (less frequent) updates
cargo run -- start --klines-interval 120 --report-interval 60
```

**Production deployment:**
```bash
# Full configuration example
DATABASE_URL=postgres://localhost/perps_stats \
  RUST_LOG=perps_stats=info,perps_core=info \
  cargo run -- start \
  --exchanges binance,hyperliquid,bybit \
  --symbols-file production_symbols.txt \
  --klines-interval 60 \
  --report-interval 30 \
  --klines-timeframes 5m,15m,1h,4h,1d
```

**With logging:**
```bash
# Debug logging
RUST_LOG=perps_stats=debug,perps_core=debug cargo run -- start

# Info logging (recommended for production)
RUST_LOG=perps_stats=info cargo run -- start
```

**What happens on first run:**

1. Loads symbols from file
2. Validates symbols against each exchange
3. For each exchange × timeframe combination:
   - Checks database for existing klines
   - If gaps exist, performs backfill to fill them
   - Starts periodic fetching
4. Starts liquidity/slippage calculation tasks
5. Starts ticker fetching tasks
6. Reports progress every `--report-interval`

**Monitoring:**

The command outputs periodic reports showing:
```
[2025-10-14T18:03:32Z INFO] Periodic report (exchange: binance, timeframe: 5m)
  - Klines fetched: 1,234
  - Liquidity snapshots: 456
  - Slippage calculations: 2,280
  - Duration: 30.5s
```

**Use cases:**

- **Production monitoring**: Continuous data collection for live dashboards
- **Historical + Real-time**: Backfills gaps on start, then keeps data current
- **Multi-exchange comparison**: Runs all exchanges in parallel
- **Grafana integration**: Provides consistent data feed for Grafana dashboards

---

## Data Collection Commands

### `run`

Periodic data collection that exports ticker and liquidity data to Excel files. Useful for offline analysis and reporting.

**Usage:**

```bash
cargo run -- run [OPTIONS]
```

**Arguments:**

- `--symbols-file <FILE>`: Path to symbols file. Default: `symbols.txt`
- `--exchanges <EXCHANGES>`: Comma-separated list of exchanges. Default: all exchanges
- `-i, --interval <SECONDS>`: Fetch interval in seconds. Default: `300` (5 minutes)
- `--output-dir <DIR>`: Output directory for Excel files. Default: `./data`
- `-n, --max-snapshots <COUNT>`: Maximum snapshots to collect (0 = unlimited). Default: `0`

**What it does:**

1. Fetches ticker data (price, volume, spread, open interest) for all symbols
2. Fetches liquidity depth (bid/ask depth at multiple spreads)
3. Exports to Excel file with separate sheets per symbol
4. Repeats every `--interval` seconds

**Examples:**

**Basic usage:**
```bash
# Run with default settings (5-minute interval, all exchanges)
cargo run -- run

# Custom symbols file
cargo run -- run --symbols-file my_symbols.txt
```

**Specific exchanges:**
```bash
# Collect from Binance and Bybit only
cargo run -- run --exchanges binance,bybit

# Single exchange
cargo run -- run --exchanges lighter
```

**Custom interval:**
```bash
# Fetch every 30 seconds
cargo run -- run --interval 30

# Fetch every 10 minutes
cargo run -- run --interval 600
```

**Limited snapshots:**
```bash
# Collect 10 snapshots then stop
cargo run -- run --max-snapshots 10 --interval 60

# Collect 100 snapshots (useful for analysis)
cargo run -- run --max-snapshots 100 --interval 300
```

**Custom output directory:**
```bash
# Save to custom directory
cargo run -- run --output-dir ./exports

# Save to dated directory
cargo run -- run --output-dir ./data/$(date +%Y-%m-%d)
```

**Output format:**

Files are saved as: `{exchange}_tickers_liquidity_{timestamp}.xlsx`

Example: `binance_tickers_liquidity_2025-10-14_18-03-32.xlsx`

Each Excel file contains:
- Multiple sheets (one per symbol)
- Ticker data: Exchange, Symbol, Price, Volume, Spread, OI
- Liquidity data: Depth at 1bps, 2.5bps, 5bps, 10bps, 20bps

---

### `market`

Retrieve L1 market data for contracts.

**Usage:**

```bash
cargo run -- market [OPTIONS] --symbols <SYMBOLS>
```

**Arguments:**

-   `-e, --exchange <EXCHANGE>`: The exchange to query. Defaults to `binance`.
-   `-s, --symbols <SYMBOLS>`: A comma-separated list of symbols.
-   `-f, --format <FORMAT>`: The output format (`table`, `json`). Defaults to `table`.
-   `-d, --detailed`: Shows detailed information, including order book depth.
-   `-t, --timeframe <TIMEFRAME>`: The timeframe for statistics (`5m`, `15m`, `1h`, `24h`). Defaults to `24h`.

**Examples:**

-   **Get market data for BTC from Binance:**

    ```bash
    cargo run -- market -s BTC
    ```

-   **Get detailed ETH data from Bybit in JSON format:**

    ```bash
    cargo run -- market --exchange bybit -s ETH --detailed --format json
    ```

### `liquidity`

Retrieve and calculate liquidity depth at multiple spread levels (1, 2.5, 5, 10, 20 basis points).

**Usage:**

```bash
cargo run -- liquidity [OPTIONS] --symbols <SYMBOLS>
```

**Arguments:**

- `-e, --exchange <EXCHANGE>`: The exchange to query. Default: all exchanges
- `-s, --symbols <SYMBOLS>`: **Required.** Comma-separated list of symbols
- `-f, --format <FORMAT>`: Output format (`table`, `json`, `csv`, `excel`). Default: `table`
- `-o, --output <OUTPUT>`: Output file path for CSV/Excel format
- `-d, --output-dir <OUTPUT_DIR>`: Output directory for files
- `-i, --interval <INTERVAL>`: Fetch interval in seconds (for periodic fetching)
- `-n, --max-snapshots <MAX_SNAPSHOTS>`: Maximum snapshots to collect (0 = unlimited)
- `--database-url <URL>`: Database connection URL (for periodic storage)

**What it calculates:**

- Bid/Ask depth at 1 bps (0.01%)
- Bid/Ask depth at 2.5 bps (0.025%)
- Bid/Ask depth at 5 bps (0.05%)
- Bid/Ask depth at 10 bps (0.10%)
- Bid/Ask depth at 20 bps (0.20%)
- Total depth (bid + ask) at each level
- Imbalance ratio (bid vs ask)

**Examples:**

**Basic usage:**
```bash
# Get liquidity data for BTC from all exchanges
cargo run -- liquidity -s BTC

# Multiple symbols
cargo run -- liquidity -s BTC,ETH,SOL

# Specific exchange
cargo run -- liquidity --exchange binance -s BTC
```

**Different output formats:**
```bash
# JSON output
cargo run -- liquidity -s BTC --format json

# CSV output (separate file per symbol)
cargo run -- liquidity -s BTC,ETH --format csv --output-dir ./liquidity_data

# Excel output (single file, multiple sheets)
cargo run -- liquidity -s BTC,ETH --format excel --output liquidity.xlsx
```

**Periodic fetching with database storage:**
```bash
# Fetch every 30 seconds and store in database
DATABASE_URL=postgres://localhost/perps_stats \
  cargo run -- liquidity \
  -s BTC \
  --interval 30 \
  --max-snapshots 100

# Continuous monitoring
DATABASE_URL=postgres://localhost/perps_stats \
  cargo run -- liquidity \
  -s BTC,ETH \
  --interval 60
```

**Multi-exchange comparison:**
```bash
# Compare liquidity across all exchanges
cargo run -- liquidity -s BTC

# Export comparison to Excel
cargo run -- liquidity -s BTC --format excel --output btc_liquidity.xlsx
```

**Use cases:**
- Monitor orderbook depth for trading decisions
- Compare liquidity across exchanges
- Identify best venue for large trades
- Track liquidity changes over time

### `ticker`

Retrieve comprehensive ticker data including 24h statistics, best bid/ask, volume, and open interest.

**Usage:**

```bash
cargo run -- ticker [OPTIONS] --symbols <SYMBOLS>
```

**Arguments:**

- `-e, --exchange <EXCHANGE>`: The exchange to query. Default: all exchanges
- `-s, --symbols <SYMBOLS>`: **Required.** Comma-separated list of symbols
- `-f, --format <FORMAT>`: Output format (`table`, `json`, `csv`, `excel`). Default: `table`
- `-o, --output <OUTPUT>`: Output file path for CSV/Excel format
- `-d, --output-dir <OUTPUT_DIR>`: Output directory for files
- `-i, --interval <INTERVAL>`: Fetch interval in seconds (for periodic fetching)
- `-n, --max-snapshots <MAX_SNAPSHOTS>`: Maximum snapshots to collect (0 = unlimited)
- `--database-url <URL>`: Database connection URL (for periodic storage)

**What it includes:**

- Best bid/ask prices and quantities
- Mark price, index price, last price
- 24h price change percentage
- 24h volume
- Open interest and open interest notional
- Funding rate (if available)

**Examples:**

**Basic usage:**
```bash
# Get ticker data for BTC from all exchanges
cargo run -- ticker -s BTC

# Multiple symbols
cargo run -- ticker -s BTC,ETH,SOL

# Specific exchange
cargo run -- ticker --exchange binance -s BTC
```

**Different output formats:**
```bash
# JSON output
cargo run -- ticker -s BTC --format json

# CSV output
cargo run -- ticker -s BTC,ETH --format csv --output tickers.csv

# Excel output
cargo run -- ticker -s BTC,ETH --format excel --output tickers.xlsx
```

**Periodic fetching with database storage:**
```bash
# Fetch every 60 seconds and store in database
DATABASE_URL=postgres://localhost/perps_stats \
  cargo run -- ticker \
  -s BTC,ETH \
  --interval 60 \
  --max-snapshots 100

# Continuous fetching (unlimited snapshots)
DATABASE_URL=postgres://localhost/perps_stats \
  cargo run -- ticker \
  -s BTC \
  --interval 30
```

**Multi-exchange comparison:**
```bash
# Compare BTC ticker across all exchanges (table format)
cargo run -- ticker -s BTC

# Export to Excel for analysis
cargo run -- ticker -s BTC --format excel --output btc_comparison.xlsx
```
