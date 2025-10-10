mod cli;
mod commands;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Commands, DbCommands};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<()> {
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
        Commands::Backfill {
            exchange,
            symbols,
            from,
            to,
        } => {
            commands::backfill::execute(exchange, symbols, from, to).await?;
        }
        Commands::Stream {
            exchange,
            symbols,
            data,
        } => {
            commands::stream::execute(exchange, symbols, data).await?;
        }
        Commands::Serve { host, port } => {
            commands::serve::execute(host, port).await?;
        }
        Commands::Run { port } => {
            commands::run::execute(port).await?;
        }
        Commands::Db { command } => match command {
            DbCommands::Init => commands::db::init().await?,
            DbCommands::Migrate => commands::db::migrate().await?,
            DbCommands::Clean => commands::db::clean().await?,
            DbCommands::Stats => commands::db::stats().await?,
        },
        Commands::Market {
            exchange,
            symbols,
            format,
            detailed,
            timeframe,
        } => {
            commands::market::execute(exchange, symbols, format, detailed, timeframe).await?;
        }
        Commands::Liquidity {
            exchange,
            symbols,
            format,
            output,
            output_dir,
            interval,
            max_snapshots,
        } => {
            commands::liquidity::execute(exchange, symbols, format, output, output_dir, interval, max_snapshots).await?;
        }
        Commands::Ticker {
            exchange,
            symbols,
            format,
            output,
            output_dir,
            interval,
            max_snapshots,
        } => {
            commands::ticker::execute(exchange, symbols, format, output, output_dir, interval, max_snapshots).await?;
        }
    }

    Ok(())
}
