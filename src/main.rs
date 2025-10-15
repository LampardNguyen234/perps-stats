mod cli;
mod commands;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Commands, DbCommands};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<()> {
    // Load environment variables from .env file (if it exists)
    // This is done before any other initialization to ensure all components can access env vars
    if let Err(e) = dotenvy::dotenv() {
        // Only log if the error is NOT "file not found" - it's okay if .env doesn't exist
        if !e.to_string().contains("not found") {
            eprintln!("Warning: Failed to load .env file: {}", e);
        }
    }

    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "perps_stats=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Backfill(args) => {
            commands::backfill::execute(args).await?;
        }
        Commands::Stream(args) => {
            commands::stream::execute(args).await?;
        }
        Commands::Start(args) => {
            commands::start::execute(args).await?;
        }
        Commands::Serve { host, port } => {
            commands::serve::execute(host, port).await?;
        }
        Commands::Run {
            symbols_file,
            exchanges,
            interval,
            output_dir,
            max_snapshots,
        } => {
            commands::run::execute(commands::run::RunArgs {
                symbols_file,
                exchanges,
                interval,
                output_dir,
                max_snapshots,
            })
            .await?;
        }
        Commands::Db { command, database_url } => match command {
            DbCommands::Migrate => commands::db::migrate(database_url).await?,
            DbCommands::Clean {
                older_than,
                drop_partitions_older_than,
                truncate,
            } => {
                commands::db::clean(database_url, older_than, drop_partitions_older_than, truncate)
                    .await?
            }
            DbCommands::Stats { format } => commands::db::stats(database_url, &format).await?,
        },
        Commands::Market {
            exchange,
            symbols,
            format,
            detailed,
            timeframe,
        } => {
            commands::market::execute(commands::market::MarketArgs {
                exchange,
                symbols,
                format,
                detailed,
                timeframe,
            })
            .await?;
        }
        Commands::Liquidity {
            exchange,
            symbols,
            format,
            output,
            output_dir,
            interval,
            max_snapshots,
            database_url,
        } => {
            commands::liquidity::execute(commands::liquidity::LiquidityArgs {
                exchange,
                symbols,
                format,
                output,
                output_dir,
                interval,
                max_snapshots,
                database_url,
            })
            .await?;
        }
        Commands::Ticker {
            exchange,
            symbols,
            format,
            output,
            output_dir,
            interval,
            max_snapshots,
            database_url,
        } => {
            commands::ticker::execute(commands::ticker::TickerArgs {
                exchange,
                symbols,
                format,
                output,
                output_dir,
                interval,
                max_snapshots,
                database_url,
            })
            .await?;
        }
        Commands::Export(args) => {
            commands::export::execute(args).await?;
        }
    }

    Ok(())
}
