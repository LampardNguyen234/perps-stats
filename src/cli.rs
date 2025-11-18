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
    Backfill(crate::commands::backfill::BackfillArgs),

    /// Stream real-time data from exchanges
    Stream(crate::commands::stream::StreamArgs),

    /// Start unified data collection service (WebSocket streaming + klines fetching + report generation)
    Start(crate::commands::start::StartArgs),

    /// Start the REST API server
    Serve {
        /// Port to listen on
        #[arg(short, long, default_value = "8080")]
        port: u16,

        /// Host to bind to
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
    },

    /// Run periodic data collection service
    Run {
        /// Path to symbols file (one symbol per line or comma-separated)
        #[arg(short, long, default_value = "symbols.txt")]
        symbols_file: String,

        /// Comma-separated exchange names (e.g., binance,bybit). If not specified, fetches from all supported exchanges in parallel.
        #[arg(short, long)]
        exchanges: Option<String>,

        /// Fetch interval in seconds
        #[arg(short, long, default_value = "300")]
        interval: u64,

        /// Output directory for Excel files
        #[arg(short, long, default_value = "./data")]
        output_dir: String,

        /// Maximum number of snapshots to collect (0 = unlimited)
        #[arg(short = 'n', long, default_value = "0")]
        max_snapshots: usize,
    },

    /// Database operations
    Db {
        #[command(subcommand)]
        command: DbCommands,

        /// Database URL (e.g., postgres://user:pass@localhost/perps)
        #[arg(long, env = "DATABASE_URL", global = true)]
        database_url: Option<String>,
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

    /// Retrieve liquidity depth for contracts
    Liquidity {
        /// Exchange name (e.g., binance)
        #[arg(short, long)]
        exchange: Option<String>,

        /// Comma-separated list of symbols (e.g., BTC-USDT,ETH-USDT)
        #[arg(short, long)]
        symbols: String,

        /// Output format (table, json, csv, excel). When interval is set and format is not specified, data is saved to database.
        #[arg(short, long, default_value = "table")]
        format: String,

        /// Output file path for CSV format (e.g., liquidity_data.csv)
        #[arg(short, long)]
        output: Option<String>,

        /// Output directory for CSV files (default: current directory)
        #[arg(short = 'd', long)]
        output_dir: Option<String>,

        /// Fetch interval in seconds (for periodic fetching)
        #[arg(short, long)]
        interval: Option<u64>,

        /// Maximum number of snapshots to collect (0 = unlimited)
        #[arg(short = 'n', long, default_value = "0")]
        max_snapshots: usize,

        /// Database URL for storing data (required when using --interval without --format)
        #[arg(long, env = "DATABASE_URL")]
        database_url: Option<String>,

        /// Exclude fees from slippage calculations (force fee=None)
        #[arg(long)]
        exclude_fees: bool,

        /// Override taker fee with custom value (e.g., 0.0005 for 0.05%)
        #[arg(long)]
        override_fee: Option<f64>,
    },

    /// Retrieve ticker data for contracts
    Ticker {
        /// Exchange name (e.g., binance)
        #[arg(short, long, default_value = "binance")]
        exchange: String,

        /// Comma-separated list of symbols (e.g., BTC-USDT,ETH-USDT)
        #[arg(short, long)]
        symbols: String,

        /// Output format (table, json, csv, excel). When interval is set and format is not specified, data is saved to database.
        #[arg(short, long, default_value = "table")]
        format: String,

        /// Output file path for CSV/Excel format (e.g., ticker_data.csv or ticker_data.xlsx)
        #[arg(short, long)]
        output: Option<String>,

        /// Output directory for files (default: current directory)
        #[arg(short = 'd', long)]
        output_dir: Option<String>,

        /// Fetch interval in seconds (for periodic fetching)
        #[arg(short, long)]
        interval: Option<u64>,

        /// Maximum number of snapshots to collect (0 = unlimited)
        #[arg(short = 'n', long, default_value = "0")]
        max_snapshots: usize,

        /// Database URL for storing data (required when using --interval without --format)
        #[arg(long, env = "DATABASE_URL")]
        database_url: Option<String>,
    },

    /// Export all database tables to CSV or Excel files
    Export(crate::commands::export::ExportArgs),
}

#[derive(Subcommand)]
pub enum DbCommands {
    /// Run database migrations to initialize or update schema
    Migrate,

    /// Clean the database (WARNING: deletes all data)
    Clean {
        /// Delete data older than N days
        #[arg(long)]
        older_than: Option<i32>,

        /// Drop partitions older than N days
        #[arg(long)]
        drop_partitions_older_than: Option<i32>,

        /// Truncate all tables (WARNING: deletes ALL data)
        #[arg(long)]
        truncate: bool,
    },

    /// Show database statistics
    Stats {
        /// Output format (table, json)
        #[arg(short, long, default_value = "table")]
        format: String,
    },

    /// Update exchange fees (maker and taker)
    UpdateFees {
        /// Exchange name (e.g., binance, hyperliquid)
        #[arg(short, long)]
        exchange: String,

        /// Maker fee as decimal (e.g., 0.0001 for 0.01%)
        #[arg(short, long)]
        maker: Option<f64>,

        /// Taker fee as decimal (e.g., 0.0002 for 0.02%)
        #[arg(short, long)]
        taker: Option<f64>,
    },
}
