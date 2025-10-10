use anyhow::Result;
use chrono::Utc;
use csv::Writer;
use perps_aggregator::{Aggregator, IAggregator};
use perps_core::{IPerps, LiquidityDepthStats};
use perps_exchanges::BinanceClient;
use prettytable::{format, Cell, Row, Table};
use rust_xlsxwriter::{Format, Workbook};
use serde_json;
use std::collections::HashMap;
use std::fs::File;
use tokio::time::{sleep, Duration};

/// Configuration for periodic fetcher output
struct OutputConfig {
    output_file: String,
    output_dir: Option<String>,
    format: String,
}

pub async fn execute(
    exchange: String,
    symbols: String,
    format: String,
    output: Option<String>,
    output_dir: Option<String>,
    interval: Option<u64>,
    max_snapshots: usize,
) -> Result<()> {
    let client = match exchange.to_lowercase().as_str() {
        "binance" => BinanceClient::new(),
        _ => {
            anyhow::bail!("Unsupported exchange: {}. Currently supported: binance", exchange);
        }
    };

    // Parse symbols
    let symbol_list: Vec<String> = symbols
        .split(',')
        .map(|s| client.parse_symbol(s.trim()))
        .collect();

    let aggregator = Aggregator::new();

    // Check if periodic fetching is enabled
    if let Some(interval_secs) = interval {
        // Validate format and output file
        let format_lower = format.to_lowercase();
        if format_lower != "csv" && format_lower != "excel" {
            anyhow::bail!("Periodic fetching (--interval) requires --format csv or --format excel");
        }
        if output.is_none() {
            anyhow::bail!("Periodic fetching (--interval) requires --output <file>");
        }

        // Create output directory if specified
        if let Some(ref dir) = output_dir {
            std::fs::create_dir_all(dir)?;
            tracing::info!("Created output directory: {}", dir);
        }

        // Run periodic fetcher
        let output_config = OutputConfig {
            output_file: output.unwrap(),
            output_dir,
            format: format_lower,
        };

        run_periodic_fetcher(
            &client,
            &aggregator,
            &symbol_list,
            &exchange,
            interval_secs,
            max_snapshots,
            output_config,
        )
        .await?;
    } else {
        // Single fetch mode (original behavior)
        tracing::info!(
            "Retrieving liquidity depth for {} from exchange {}",
            symbols,
            &exchange,
        );

        for symbol in &symbol_list {
            match fetch_and_display_liquidity_data(&client, &aggregator, symbol, &format, &exchange)
                .await
            {
                Ok(_) => {}
                Err(e) => {
                    tracing::error!("Failed to fetch liquidity for {}: {}", symbol, e);
                    eprintln!("Error fetching liquidity for {}: {}", symbol, e);
                }
            }
        }
    }

    Ok(())
}

/// Run periodic liquidity fetcher and write to file
async fn run_periodic_fetcher(
    client: &impl IPerps,
    aggregator: &impl IAggregator,
    symbols: &[String],
    exchange: &str,
    interval_secs: u64,
    max_snapshots: usize,
    config: OutputConfig,
) -> Result<()> {
    tracing::info!(
        "Starting periodic liquidity fetcher (interval: {}s, max_snapshots: {}, format: {}, output: {}{})",
        interval_secs,
        if max_snapshots == 0 { "unlimited".to_string() } else { max_snapshots.to_string() },
        config.format,
        config.output_dir.as_ref().map(|d| format!("{}/", d)).unwrap_or_default(),
        config.output_file
    );

    // Storage for all snapshots: symbol -> Vec<LiquidityDepthStats>
    let mut data_by_symbol: HashMap<String, Vec<LiquidityDepthStats>> = HashMap::new();
    for symbol in symbols {
        data_by_symbol.insert(symbol.clone(), Vec::new());
    }

    let mut snapshot_count = 0;
    let unlimited = max_snapshots == 0;

    loop {
        let now = Utc::now();
        tracing::info!("[Snapshot #{}] Fetching liquidity at {}", snapshot_count + 1, now.format("%Y-%m-%d %H:%M:%S UTC"));

        // Fetch data for all symbols
        for symbol in symbols {
            match fetch_liquidity_data(client, aggregator, symbol, exchange).await {
                Ok(stats) => {
                    tracing::debug!("✓ {} - Mid: ${:.2}, Bid(10bps): ${:.2}, Ask(10bps): ${:.2}",
                        stats.symbol,
                        stats.mid_price,
                        stats.bid_10bps,
                        stats.ask_10bps
                    );
                    data_by_symbol.get_mut(symbol).unwrap().push(stats);
                }
                Err(e) => {
                    tracing::error!("Failed to fetch liquidity for {}: {}", symbol, e);
                }
            }
        }

        snapshot_count += 1;

        // Write to file after each snapshot
        match config.format.as_str() {
            "csv" => write_to_csv(&data_by_symbol, &config.output_file, &config.output_dir)?,
            "excel" => write_to_excel(&data_by_symbol, &config.output_file, &config.output_dir)?,
            _ => unreachable!("Format already validated"),
        }

        tracing::info!("✓ Written snapshot #{} to {}{}",
            snapshot_count,
            config.output_dir.as_ref().map(|d| format!("{}/", d)).unwrap_or_default(),
            config.output_file
        );

        // Check if we've reached the max snapshots
        if !unlimited && snapshot_count >= max_snapshots {
            tracing::info!("Reached maximum snapshots ({}). Stopping.", max_snapshots);
            break;
        }

        // Wait for next interval
        tracing::debug!("Waiting {} seconds until next snapshot...", interval_secs);
        sleep(Duration::from_secs(interval_secs)).await;
    }

    tracing::info!("✓ Periodic fetcher completed. Total snapshots: {}", snapshot_count);
    Ok(())
}

/// Fetch liquidity data for a single symbol (without displaying)
async fn fetch_liquidity_data(
    client: &impl IPerps,
    aggregator: &impl IAggregator,
    symbol: &str,
    exchange: &str,
) -> Result<LiquidityDepthStats> {
    let orderbook = client.get_orderbook(symbol, 500).await?;
    let stats = aggregator.calculate_liquidity_depth(&orderbook, exchange).await?;
    Ok(stats)
}

/// Write liquidity data to CSV file with separate files per symbol
/// Format: symbol_exchange.csv (e.g., BTC_binance.csv, ETH_lighter.csv)
/// Column order: All bids first, then all asks
fn write_to_csv(
    data_by_symbol: &HashMap<String, Vec<LiquidityDepthStats>>,
    _base_filename: &str,
    output_dir: &Option<String>,
) -> Result<()> {
    for (symbol, snapshots) in data_by_symbol {
        if snapshots.is_empty() {
            continue;
        }

        // Get exchange from first snapshot (all snapshots should have same exchange)
        let exchange = &snapshots[0].exchange;

        // Create filename: symbol_exchange.csv (e.g., BTC-USDT_binance.csv)
        let csv_filename = format!("{}_{}.csv", symbol.replace("-", "_"), exchange);

        // Prepend output directory if specified
        let full_path = if let Some(dir) = output_dir {
            format!("{}/{}", dir, csv_filename)
        } else {
            csv_filename
        };

        // Create CSV writer
        let file = File::create(&full_path)?;
        let mut wtr = Writer::from_writer(file);

        // Write header row with all bids first, then all asks
        wtr.write_record([
            "Timestamp",
            "Exchange",
            "Symbol",
            "Mid Price",
            "Bid 1bps",
            "Bid 2.5bps",
            "Bid 5bps",
            "Bid 10bps",
            "Bid 20bps",
            "Ask 1bps",
            "Ask 2.5bps",
            "Ask 5bps",
            "Ask 10bps",
            "Ask 20bps",
        ])?;

        // Write data rows in reverse order (latest first)
        for stats in snapshots.iter().rev() {
            wtr.write_record(&[
                stats.timestamp.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
                stats.exchange.clone(),
                stats.symbol.clone(),
                stats.mid_price.to_string(),
                // All bids first
                stats.bid_1bps.to_string(),
                stats.bid_2_5bps.to_string(),
                stats.bid_5bps.to_string(),
                stats.bid_10bps.to_string(),
                stats.bid_20bps.to_string(),
                // Then all asks
                stats.ask_1bps.to_string(),
                stats.ask_2_5bps.to_string(),
                stats.ask_5bps.to_string(),
                stats.ask_10bps.to_string(),
                stats.ask_20bps.to_string(),
            ])?;
        }

        wtr.flush()?;
    }

    Ok(())
}

/// Write liquidity data to Excel file with separate sheets per symbol
/// Format: Single workbook with multiple sheets
/// Column order: All bids first, then all asks (matching CSV format)
fn write_to_excel(
    data_by_symbol: &HashMap<String, Vec<LiquidityDepthStats>>,
    base_filename: &str,
    output_dir: &Option<String>,
) -> Result<()> {
    // Determine the full path for the Excel file
    let full_path = if let Some(dir) = output_dir {
        format!("{}/{}", dir, base_filename)
    } else {
        base_filename.to_string()
    };

    // Create a new workbook
    let mut workbook = Workbook::new();

    // Create formats
    let header_format = Format::new().set_bold();
    let timestamp_format = Format::new();

    // Create a sheet for each symbol
    for (symbol, snapshots) in data_by_symbol {
        if snapshots.is_empty() {
            continue;
        }

        // Get exchange from first snapshot
        let exchange = &snapshots[0].exchange;

        // Create sheet name: symbol_exchange (e.g., BTC_USDT_binance)
        let sheet_name = format!("{}_{}", symbol.replace("-", "_"), exchange);
        let worksheet = workbook.add_worksheet();
        worksheet.set_name(&sheet_name)?;

        // Write header row
        let headers = [
            "Timestamp",
            "Exchange",
            "Symbol",
            "Mid Price",
            "Bid 1bps",
            "Bid 2.5bps",
            "Bid 5bps",
            "Bid 10bps",
            "Bid 20bps",
            "Ask 1bps",
            "Ask 2.5bps",
            "Ask 5bps",
            "Ask 10bps",
            "Ask 20bps",
        ];

        for (col, header) in headers.iter().enumerate() {
            worksheet.write_string_with_format(0, col as u16, *header, &header_format)?;
        }

        // Write data rows in reverse order (latest first, matching CSV behavior)
        for (row_idx, stats) in snapshots.iter().rev().enumerate() {
            let row = (row_idx + 1) as u32; // +1 to skip header row

            worksheet.write_string_with_format(
                row,
                0,
                stats.timestamp.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
                &timestamp_format,
            )?;
            worksheet.write_string(row, 1, &stats.exchange)?;
            worksheet.write_string(row, 2, &stats.symbol)?;
            worksheet.write_number(row, 3, stats.mid_price.to_string().parse::<f64>().unwrap_or(0.0))?;

            // All bids first
            worksheet.write_number(row, 4, stats.bid_1bps.to_string().parse::<f64>().unwrap_or(0.0))?;
            worksheet.write_number(row, 5, stats.bid_2_5bps.to_string().parse::<f64>().unwrap_or(0.0))?;
            worksheet.write_number(row, 6, stats.bid_5bps.to_string().parse::<f64>().unwrap_or(0.0))?;
            worksheet.write_number(row, 7, stats.bid_10bps.to_string().parse::<f64>().unwrap_or(0.0))?;
            worksheet.write_number(row, 8, stats.bid_20bps.to_string().parse::<f64>().unwrap_or(0.0))?;

            // Then all asks
            worksheet.write_number(row, 9, stats.ask_1bps.to_string().parse::<f64>().unwrap_or(0.0))?;
            worksheet.write_number(row, 10, stats.ask_2_5bps.to_string().parse::<f64>().unwrap_or(0.0))?;
            worksheet.write_number(row, 11, stats.ask_5bps.to_string().parse::<f64>().unwrap_or(0.0))?;
            worksheet.write_number(row, 12, stats.ask_10bps.to_string().parse::<f64>().unwrap_or(0.0))?;
            worksheet.write_number(row, 13, stats.ask_20bps.to_string().parse::<f64>().unwrap_or(0.0))?;
        }

        // Auto-fit columns for better readability
        worksheet.set_column_width(0, 25)?; // Timestamp
        worksheet.set_column_width(1, 12)?; // Exchange
        worksheet.set_column_width(2, 15)?; // Symbol
        for col in 3..14 {
            worksheet.set_column_width(col, 15)?; // Prices and liquidity values
        }
    }

    // Save the workbook
    workbook.save(&full_path)?;

    Ok(())
}

async fn fetch_and_display_liquidity_data(
    client: &impl IPerps,
    aggregator: &impl IAggregator,
    symbol: &str,
    format: &str,
    exchange: &str,
) -> Result<()> {
    // Fetch a deep orderbook to ensure we have enough data for 20bps
    let orderbook = client.get_orderbook(symbol, 1000).await?;

    // Calculate liquidity depth stats
    let stats = aggregator.calculate_liquidity_depth(&orderbook, exchange).await?;

    match format.to_lowercase().as_str() {
        "json" => {
            display_json(&stats)?;
        }
        "table" => {
            display_table(&stats)?;
        }
        _ => {
            display_table(&stats)?;
        }
    }

    Ok(())
}

fn display_table(stats: &LiquidityDepthStats) -> Result<()> {
    let mut table = Table::new();
    table.set_format(*format::consts::FORMAT_NO_LINESEP_WITH_TITLE);

    table.set_titles(Row::new(vec![Cell::new(&format!(
        "Liquidity Depth: {}/{} (Mid Price: {})",
        stats.exchange.to_uppercase(),
        stats.symbol,
        stats.mid_price.round_dp(4)
    ))
    .with_hspan(3)]));

    table.add_row(Row::new(vec![
        Cell::new("Spread (bps)").with_style(prettytable::Attr::Bold),
        Cell::new("Cumulative Bid Notional").with_style(prettytable::Attr::Bold),
        Cell::new("Cumulative Ask Notional").with_style(prettytable::Attr::Bold),
    ]));

    let bps_labels = ["1 bps", "2.5 bps", "5 bps", "10 bps", "20 bps"];
    let bids = [
        stats.bid_1bps,
        stats.bid_2_5bps,
        stats.bid_5bps,
        stats.bid_10bps,
        stats.bid_20bps,
    ];
    let asks = [
        stats.ask_1bps,
        stats.ask_2_5bps,
        stats.ask_5bps,
        stats.ask_10bps,
        stats.ask_20bps,
    ];

    for i in 0..bps_labels.len() {
        table.add_row(Row::new(vec![
            Cell::new(bps_labels[i]),
            Cell::new_align(&format!("${:.2}", bids[i]), format::Alignment::RIGHT),
            Cell::new_align(&format!("${:.2}", asks[i]), format::Alignment::RIGHT),
        ]));
    }

    table.add_row(Row::new(vec![Cell::new("Timestamp").with_style(prettytable::Attr::Bold), Cell::new_align(&stats.timestamp.format("%Y-%m-%d %H:%M:%S UTC").to_string(), format::Alignment::RIGHT).with_hspan(2)]));

    println!();
    table.printstd();
    println!();

    Ok(())
}

fn display_json(stats: &LiquidityDepthStats) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(stats)?);
    Ok(())
}
