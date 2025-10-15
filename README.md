# perps-stats

A Rust-based backend service for retrieving and serving perpetual futures (perps) market data from various cryptocurrency exchanges. It supports historical data backfilling, real-time streaming, and serving aggregated statistics via REST API.

## Features

- **Multi-Exchange Support**: Aster, Binance, Bybit, Extended, Hyperliquid, KuCoin, Lighter, Pacifica, Paradex
- **Historical Data Backfilling**: Intelligent backfill with auto-discovery and gap detection
- **Real-Time Streaming**: WebSocket-based data ingestion for tickers, trades, orderbooks, funding rates
- **Unified Data Collection**: Automated periodic fetching with the `start` command
- **Time-Series Database**: PostgreSQL with optimized queries and caching
- **Market Analytics**: Calculate spread, VWAP, slippage, liquidity depth, and funding rate statistics
- **Data Export**: Export to CSV, Excel, JSON formats
- **Grafana Integration**: Pre-built dashboards for visualization

## Architecture

The project is organized as a Cargo workspace with the following crates:

- `perps-core`: Core domain types and the `IPerps` trait
- `perps-exchanges`: Exchange-specific implementations
- `perps-database`: Database repository pattern with sqlx
- `perps-aggregator`: Business logic for market calculations

For detailed architecture information, see [docs/architecture.md](docs/architecture.md).

## Getting Started

### Prerequisites

- Rust 1.70+ (install via [rustup](https://rustup.rs/))
- PostgreSQL 14+ with TimescaleDB extension
- Docker (optional, for running PostgreSQL)

### Installation

1. Clone the repository:
```bash
git clone https://github.com/LampardNguyen234/perps-stats.git
cd perps-stats
```

2. Build the project:
```bash
cargo build --release
```

3. Set up environment variables:
```bash
cp .env.example .env
# Edit .env with your database credentials
```

4. Initialize the database:
```bash
cargo run -- db migrate
```

### Quick Start

#### 1. Initialize Database
```bash
DATABASE_URL=postgres://localhost/perps_stats cargo run -- db migrate
```

#### 2. Start Data Collection (Recommended)
```bash
# Create symbols file
echo -e "BTC,ETH,SOL" > symbols.txt

# Start unified data collection service (all exchanges)
DATABASE_URL=postgres://localhost/perps_stats cargo run -- start
```

This will:
- Calculate liquidity depth and slippage every 30 seconds
- Fetch tickers with 24h statistics every 60 seconds
- Run across all configured exchanges in parallel

### Common Commands

#### Backfill Historical Data
```bash
# Auto-discover and backfill all available data for BTC
cargo run -- backfill -s BTC

# Backfill specific date range
cargo run -- backfill -s BTC,ETH --start-date 2024-01-01 --end-date 2024-12-31

# Backfill specific exchanges
cargo run -- backfill --exchanges binance,hyperliquid -s BTC

# Custom intervals and data types
cargo run -- backfill -s BTC --intervals 1h,4h,1d --data-types klines,funding_rates
```

#### Stream Real-Time Data
```bash
# Stream tickers and trades
cargo run -- stream -s BTC --data-types ticker,trade

# Stream with database storage
DATABASE_URL=postgres://localhost/perps_stats cargo run -- stream -s BTC,ETH --data-types ticker,trade,orderbook

# Stream from specific exchange
cargo run -- stream --exchange binance -s BTC --data-types ticker,trade --max-duration 3600
```

#### Periodic Excel Exports
```bash
# Export ticker + liquidity data every 5 minutes
cargo run -- run

# Custom exchanges and interval
cargo run -- run --exchanges binance,bybit --interval 30 --max-snapshots 10
```

#### Retrieve Market Data
```bash
# Get ticker data for BTC across all exchanges
cargo run -- ticker -s BTC

# Get liquidity depth
cargo run -- liquidity -s BTC --exchange binance

# Get detailed market data
cargo run -- market -s BTC --exchange hyperliquid --detailed
```

#### Database Operations
```bash
# Initialize database schema and run migrations
DATABASE_URL=postgres://localhost/perps_stats cargo run -- db migrate

# Show database statistics
cargo run -- db stats

# Show statistics in JSON format
cargo run -- db stats --format json

# Clean old data (older than 30 days)
cargo run -- db clean --older-than 30

# WARNING: Delete all data
cargo run -- db clean --truncate
```

## Supported Exchanges

| Exchange    | REST API | WebSocket | Symbol Format | Notes                     |
|-------------|:--------:|:---------:|---------------|---------------------------|
| Aster       |    ✓     |     -     | BTCUSDT       | Binance-compatible        |
| Binance     |    ✓     |     -     | BTCUSDT       | Full support              |
| Bybit       |    ✓     |     -     | BTCUSDT       | Full support              |
| Extended    |    ✓     |     -     | BTC-USD       | Starknet L2 DEX           |
| Hyperliquid |    ✓     |     -     | BTC           | POST-based API            |
| KuCoin      |    ✓     |     -     | XBTUSDTM      | Full support              |
| Lighter     |    ✓     |     -     | BTC           | Uses market_id internally |
| Pacifica    |    ✓     |     -     | BTC           | StarkEx L2 DEX            |
| Paradex     |    ✓     |     -     | BTC-USD-PERP  | Full support              |

## Documentation

### General
- **[TUTORIALS.md](TUTORIALS.md)** - Comprehensive command tutorials
- **[CLAUDE.md](CLAUDE.md)** - Developer guide and architecture details
- **[docs/DOCKER_DEPLOYMENT.md](docs/DOCKER_DEPLOYMENT.md)** - Docker deployment guide

## Development

### Running Tests
```bash
# Run all tests
cargo test

# Run tests for a specific crate
cargo test -p perps-aggregator

# Run a specific test
cargo test test_calculate_vwap
```

### Code Quality
```bash
# Format code
cargo fmt

# Run linter
cargo clippy

# Check without building
cargo check
```

### Project Structure
```
perps-stats/
├── crates/
│   ├── perps-core/          # Core types and traits
│   ├── perps-exchanges/     # Exchange implementations
│   ├── perps-database/      # Database layer
│   └── perps-aggregator/    # Business logic
├── src/
│   ├── main.rs              # CLI entry point
│   ├── cli.rs               # Command definitions
│   └── commands/            # Command implementations
├── docs/                    # Documentation
└── migrations/              # Database migrations
```

## License

MIT
