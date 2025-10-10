use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "perps-stats")]
#[command(author, version, about, long_about = None)]
#[command(propagate_version = true)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Backfill historical data from exchanges
    Backfill {
        /// Exchange name (e.g., kucoin, binance)
        #[arg(short, long)]
        exchange: String,

        /// Comma-separated list of symbols (e.g., BTC,ETH)
        #[arg(short, long)]
        symbols: String,

        /// Start date (format: YYYY-MM-DD)
        #[arg(long)]
        from: Option<String>,

        /// End date (format: YYYY-MM-DD)
        #[arg(long)]
        to: Option<String>,
    },

    /// Stream real-time data from exchanges
    Stream {
        /// Exchange name (e.g., kucoin, binance)
        #[arg(short, long)]
        exchange: String,

        /// Comma-separated list of symbols (e.g., BTC,ETH)
        #[arg(short, long)]
        symbols: String,

        /// Data types to stream (trades, orderbook, ticker)
        #[arg(short, long, default_value = "trades,orderbook")]
        data: String,
    },

    /// Start the REST API server
    Serve {
        /// Port to listen on
        #[arg(short, long, default_value = "8080")]
        port: u16,

        /// Host to bind to
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
    },

    /// Run all services (backfill, stream, and serve)
    Run {
        /// Port for the API server
        #[arg(short, long, default_value = "8080")]
        port: u16,
    },

    /// Database operations
    Db {
        #[command(subcommand)]
        command: DbCommands,
    },

    /// Retrieve L1 market data for contracts
    Market {
        /// Exchange name (e.g., binance)
        #[arg(short, long, default_value = "binance")]
        exchange: String,

        /// Comma-separated list of symbols (e.g., BTC-USDT,ETH-USDT)
        #[arg(short, long)]
        symbols: String,

        /// Output format (table, json)
        #[arg(short, long, default_value = "table")]
        format: String,

        /// Show detailed information (orderbook depth)
        #[arg(short, long)]
        detailed: bool,

        /// Timeframe for statistics (5m, 15m, 30m, 1h, 4h, 24h)
        #[arg(short, long, default_value = "24h")]
        timeframe: String,
    },
}

#[derive(Subcommand)]
pub enum DbCommands {
    /// Initialize the database schema
    Init,

    /// Run migrations
    Migrate,

    /// Clean the database (WARNING: deletes all data)
    Clean,

    /// Show database statistics
    Stats,
}
