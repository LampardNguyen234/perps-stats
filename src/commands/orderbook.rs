use anyhow::{Context, Result};
use chrono::{NaiveDate, NaiveTime, Utc};
use clap::{Args, Subcommand};
use perps_database::OrderbookParquetReader;
use std::path::PathBuf;

#[derive(Args)]
pub struct OrderbookArgs {
    #[command(subcommand)]
    pub command: OrderbookCommands,
}

#[derive(Subcommand)]
pub enum OrderbookCommands {
    /// Read full orderbook snapshots from Parquet files
    Read {
        /// Exchange name (e.g., binance)
        #[arg(short, long)]
        exchange: String,

        /// Symbol (e.g., BTC)
        #[arg(short, long)]
        symbol: String,

        /// Date (YYYY-MM-DD)
        #[arg(short, long)]
        date: String,

        /// Optional time for closest snapshot (HH:MM:SS)
        #[arg(short, long)]
        time: Option<String>,

        /// Output format (json, table, csv)
        #[arg(short, long, default_value = "table")]
        format: String,

        /// Parquet data directory
        #[arg(long, env = "PARQUET_DIR", default_value = "data/parquet")]
        parquet_dir: String,
    },

    /// List available Parquet orderbook files
    List {
        /// Filter by exchange
        #[arg(short, long)]
        exchange: Option<String>,

        /// Filter by symbol
        #[arg(short, long)]
        symbol: Option<String>,

        /// Start date filter (YYYY-MM-DD)
        #[arg(long)]
        from: Option<String>,

        /// End date filter (YYYY-MM-DD)
        #[arg(long)]
        to: Option<String>,

        /// Parquet data directory
        #[arg(long, env = "PARQUET_DIR", default_value = "data/parquet")]
        parquet_dir: String,
    },

    /// Export date range to a single merged Parquet file
    Export {
        /// Exchange name
        #[arg(short, long)]
        exchange: String,

        /// Symbol
        #[arg(short, long)]
        symbol: String,

        /// Start date (YYYY-MM-DD)
        #[arg(long)]
        from: String,

        /// End date (YYYY-MM-DD)
        #[arg(long)]
        to: String,

        /// Output file path
        #[arg(short, long)]
        output: String,

        /// Parquet data directory
        #[arg(long, env = "PARQUET_DIR", default_value = "data/parquet")]
        parquet_dir: String,
    },
}

pub async fn execute(args: OrderbookArgs) -> Result<()> {
    match args.command {
        OrderbookCommands::Read {
            exchange,
            symbol,
            date,
            time,
            format,
            parquet_dir,
        } => execute_read(&exchange, &symbol, &date, time.as_deref(), &format, &parquet_dir),
        OrderbookCommands::List {
            exchange,
            symbol,
            from,
            to,
            parquet_dir,
        } => execute_list(
            exchange.as_deref(),
            symbol.as_deref(),
            from.as_deref(),
            to.as_deref(),
            &parquet_dir,
        ),
        OrderbookCommands::Export {
            exchange,
            symbol,
            from,
            to,
            output,
            parquet_dir,
        } => execute_export(&exchange, &symbol, &from, &to, &output, &parquet_dir),
    }
}

fn execute_read(
    exchange: &str,
    symbol: &str,
    date_str: &str,
    time_str: Option<&str>,
    format: &str,
    parquet_dir: &str,
) -> Result<()> {
    let reader = OrderbookParquetReader::new(parquet_dir);
    let date = NaiveDate::parse_from_str(date_str, "%Y-%m-%d")
        .with_context(|| format!("invalid date: {}", date_str))?;

    let orderbooks = if let Some(time_s) = time_str {
        let time = NaiveTime::parse_from_str(time_s, "%H:%M:%S")
            .with_context(|| format!("invalid time: {}", time_s))?;
        let dt = date
            .and_time(time)
            .and_utc();
        match reader.read_orderbook_at(exchange, symbol, dt)? {
            Some(ob) => vec![ob],
            None => {
                println!("No orderbook found near {}", dt);
                return Ok(());
            }
        }
    } else {
        reader.read_orderbooks(exchange, symbol, date)?
    };

    if orderbooks.is_empty() {
        println!("No orderbook data found for {}/{} on {}", exchange, symbol, date);
        return Ok(());
    }

    match format {
        "json" => {
            let json = serde_json::to_string_pretty(&orderbooks)?;
            println!("{}", json);
        }
        "csv" => {
            println!("timestamp,side,level,price,quantity");
            for ob in &orderbooks {
                for (i, level) in ob.bids.iter().enumerate() {
                    println!(
                        "{},bid,{},{},{}",
                        ob.timestamp.timestamp(),
                        i,
                        level.price,
                        level.quantity
                    );
                }
                for (i, level) in ob.asks.iter().enumerate() {
                    println!(
                        "{},ask,{},{},{}",
                        ob.timestamp.timestamp(),
                        i,
                        level.price,
                        level.quantity
                    );
                }
            }
        }
        _ => {
            // table format
            println!(
                "Found {} snapshots for {}/{} on {}",
                orderbooks.len(),
                exchange,
                symbol,
                date
            );
            for ob in &orderbooks {
                println!("\n--- {} (ts={}) ---", ob.symbol, ob.timestamp);
                println!("  Bids: {} levels", ob.bids.len());
                for (i, level) in ob.bids.iter().take(5).enumerate() {
                    println!("    [{}] {} @ {}", i, level.quantity, level.price);
                }
                if ob.bids.len() > 5 {
                    println!("    ... and {} more", ob.bids.len() - 5);
                }
                println!("  Asks: {} levels", ob.asks.len());
                for (i, level) in ob.asks.iter().take(5).enumerate() {
                    println!("    [{}] {} @ {}", i, level.quantity, level.price);
                }
                if ob.asks.len() > 5 {
                    println!("    ... and {} more", ob.asks.len() - 5);
                }
            }
        }
    }

    Ok(())
}

fn execute_list(
    exchange: Option<&str>,
    symbol: Option<&str>,
    from_str: Option<&str>,
    to_str: Option<&str>,
    parquet_dir: &str,
) -> Result<()> {
    let reader = OrderbookParquetReader::new(parquet_dir);
    let pairs = reader.list_pairs()?;

    if pairs.is_empty() {
        println!("No Parquet orderbook data found in {}", parquet_dir);
        return Ok(());
    }

    let from_date = from_str
        .map(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d"))
        .transpose()
        .context("invalid --from date")?;
    let to_date = to_str
        .map(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d"))
        .transpose()
        .context("invalid --to date")?;

    for (ex, sym) in &pairs {
        if let Some(e) = exchange {
            if ex != e {
                continue;
            }
        }
        if let Some(s) = symbol {
            if sym != s {
                continue;
            }
        }

        let dates = reader.list_dates(ex, sym)?;
        let filtered_dates: Vec<_> = dates
            .into_iter()
            .filter(|d| {
                if let Some(from) = from_date {
                    if *d < from {
                        return false;
                    }
                }
                if let Some(to) = to_date {
                    if *d > to {
                        return false;
                    }
                }
                true
            })
            .collect();

        if filtered_dates.is_empty() {
            continue;
        }

        println!(
            "{}/{}: {} files ({} to {})",
            ex,
            sym,
            filtered_dates.len(),
            filtered_dates.first().unwrap_or(&Utc::now().date_naive()),
            filtered_dates.last().unwrap_or(&Utc::now().date_naive()),
        );
    }

    Ok(())
}

fn execute_export(
    exchange: &str,
    symbol: &str,
    from_str: &str,
    to_str: &str,
    output: &str,
    parquet_dir: &str,
) -> Result<()> {
    use perps_database::parquet::schema::{orderbook_to_rows, rows_to_record_batch, orderbook_arrow_schema};
    use parquet::arrow::ArrowWriter;
    use parquet::basic::Compression;
    use parquet::file::properties::WriterProperties;
    use std::sync::Arc;

    let reader = OrderbookParquetReader::new(parquet_dir);
    let from_date = NaiveDate::parse_from_str(from_str, "%Y-%m-%d")
        .with_context(|| format!("invalid --from date: {}", from_str))?;
    let to_date = NaiveDate::parse_from_str(to_str, "%Y-%m-%d")
        .with_context(|| format!("invalid --to date: {}", to_str))?;

    let dates = reader.list_dates(exchange, symbol)?;
    let filtered_dates: Vec<_> = dates
        .into_iter()
        .filter(|d| *d >= from_date && *d <= to_date)
        .collect();

    if filtered_dates.is_empty() {
        println!(
            "No data found for {}/{} between {} and {}",
            exchange, symbol, from_str, to_str
        );
        return Ok(());
    }

    // Create output directory if needed
    let output_path = PathBuf::from(output);
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create output directory: {}", parent.display()))?;
    }

    let schema = Arc::new(orderbook_arrow_schema());
    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(Default::default()))
        .set_max_row_group_size(8192)
        .build();

    let file = std::fs::File::create(&output_path)
        .with_context(|| format!("failed to create output file: {}", output))?;
    let mut writer = ArrowWriter::try_new(file, schema, Some(props))
        .context("failed to create ArrowWriter")?;

    let mut total_snapshots = 0usize;
    for date in &filtered_dates {
        let orderbooks = reader.read_orderbooks(exchange, symbol, *date)?;
        total_snapshots += orderbooks.len();
        for ob in &orderbooks {
            let rows = orderbook_to_rows(exchange, ob);
            if !rows.is_empty() {
                let batch = rows_to_record_batch(&rows)?;
                writer.write(&batch)?;
            }
        }
    }

    writer.close().context("failed to close ArrowWriter")?;

    println!(
        "Exported {} snapshots ({} days) to {}",
        total_snapshots,
        filtered_dates.len(),
        output
    );

    Ok(())
}
