use anyhow::Result;
use chrono::Utc;
use csv::Writer;
use perps_core::{IPerps, Ticker};
use perps_exchanges::get_exchange;
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
    // Create exchange client using factory
    let client = get_exchange(&exchange)?;

    // Parse symbols
    let requested_symbols: Vec<String> = symbols
        .split(',')
        .map(|s| client.parse_symbol(s.trim()))
        .collect();

    // Validate symbols - filter out unsupported ones
    let symbol_list = validate_symbols(client.as_ref(), &requested_symbols).await?;

    if symbol_list.is_empty() {
        anyhow::bail!("No valid symbols found for exchange {}", exchange);
    }

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
            client.as_ref(),
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
            "Retrieving ticker data for {} from exchange {}",
            symbols,
            &exchange,
        );

        for symbol in &symbol_list {
            match fetch_and_display_ticker_data(client.as_ref(), symbol, &format, &exchange)
                .await
            {
                Ok(_) => {}
                Err(e) => {
                    tracing::error!("Failed to fetch ticker for {}: {}", symbol, e);
                    eprintln!("Error fetching ticker for {}: {}", symbol, e);
                }
            }
        }
    }

    Ok(())
}

/// Run periodic ticker fetcher and write to file
async fn run_periodic_fetcher(
    client: &dyn IPerps,
    symbols: &[String],
    exchange: &str,
    interval_secs: u64,
    max_snapshots: usize,
    config: OutputConfig,
) -> Result<()> {
    tracing::info!(
        "Starting periodic ticker fetcher (interval: {}s, max_snapshots: {}, format: {}, output: {}{})",
        interval_secs,
        if max_snapshots == 0 { "unlimited".to_string() } else { max_snapshots.to_string() },
        config.format,
        config.output_dir.as_ref().map(|d| format!("{}/", d)).unwrap_or_default(),
        config.output_file
    );

    // Storage for all snapshots: symbol -> Vec<Ticker>
    let mut data_by_symbol: HashMap<String, Vec<Ticker>> = HashMap::new();
    for symbol in symbols {
        data_by_symbol.insert(symbol.clone(), Vec::new());
    }

    let mut snapshot_count = 0;
    let unlimited = max_snapshots == 0;

    loop {
        let now = Utc::now();
        tracing::info!("[Snapshot #{}] Fetching tickers at {}", snapshot_count + 1, now.format("%Y-%m-%d %H:%M:%S UTC"));

        // Fetch data for all symbols
        for symbol in symbols {
            match fetch_ticker_data(client, symbol).await {
                Ok(ticker) => {
                    tracing::debug!("✓ {} - Last: ${:.2}, Volume: {:.2}, BestBidQty: {:.2}, BestAskQty: {:.2}",
                        ticker.symbol,
                        ticker.last_price,
                        ticker.volume_24h,
                        ticker.best_bid_qty,
                        ticker.best_ask_qty
                    );
                    data_by_symbol.get_mut(symbol).unwrap().push(ticker);
                }
                Err(e) => {
                    tracing::error!("Failed to fetch ticker for {}: {}", symbol, e);
                }
            }
        }

        snapshot_count += 1;

        // Write to file after each snapshot
        match config.format.as_str() {
            "csv" => write_to_csv(&data_by_symbol, &config.output_file, &config.output_dir, exchange)?,
            "excel" => write_to_excel(&data_by_symbol, &config.output_file, &config.output_dir, exchange)?,
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

/// Fetch ticker data for a single symbol (without displaying)
async fn fetch_ticker_data(
    client: &dyn IPerps,
    symbol: &str,
) -> Result<Ticker> {
    let ticker = client.get_ticker(symbol).await?;
    Ok(ticker)
}

/// Write ticker data to CSV file with separate files per symbol
/// Format: symbol_exchange.csv (e.g., BTC_binance.csv, ETH_lighter.csv)
fn write_to_csv(
    data_by_symbol: &HashMap<String, Vec<Ticker>>,
    _base_filename: &str,
    output_dir: &Option<String>,
    exchange: &str,
) -> Result<()> {
    for (symbol, snapshots) in data_by_symbol {
        if snapshots.is_empty() {
            continue;
        }

        // Create filename: symbol_exchange.csv
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

        // Write header row
        wtr.write_record([
            "Timestamp",
            "Exchange",
            "Symbol",
            "Last Price",
            "Mark Price",
            "Index Price",
            "Best Bid Price",
            "Best Bid Qty",
            "Best Bid Notional",
            "Best Ask Price",
            "Best Ask Qty",
            "Best Ask Notional",
            "Volume 24h",
            "Turnover 24h",
            "Price Change 24h",
            "Price Change %",
            "High 24h",
            "Low 24h",
        ])?;

        // Write data rows in reverse order (latest first)
        for ticker in snapshots.iter().rev() {
            // Calculate notional values
            let bid_notional = ticker.best_bid_price * ticker.best_bid_qty;
            let ask_notional = ticker.best_ask_price * ticker.best_ask_qty;

            wtr.write_record(&[
                ticker.timestamp.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
                exchange.to_string(),
                ticker.symbol.clone(),
                ticker.last_price.to_string(),
                ticker.mark_price.to_string(),
                ticker.index_price.to_string(),
                ticker.best_bid_price.to_string(),
                ticker.best_bid_qty.to_string(),
                bid_notional.to_string(),
                ticker.best_ask_price.to_string(),
                ticker.best_ask_qty.to_string(),
                ask_notional.to_string(),
                ticker.volume_24h.to_string(),
                ticker.turnover_24h.to_string(),
                ticker.price_change_24h.to_string(),
                ticker.price_change_pct.to_string(),
                ticker.high_price_24h.to_string(),
                ticker.low_price_24h.to_string(),
            ])?;
        }

        wtr.flush()?;
    }

    Ok(())
}

/// Write ticker data to Excel file with separate sheets per symbol
/// Format: Single workbook with multiple sheets
fn write_to_excel(
    data_by_symbol: &HashMap<String, Vec<Ticker>>,
    base_filename: &str,
    output_dir: &Option<String>,
    exchange: &str,
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

        // Create sheet name: symbol_exchange
        let sheet_name = format!("{}_{}", symbol.replace("-", "_"), exchange);
        let worksheet = workbook.add_worksheet();
        worksheet.set_name(&sheet_name)?;

        // Write header row
        let headers = [
            "Timestamp",
            "Exchange",
            "Symbol",
            "Last Price",
            "Mark Price",
            "Index Price",
            "Best Bid Price",
            "Best Bid Qty",
            "Best Bid Notional",
            "Best Ask Price",
            "Best Ask Qty",
            "Best Ask Notional",
            "Volume 24h",
            "Turnover 24h",
            "Price Change 24h",
            "Price Change %",
            "High 24h",
            "Low 24h",
        ];

        for (col, header) in headers.iter().enumerate() {
            worksheet.write_string_with_format(0, col as u16, *header, &header_format)?;
        }

        // Write data rows in reverse order (latest first)
        for (row_idx, ticker) in snapshots.iter().rev().enumerate() {
            let row = (row_idx + 1) as u32; // +1 to skip header row

            // Calculate notional values
            let bid_notional = ticker.best_bid_price * ticker.best_bid_qty;
            let ask_notional = ticker.best_ask_price * ticker.best_ask_qty;

            worksheet.write_string_with_format(
                row,
                0,
                ticker.timestamp.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
                &timestamp_format,
            )?;
            worksheet.write_string(row, 1, exchange)?;
            worksheet.write_string(row, 2, &ticker.symbol)?;
            worksheet.write_number(row, 3, ticker.last_price.to_string().parse::<f64>().unwrap_or(0.0))?;
            worksheet.write_number(row, 4, ticker.mark_price.to_string().parse::<f64>().unwrap_or(0.0))?;
            worksheet.write_number(row, 5, ticker.index_price.to_string().parse::<f64>().unwrap_or(0.0))?;
            worksheet.write_number(row, 6, ticker.best_bid_price.to_string().parse::<f64>().unwrap_or(0.0))?;
            worksheet.write_number(row, 7, ticker.best_bid_qty.to_string().parse::<f64>().unwrap_or(0.0))?;
            worksheet.write_number(row, 8, bid_notional.to_string().parse::<f64>().unwrap_or(0.0))?;
            worksheet.write_number(row, 9, ticker.best_ask_price.to_string().parse::<f64>().unwrap_or(0.0))?;
            worksheet.write_number(row, 10, ticker.best_ask_qty.to_string().parse::<f64>().unwrap_or(0.0))?;
            worksheet.write_number(row, 11, ask_notional.to_string().parse::<f64>().unwrap_or(0.0))?;
            worksheet.write_number(row, 12, ticker.volume_24h.to_string().parse::<f64>().unwrap_or(0.0))?;
            worksheet.write_number(row, 13, ticker.turnover_24h.to_string().parse::<f64>().unwrap_or(0.0))?;
            worksheet.write_number(row, 14, ticker.price_change_24h.to_string().parse::<f64>().unwrap_or(0.0))?;
            worksheet.write_number(row, 15, ticker.price_change_pct.to_string().parse::<f64>().unwrap_or(0.0))?;
            worksheet.write_number(row, 16, ticker.high_price_24h.to_string().parse::<f64>().unwrap_or(0.0))?;
            worksheet.write_number(row, 17, ticker.low_price_24h.to_string().parse::<f64>().unwrap_or(0.0))?;
        }

        // Auto-fit columns for better readability
        worksheet.set_column_width(0, 25)?; // Timestamp
        worksheet.set_column_width(1, 12)?; // Exchange
        worksheet.set_column_width(2, 15)?; // Symbol
        for col in 3..18 {
            worksheet.set_column_width(col, 15)?; // Numeric columns
        }
    }

    // Save the workbook
    workbook.save(&full_path)?;

    Ok(())
}

async fn fetch_and_display_ticker_data(
    client: &dyn IPerps,
    symbol: &str,
    format: &str,
    exchange: &str,
) -> Result<()> {
    let ticker = client.get_ticker(symbol).await?;

    match format.to_lowercase().as_str() {
        "json" => {
            display_json(&ticker, exchange)?;
        }
        "table" => {
            display_table(&ticker, exchange)?;
        }
        _ => {
            display_table(&ticker, exchange)?;
        }
    }

    Ok(())
}

fn display_table(ticker: &Ticker, exchange: &str) -> Result<()> {
    let mut table = Table::new();
    table.set_format(*format::consts::FORMAT_NO_LINESEP_WITH_TITLE);

    table.set_titles(Row::new(vec![Cell::new(&format!(
        "Ticker: {}/{} (24H)",
        exchange.to_uppercase(),
        ticker.symbol
    ))
    .with_hspan(2)]));

    // Price Information
    table.add_row(Row::new(vec![
        Cell::new("Price").with_style(prettytable::Attr::Bold),
        Cell::new(""),
    ]));
    table.add_row(Row::new(vec![
        Cell::new("  Last"),
        Cell::new_align(&format!("${:.2}", ticker.last_price), format::Alignment::RIGHT),
    ]));
    table.add_row(Row::new(vec![
        Cell::new("  Mark"),
        Cell::new_align(&format!("${:.2}", ticker.mark_price), format::Alignment::RIGHT),
    ]));
    table.add_row(Row::new(vec![
        Cell::new("  Index"),
        Cell::new_align(&format!("${:.2}", ticker.index_price), format::Alignment::RIGHT),
    ]));

    // Best Bid/Ask
    table.add_row(Row::new(vec![
        Cell::new("Best Bid/Ask").with_style(prettytable::Attr::Bold),
        Cell::new(""),
    ]));

    let bid_notional = ticker.best_bid_price * ticker.best_bid_qty;
    let ask_notional = ticker.best_ask_price * ticker.best_ask_qty;

    table.add_row(Row::new(vec![
        Cell::new("  Bid"),
        Cell::new_align(&format!("${:.2} @ {} (${:.2})", ticker.best_bid_price, ticker.best_bid_qty, bid_notional), format::Alignment::RIGHT),
    ]));
    table.add_row(Row::new(vec![
        Cell::new("  Ask"),
        Cell::new_align(&format!("${:.2} @ {} (${:.2})", ticker.best_ask_price, ticker.best_ask_qty, ask_notional), format::Alignment::RIGHT),
    ]));

    // 24H Statistics
    table.add_row(Row::new(vec![
        Cell::new("24H Statistics").with_style(prettytable::Attr::Bold),
        Cell::new(""),
    ]));
    table.add_row(Row::new(vec![
        Cell::new("  Volume"),
        Cell::new_align(&ticker.volume_24h.to_string(), format::Alignment::RIGHT),
    ]));
    table.add_row(Row::new(vec![
        Cell::new("  Turnover"),
        Cell::new_align(&format!("${:.2}", ticker.turnover_24h), format::Alignment::RIGHT),
    ]));
    table.add_row(Row::new(vec![
        Cell::new("  Change"),
        Cell::new_align(
            &format!("{:.2} ({:.2}%)", ticker.price_change_24h, ticker.price_change_pct * rust_decimal::Decimal::new(100, 0)),
            format::Alignment::RIGHT
        ),
    ]));
    table.add_row(Row::new(vec![
        Cell::new("  High"),
        Cell::new_align(&format!("${:.2}", ticker.high_price_24h), format::Alignment::RIGHT),
    ]));
    table.add_row(Row::new(vec![
        Cell::new("  Low"),
        Cell::new_align(&format!("${:.2}", ticker.low_price_24h), format::Alignment::RIGHT),
    ]));

    table.add_row(Row::new(vec![Cell::new("Timestamp").with_style(prettytable::Attr::Bold), Cell::new_align(&ticker.timestamp.format("%Y-%m-%d %H:%M:%S UTC").to_string(), format::Alignment::RIGHT)]));

    println!();
    table.printstd();
    println!();

    Ok(())
}

fn display_json(ticker: &Ticker, exchange: &str) -> Result<()> {
    let bid_notional = ticker.best_bid_price * ticker.best_bid_qty;
    let ask_notional = ticker.best_ask_price * ticker.best_ask_qty;

    let output = serde_json::json!({
        "exchange": exchange,
        "symbol": ticker.symbol,
        "timestamp": ticker.timestamp,
        "price": {
            "last": ticker.last_price,
            "mark": ticker.mark_price,
            "index": ticker.index_price,
        },
        "best_bid": {
            "price": ticker.best_bid_price,
            "quantity": ticker.best_bid_qty,
            "notional": bid_notional,
        },
        "best_ask": {
            "price": ticker.best_ask_price,
            "quantity": ticker.best_ask_qty,
            "notional": ask_notional,
        },
        "statistics_24h": {
            "volume": ticker.volume_24h,
            "turnover": ticker.turnover_24h,
            "price_change": ticker.price_change_24h,
            "price_change_pct": ticker.price_change_pct,
            "high": ticker.high_price_24h,
            "low": ticker.low_price_24h,
        }
    });
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Validate symbols against the exchange - filter out unsupported symbols
async fn validate_symbols(client: &dyn IPerps, symbols: &[String]) -> Result<Vec<String>> {
    let mut valid_symbols = Vec::new();
    let mut invalid_symbols = Vec::new();

    for symbol in symbols {
        match client.is_supported(symbol).await {
            Ok(true) => {
                tracing::debug!("✓ Symbol {} is supported on {}", symbol, client.get_name());
                valid_symbols.push(symbol.clone());
            }
            Ok(false) => {
                tracing::warn!("✗ Symbol {} is not supported on {} - skipping", symbol, client.get_name());
                invalid_symbols.push(symbol.clone());
            }
            Err(e) => {
                tracing::error!("Failed to check if symbol {} is supported: {} - skipping", symbol, e);
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
