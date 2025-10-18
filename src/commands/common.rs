use anyhow::Result;
use perps_aggregator::IAggregator;
use perps_core::types::LiquidityDepthStats;
use perps_core::IPerps;
use perps_database::{PgPool, PostgresRepository};
use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use tokio::time::{interval, Duration};

/// Configuration for periodic fetcher output
pub enum OutputConfig {
    File {
        output_file: String,
        output_dir: Option<String>,
        format: String,
    },
    Database {
        repository: PostgresRepository,
    },
}

/// Validate symbols against the exchange - filter out unsupported symbols
pub async fn validate_symbols(client: &dyn IPerps, symbols: &[String]) -> Result<Vec<String>> {
    let mut valid_symbols = Vec::new();
    let mut invalid_symbols = Vec::new();

    for symbol in symbols {
        match client.is_supported(symbol).await {
            Ok(true) => {
                tracing::debug!("✓ Symbol {} is supported on {}", symbol, client.get_name());
                valid_symbols.push(symbol.clone());
            }
            Ok(false) => {
                tracing::warn!(
                    "✗ Symbol {} is not supported on {} - skipping",
                    symbol,
                    client.get_name()
                );
                invalid_symbols.push(symbol.clone());
            }
            Err(e) => {
                tracing::error!(
                    "Failed to check if symbol {} is supported: {} - skipping",
                    symbol,
                    e
                );
                invalid_symbols.push(symbol.clone());
            }
        }
    }

    // Log summary
    if !invalid_symbols.is_empty() {
        eprintln!(
            "Warning: {} symbol(s) not supported on {}: {}",
            invalid_symbols.len(),
            client.get_name(),
            invalid_symbols.join(", ")
        );
    }

    if !valid_symbols.is_empty() {
        tracing::info!(
            "Validated {} symbol(s) on {}: {}",
            valid_symbols.len(),
            client.get_name(),
            valid_symbols.join(", ")
        );
    }

    Ok(valid_symbols)
}

/// Determine output configuration from command-line arguments
///
/// This function validates the output configuration for periodic fetching:
/// - If format is "table" (default), requires database URL
/// - If format is "csv" or "excel", requires output file
/// - Creates output directory if specified
pub async fn determine_output_config(
    format: &str,
    output: Option<String>,
    output_dir: &Option<String>,
    database_url: Option<String>,
) -> Result<OutputConfig> {
    let format_lower = format.to_lowercase();

    if format_lower == "table" {
        // When format is table (default) and interval is set, use database
        if let Some(db_url) = database_url {
            let pool = PgPool::connect(&db_url).await?;
            let repository = PostgresRepository::new(pool);
            Ok(OutputConfig::Database { repository })
        } else {
            anyhow::bail!("Periodic fetching (--interval) without explicit format requires --database-url or DATABASE_URL environment variable");
        }
    } else if format_lower == "csv" || format_lower == "excel" {
        let output_file = output.ok_or_else(|| {
            anyhow::anyhow!(
                "Periodic fetching (--interval) with --format csv/excel requires --output <file>"
            )
        })?;
        if let Some(ref dir) = output_dir {
            std::fs::create_dir_all(dir)?;
        }
        Ok(OutputConfig::File {
            output_file,
            output_dir: output_dir.clone(),
            format: format_lower,
        })
    } else {
        anyhow::bail!("Periodic fetching (--interval) requires --format csv, --format excel, or database storage (no format specified)");
    }
}

/// Format output description for logging
pub fn format_output_description(config: &OutputConfig) -> String {
    match config {
        OutputConfig::File {
            output_file,
            output_dir,
            format,
        } => {
            format!(
                "format: {}, output: {}{}",
                format,
                output_dir
                    .as_ref()
                    .map(|d| format!("{}/", d))
                    .unwrap_or_default(),
                output_file
            )
        }
        OutputConfig::Database { .. } => "database".to_string(),
    }
}

/// Generic periodic fetcher that can be reused across different commands
///
/// This function abstracts the common pattern of periodically fetching data
/// for multiple symbols and processing it. It handles:
/// - Interval-based fetching
/// - Snapshot counting with optional limit
/// - Per-symbol data fetching
/// - Error handling and logging
///
/// # Type Parameters
/// * `T` - The data type being fetched (e.g., Ticker, LiquidityDepthStats)
/// * `F` - Function that fetches data for a single symbol
/// * `Fut` - Future returned by the fetch function
/// * `C` - Callback function invoked for each fetched data point
///
/// # Arguments
/// * `interval_seconds` - How often to fetch data
/// * `max_snapshots` - Maximum number of snapshots to collect (0 = unlimited)
/// * `symbols` - List of symbols to fetch
/// * `fetch_fn` - Async function that fetches data for a symbol
/// * `on_data` - Callback invoked for each fetched data point
pub async fn run_periodic_fetcher<T, F, Fut, C>(
    interval_seconds: u64,
    max_snapshots: usize,
    symbols: Vec<String>,
    fetch_fn: F,
    mut on_data: C,
) -> Result<()>
where
    T: Clone + Send + Sync,
    F: Fn(String) -> Fut,
    Fut: Future<Output = Result<T>>,
    C: FnMut(String, T) -> Result<()>,
{
    let mut interval_timer = interval(Duration::from_secs(interval_seconds));
    let mut snapshot_count = 0;
    let unlimited = max_snapshots == 0;

    loop {
        interval_timer.tick().await;

        snapshot_count += 1;
        tracing::info!(
            "Fetching data (snapshot {}/{})",
            snapshot_count,
            if unlimited {
                "unlimited".to_string()
            } else {
                max_snapshots.to_string()
            }
        );

        // Fetch data for all symbols
        for symbol in &symbols {
            match fetch_fn(symbol.clone()).await {
                Ok(data) => {
                    if let Err(e) = on_data(symbol.clone(), data) {
                        tracing::error!("Failed to process data for {}: {}", symbol, e);
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to fetch data for {}: {}", symbol, e);
                }
            }
        }

        // Check if we've reached the max snapshots
        if !unlimited && snapshot_count >= max_snapshots {
            tracing::info!("Reached maximum snapshots ({}), stopping", max_snapshots);
            break;
        }
    }

    Ok(())
}

/// Fetch orderbook and calculate liquidity depth
///
/// This function consolidates the common pattern of:
/// 1. Fetching orderbook from REST API
/// 2. Calculating liquidity depth using aggregator
///
/// Used by both `liquidity` command and `start` command's report task.
///
/// # Arguments
/// * `client` - Exchange client implementing IPerps
/// * `aggregator` - Aggregator implementing IAggregator
/// * `symbol` - Exchange-specific symbol format (e.g., "BTCUSDT")
/// * `global_symbol` - Global symbol format (e.g., "BTC")
/// * `exchange` - Exchange name (e.g., "binance")
/// * `depth` - Orderbook depth to fetch (e.g., 500, 1000)
pub async fn fetch_and_calculate_liquidity(
    client: &dyn IPerps,
    aggregator: &dyn IAggregator,
    symbol: &str,
    global_symbol: &str,
    exchange: &str,
    depth: u32,
) -> Result<LiquidityDepthStats> {
    // Fetch orderbook from REST API
    let orderbook = client.get_orderbook(symbol, depth).await?;

    // Calculate liquidity depth using aggregator
    let stats = aggregator
        .calculate_liquidity_depth(&orderbook, exchange, global_symbol)
        .await?;

    Ok(stats)
}

/// Generic CSV writer for data organized by symbol
///
/// Creates separate CSV files for each symbol with columns determined by the header function.
/// Data is written in descending timestamp order (latest first).
///
/// # Type Parameters
/// * `T` - The data type being written
/// * `H` - Function that generates CSV headers
/// * `R` - Function that converts data to CSV row
///
/// # Arguments
/// * `data` - HashMap of symbol -> Vec<data>
/// * `output_dir` - Directory to write CSV files
/// * `header_fn` - Function that returns CSV header row
/// * `row_fn` - Function that converts data item to CSV row
pub fn write_to_csv_generic<T, H, R>(
    data: &HashMap<String, Vec<T>>,
    output_dir: &Path,
    header_fn: H,
    row_fn: R,
) -> Result<()>
where
    H: Fn() -> Vec<String>,
    R: Fn(&T) -> Vec<String>,
{
    use std::fs;

    // Create output directory if it doesn't exist
    fs::create_dir_all(output_dir)?;

    for (symbol, items) in data {
        if items.is_empty() {
            tracing::warn!("No data to write for symbol: {}", symbol);
            continue;
        }

        let csv_path = output_dir.join(format!("{}.csv", symbol));
        let mut wtr = csv::Writer::from_path(&csv_path)?;

        // Write header
        wtr.write_record(&header_fn())?;

        // Write rows (data already in descending order)
        for item in items {
            wtr.write_record(&row_fn(item))?;
        }

        wtr.flush()?;
        tracing::info!("Wrote {} rows to {}", items.len(), csv_path.display());
    }

    Ok(())
}

/// Generic Excel writer for data organized by symbol
///
/// Creates a single Excel workbook with separate sheets for each symbol.
/// Data is written in descending timestamp order (latest first).
/// Sheets are sorted alphabetically by symbol name.
///
/// # Type Parameters
/// * `T` - The data type being written
/// * `H` - Function that generates Excel headers
/// * `R` - Function that converts data to Excel row
///
/// # Arguments
/// * `data` - HashMap of symbol -> Vec<data>
/// * `output_path` - Path to Excel workbook
/// * `header_fn` - Function that returns Excel header row
/// * `row_fn` - Function that converts data item to Excel row
pub fn write_to_excel_generic<T, H, R>(
    data: &HashMap<String, Vec<T>>,
    output_path: &Path,
    header_fn: H,
    row_fn: R,
) -> Result<()>
where
    H: Fn() -> Vec<String>,
    R: Fn(&T) -> Vec<String>,
{
    use rust_xlsxwriter::*;

    let mut workbook = Workbook::new();

    // Sort symbols alphabetically for consistent sheet ordering
    let mut sorted_symbols: Vec<_> = data.keys().collect();
    sorted_symbols.sort();

    for symbol in sorted_symbols {
        let items = &data[symbol];

        if items.is_empty() {
            tracing::warn!("No data to write for symbol: {}", symbol);
            continue;
        }

        let sheet = workbook.add_worksheet();
        sheet.set_name(symbol)?;

        // Write header
        let headers = header_fn();
        for (col_idx, header) in headers.iter().enumerate() {
            sheet.write_string(0, col_idx as u16, header)?;
        }

        // Write rows (data already in descending order)
        for (row_idx, item) in items.iter().enumerate() {
            let row = row_fn(item);
            for (col_idx, value) in row.iter().enumerate() {
                sheet.write_string((row_idx + 1) as u32, col_idx as u16, value)?;
            }
        }

        tracing::info!("Wrote {} rows to sheet '{}'", items.len(), symbol);
    }

    workbook.save(output_path)?;
    tracing::info!("Excel workbook saved to {}", output_path.display());

    Ok(())
}
