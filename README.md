# perps-stats

A Rust-based backend service for retrieving and serving perpetual futures (perps) market data from various cryptocurrency exchanges. It supports historical data backfilling, real-time streaming, and serving aggregated statistics via REST API.

## Features

- **Multi-Exchange Support**: Abstracted interface for connecting to multiple exchanges (KuCoin, Binance, Bybit, etc.)
- **Historical Data Backfilling**: Fetch and store historical market data via REST APIs
- **Real-Time Streaming**: WebSocket-based real-time data ingestion
- **REST API**: Serve market data and calculated metrics
- **Time-Series Database**: PostgreSQL with TimescaleDB for optimized time-series queries
- **Market Analytics**: Calculate VWAP, slippage, market depth, and other metrics

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
git clone https://github.com/yourusername/perps-stats.git
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
cargo run -- db init
```

### Usage

#### Backfill Historical Data
```bash
cargo run -- backfill --exchange kucoin --symbols BTC,ETH --from 2024-01-01 --to 2024-12-31
```

#### Stream Real-Time Data
```bash
cargo run -- stream --exchange kucoin --symbols BTC,ETH --data trades,orderbook
```

#### Start the API Server
```bash
cargo run -- serve --port 8080
```

#### Run All Services
```bash
cargo run -- run --port 8080
```

#### Retrieve Market Data
```bash
# Get L1 market data for specific symbols (default: 24h timeframe)
cargo run -- market --exchange binance --symbols BTC-USDT,ETH-USDT

# Use custom timeframe (5m, 15m, 30m, 1h, 4h, 24h)
cargo run -- market --exchange binance --symbols BTC-USDT --timeframe 1h

# Show detailed orderbook depth
cargo run -- market --exchange binance --symbols BTC-USDT --detailed

# Output as JSON
cargo run -- market --exchange binance --symbols BTC-USDT --format json
```

#### Database Operations
```bash
# Initialize database schema
cargo run -- db init

# Run migrations
cargo run -- db migrate

# Show database statistics
cargo run -- db stats

# Clean database (WARNING: deletes all data)
cargo run -- db clean
```

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
