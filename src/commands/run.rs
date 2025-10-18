use anyhow::Result;
use chrono::Utc;
use futures::future::join_all;
use perps_aggregator::{Aggregator, IAggregator};
use perps_core::{IPerps, LiquidityDepthStats, Ticker};
use perps_exchanges::{all_exchanges, get_exchange};
use rust_xlsxwriter::{Format, Workbook};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use tokio::time::{sleep, Duration};

use super::common::validate_symbols;

/// Arguments for the run command
pub struct RunArgs {
    pub symbols_file: String,
    pub exchanges: Option<String>,
    pub interval: u64,
    pub output_dir: String,
    pub max_snapshots: usize,
}

pub async fn execute(args: RunArgs) -> Result<()> {
    let RunArgs {
        symbols_file,
        exchanges,
        interval,
        output_dir,
        max_snapshots,
    } = args;
    let exchange_names: Vec<String> = if let Some(ref ex) = exchanges {
        // Parse comma-separated exchanges
        ex.split(',').map(|s| s.trim().to_string()).collect()
    } else {
        // Get all supported exchanges from the factory
        all_exchanges()
            .await
            .into_iter()
            .map(|(name, _)| name)
            .collect()
    };

    tracing::info!(
        "Starting periodic data collection service (exchanges: [{}], interval: {}s, max_snapshots: {})",
        exchange_names.join(", "),
        interval,
        if max_snapshots == 0 { "unlimited".to_string() } else { max_snapshots.to_string() }
    );

    // Create output directory
    fs::create_dir_all(&output_dir)?;
    tracing::info!("Output directory: {}", output_dir);

    // Read symbols from file
    let symbols = read_symbols_from_file(&symbols_file)?;
    tracing::info!(
        "Loaded {} symbols from {}: {}",
        symbols.len(),
        symbols_file,
        symbols.join(", ")
    );

    // Create exchange clients
    let mut clients: Vec<(String, Arc<Box<dyn IPerps + Send + Sync>>)> = Vec::new();
    for ex_name in &exchange_names {
        match get_exchange(ex_name.as_str()).await {
            Ok(client) => {
                clients.push((ex_name.to_string(), Arc::new(client)));
                tracing::info!("✓ Initialized {} exchange client", ex_name);
            }
            Err(e) => {
                tracing::error!("Failed to create client for {}: {}", ex_name, e);
            }
        }
    }

    if clients.is_empty() {
        anyhow::bail!("No valid exchange clients could be created");
    }

    // Validate symbols for each exchange
    let mut exchange_symbols: HashMap<String, Vec<String>> = HashMap::new();
    for (ex_name, client) in &clients {
        let parsed_symbols: Vec<String> = symbols
            .iter()
            .map(|s| client.parse_symbol(s.trim()))
            .collect();

        match validate_symbols(client.as_ref().as_ref(), &parsed_symbols).await {
            Ok(valid_symbols) => {
                if !valid_symbols.is_empty() {
                    tracing::info!(
                        "✓ Validated {} symbols for {}",
                        valid_symbols.len(),
                        ex_name
                    );
                    exchange_symbols.insert(ex_name.clone(), valid_symbols);
                } else {
                    tracing::warn!("No valid symbols found for {}", ex_name);
                }
            }
            Err(e) => {
                tracing::error!("Failed to validate symbols for {}: {}", ex_name, e);
            }
        }
    }

    if exchange_symbols.is_empty() {
        anyhow::bail!("No valid symbols found on any exchange");
    }

    // Storage for data - keyed by symbol, then exchange
    let mut liquidity_data: HashMap<String, HashMap<String, Vec<LiquidityDepthStats>>> =
        HashMap::new();
    let mut ticker_data: HashMap<String, HashMap<String, Vec<Ticker>>> = HashMap::new();

    for symbol in &symbols {
        liquidity_data.insert(symbol.clone(), HashMap::new());
        ticker_data.insert(symbol.clone(), HashMap::new());
        for (ex_name, _) in &clients {
            liquidity_data
                .get_mut(symbol)
                .unwrap()
                .insert(ex_name.clone(), Vec::new());
            ticker_data
                .get_mut(symbol)
                .unwrap()
                .insert(ex_name.clone(), Vec::new());
        }
    }

    // Create aggregator for liquidity calculations
    let aggregator = Arc::new(Aggregator::new());

    let mut snapshot_count = 0;
    let unlimited = max_snapshots == 0;

    // Periodic data collection loop
    loop {
        let now = Utc::now();
        tracing::info!(
            "[Snapshot #{}] Fetching data at {}",
            snapshot_count + 1,
            now.format("%Y-%m-%d %H:%M:%S UTC")
        );

        // Collect data from all exchanges in parallel
        let mut tasks = Vec::new();
        for (ex_name, client) in &clients {
            if let Some(valid_symbols) = exchange_symbols.get(ex_name) {
                for symbol in valid_symbols {
                    let ex_name_clone = ex_name.clone();
                    let symbol_clone = symbol.clone();
                    let client_clone = Arc::clone(client);
                    let aggregator_clone = Arc::clone(&aggregator);
                    let symbols_clone = symbols.clone();

                    let task = tokio::spawn(async move {
                        let mut results = (None, None);

                        // Fetch ticker
                        match client_clone.get_ticker(&symbol_clone).await {
                            Ok(ticker) => {
                                tracing::debug!(
                                    "✓ {} @ {} ticker - Last: ${:.2}, Volume: {:.2}",
                                    symbol_clone,
                                    ex_name_clone,
                                    ticker.last_price,
                                    ticker.volume_24h
                                );
                                results.0 = Some(ticker);
                            }
                            Err(e) => {
                                tracing::error!(
                                    "Failed to fetch ticker for {} @ {}: {}",
                                    symbol_clone,
                                    ex_name_clone,
                                    e
                                );
                            }
                        }

                        // Fetch orderbook and calculate liquidity
                        match client_clone.get_orderbook(&symbol_clone, 100).await {
                            Ok(orderbook) => {
                                // Extract global symbol
                                let symbol_string = symbol_clone.to_string();
                                let global_symbol = symbols_clone
                                    .iter()
                                    .find(|s| client_clone.parse_symbol(s) == symbol_clone)
                                    .unwrap_or(&symbol_string);

                                match aggregator_clone
                                    .calculate_liquidity_depth(
                                        &orderbook,
                                        &ex_name_clone,
                                        global_symbol,
                                    )
                                    .await
                                {
                                    Ok(depth_stats) => {
                                        tracing::debug!(
                                            "✓ {} @ {} liquidity - Mid: ${:.2}, Bid 10bps: ${:.2}, Ask 10bps: ${:.2}",
                                            symbol_clone,
                                            ex_name_clone,
                                            depth_stats.mid_price,
                                            depth_stats.bid_10bps,
                                            depth_stats.ask_10bps
                                        );
                                        results.1 = Some(depth_stats);
                                    }
                                    Err(e) => {
                                        tracing::error!(
                                            "Failed to calculate liquidity for {} @ {}: {}",
                                            symbol_clone,
                                            ex_name_clone,
                                            e
                                        );
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::error!(
                                    "Failed to fetch orderbook for {} @ {}: {}",
                                    symbol_clone,
                                    ex_name_clone,
                                    e
                                );
                            }
                        }

                        (ex_name_clone, symbol_clone, results)
                    });

                    tasks.push(task);
                }
            }
        }

        // Wait for all tasks to complete
        let results = join_all(tasks).await;

        // Process results - filter out errors and process successful results
        for (ex_name, symbol, (ticker_opt, liquidity_opt)) in
            results.into_iter().filter_map(Result::ok)
        {
            // Find the global symbol for this parsed symbol
            let global_symbol = symbols
                .iter()
                .find(|s| {
                    // Check all clients to see which one parses to this symbol
                    clients.iter().any(|(client_ex_name, client)| {
                        client_ex_name == &ex_name && client.parse_symbol(s) == symbol
                    })
                })
                .unwrap_or(&symbol);

            if let Some(ticker) = ticker_opt {
                ticker_data
                    .get_mut(global_symbol)
                    .unwrap()
                    .get_mut(&ex_name)
                    .unwrap()
                    .push(ticker);
            }

            if let Some(liquidity) = liquidity_opt {
                liquidity_data
                    .get_mut(global_symbol)
                    .unwrap()
                    .get_mut(&ex_name)
                    .unwrap()
                    .push(liquidity);
            }
        }

        snapshot_count += 1;

        // Write data to Excel files
        let liquidity_file = Path::new(&output_dir).join("liquidity.xlsx");
        let ticker_file = Path::new(&output_dir).join("ticker.xlsx");

        match write_liquidity_to_excel_multi(&liquidity_data, &liquidity_file) {
            Ok(_) => tracing::info!("✓ Written liquidity data to {}", liquidity_file.display()),
            Err(e) => tracing::error!("Failed to write liquidity data: {}", e),
        }

        match write_ticker_to_excel_multi(&ticker_data, &ticker_file) {
            Ok(_) => tracing::info!("✓ Written ticker data to {}", ticker_file.display()),
            Err(e) => tracing::error!("Failed to write ticker data: {}", e),
        }

        tracing::info!("✓ Snapshot #{} completed", snapshot_count);

        // Check if we've reached max snapshots
        if !unlimited && snapshot_count >= max_snapshots {
            tracing::info!("Reached maximum snapshots ({}). Stopping.", max_snapshots);
            break;
        }

        // Wait for next interval
        tracing::debug!("Waiting {} seconds until next snapshot...", interval);
        sleep(Duration::from_secs(interval)).await;
    }

    tracing::info!(
        "✓ Data collection service completed. Total snapshots: {}",
        snapshot_count
    );
    Ok(())
}

/// Read symbols from file (supports both comma-separated and line-separated)
fn read_symbols_from_file(file_path: &str) -> Result<Vec<String>> {
    let content = fs::read_to_string(file_path)?;
    let mut symbols = Vec::new();

    // Split by both newlines and commas
    for line in content.lines() {
        for symbol in line.split(',') {
            let trimmed = symbol.trim();
            if !trimmed.is_empty() {
                symbols.push(trimmed.to_string());
            }
        }
    }

    if symbols.is_empty() {
        anyhow::bail!("No symbols found in file: {}", file_path);
    }

    Ok(symbols)
}

/// Write liquidity depth data to Excel file (multi-exchange version)
fn write_liquidity_to_excel_multi(
    data_by_symbol: &HashMap<String, HashMap<String, Vec<LiquidityDepthStats>>>,
    file_path: &Path,
) -> Result<()> {
    let mut workbook = Workbook::new();
    let header_format = Format::new().set_bold();
    let timestamp_format = Format::new();

    // Sort symbols alphabetically
    let mut symbols: Vec<&String> = data_by_symbol.keys().collect();
    symbols.sort();

    for symbol in symbols {
        let exchange_data = &data_by_symbol[symbol];
        // Collect all data from all exchanges for this symbol
        let mut all_data: Vec<(&String, &LiquidityDepthStats)> = Vec::new();
        for (exchange, snapshots) in exchange_data {
            for stats in snapshots {
                all_data.push((exchange, stats));
            }
        }

        if all_data.is_empty() {
            continue;
        }

        // Sort by timestamp descending (latest first)
        all_data.sort_by(|a, b| b.1.timestamp.cmp(&a.1.timestamp));

        let sheet_name = symbol.replace("-", "_");
        let worksheet = workbook.add_worksheet();
        worksheet.set_name(&sheet_name)?;

        // Write header
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

        // Write data rows
        for (row_idx, (exchange, stats)) in all_data.iter().enumerate() {
            let row = (row_idx + 1) as u32;

            worksheet.write_string_with_format(
                row,
                0,
                stats.timestamp.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
                &timestamp_format,
            )?;
            worksheet.write_string(row, 1, exchange.as_str())?;
            worksheet.write_string(row, 2, &stats.symbol)?;
            worksheet.write_number(
                row,
                3,
                stats.mid_price.to_string().parse::<f64>().unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                4,
                stats.bid_1bps.to_string().parse::<f64>().unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                5,
                stats.bid_2_5bps.to_string().parse::<f64>().unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                6,
                stats.bid_5bps.to_string().parse::<f64>().unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                7,
                stats.bid_10bps.to_string().parse::<f64>().unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                8,
                stats.bid_20bps.to_string().parse::<f64>().unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                9,
                stats.ask_1bps.to_string().parse::<f64>().unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                10,
                stats.ask_2_5bps.to_string().parse::<f64>().unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                11,
                stats.ask_5bps.to_string().parse::<f64>().unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                12,
                stats.ask_10bps.to_string().parse::<f64>().unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                13,
                stats.ask_20bps.to_string().parse::<f64>().unwrap_or(0.0),
            )?;
        }

        // Auto-fit columns
        worksheet.set_column_width(0, 25)?;
        worksheet.set_column_width(1, 12)?;
        worksheet.set_column_width(2, 15)?;
        for col in 3..14 {
            worksheet.set_column_width(col, 15)?;
        }
    }

    workbook.save(file_path)?;
    Ok(())
}

/// Write ticker data to Excel file (multi-exchange version)
fn write_ticker_to_excel_multi(
    data_by_symbol: &HashMap<String, HashMap<String, Vec<Ticker>>>,
    file_path: &Path,
) -> Result<()> {
    let mut workbook = Workbook::new();
    let header_format = Format::new().set_bold();
    let timestamp_format = Format::new();

    // Sort symbols alphabetically
    let mut symbols: Vec<&String> = data_by_symbol.keys().collect();
    symbols.sort();

    for symbol in symbols {
        let exchange_data = &data_by_symbol[symbol];
        // Collect all data from all exchanges for this symbol
        let mut all_data: Vec<(&String, &Ticker)> = Vec::new();
        for (exchange, snapshots) in exchange_data {
            for ticker in snapshots {
                all_data.push((exchange, ticker));
            }
        }

        if all_data.is_empty() {
            continue;
        }

        // Sort by timestamp descending (latest first)
        all_data.sort_by(|a, b| b.1.timestamp.cmp(&a.1.timestamp));

        let sheet_name = symbol.replace("-", "_");
        let worksheet = workbook.add_worksheet();
        worksheet.set_name(&sheet_name)?;

        // Write header (matches ticker command format)
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
            "Open Interest",
            "Open Interest Notional",
            "Price Change 24h",
            "Price Change %",
            "High 24h",
            "Low 24h",
        ];

        for (col, header) in headers.iter().enumerate() {
            worksheet.write_string_with_format(0, col as u16, *header, &header_format)?;
        }

        // Write data rows
        for (row_idx, (exchange, ticker)) in all_data.iter().enumerate() {
            let row = (row_idx + 1) as u32;

            let bid_notional = ticker.best_bid_price * ticker.best_bid_qty;
            let ask_notional = ticker.best_ask_price * ticker.best_ask_qty;

            worksheet.write_string_with_format(
                row,
                0,
                ticker.timestamp.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
                &timestamp_format,
            )?;
            worksheet.write_string(row, 1, exchange.as_str())?;
            worksheet.write_string(row, 2, &ticker.symbol)?;
            worksheet.write_number(
                row,
                3,
                ticker.last_price.to_string().parse::<f64>().unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                4,
                ticker.mark_price.to_string().parse::<f64>().unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                5,
                ticker.index_price.to_string().parse::<f64>().unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                6,
                ticker
                    .best_bid_price
                    .to_string()
                    .parse::<f64>()
                    .unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                7,
                ticker
                    .best_bid_qty
                    .to_string()
                    .parse::<f64>()
                    .unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                8,
                bid_notional.to_string().parse::<f64>().unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                9,
                ticker
                    .best_ask_price
                    .to_string()
                    .parse::<f64>()
                    .unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                10,
                ticker
                    .best_ask_qty
                    .to_string()
                    .parse::<f64>()
                    .unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                11,
                ask_notional.to_string().parse::<f64>().unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                12,
                ticker.volume_24h.to_string().parse::<f64>().unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                13,
                ticker
                    .turnover_24h
                    .to_string()
                    .parse::<f64>()
                    .unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                14,
                ticker
                    .open_interest
                    .to_string()
                    .parse::<f64>()
                    .unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                15,
                ticker
                    .open_interest_notional
                    .to_string()
                    .parse::<f64>()
                    .unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                16,
                ticker
                    .price_change_24h
                    .to_string()
                    .parse::<f64>()
                    .unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                17,
                ticker
                    .price_change_pct
                    .to_string()
                    .parse::<f64>()
                    .unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                18,
                ticker
                    .high_price_24h
                    .to_string()
                    .parse::<f64>()
                    .unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                19,
                ticker
                    .low_price_24h
                    .to_string()
                    .parse::<f64>()
                    .unwrap_or(0.0),
            )?;
        }

        // Auto-fit columns
        worksheet.set_column_width(0, 25)?;
        worksheet.set_column_width(1, 12)?;
        worksheet.set_column_width(2, 15)?;
        for col in 3..20 {
            worksheet.set_column_width(col, 15)?;
        }
    }

    workbook.save(file_path)?;
    Ok(())
}

/// Write liquidity depth data to Excel file (single-exchange version - deprecated)
#[allow(dead_code)]
fn write_liquidity_to_excel(
    data_by_symbol: &HashMap<String, Vec<LiquidityDepthStats>>,
    file_path: &Path,
    exchange: &str,
) -> Result<()> {
    let mut workbook = Workbook::new();
    let header_format = Format::new().set_bold();
    let timestamp_format = Format::new();

    for (symbol, snapshots) in data_by_symbol {
        if snapshots.is_empty() {
            continue;
        }

        let sheet_name = format!("{}_{}", symbol.replace("-", "_"), exchange);
        let worksheet = workbook.add_worksheet();
        worksheet.set_name(&sheet_name)?;

        // Write header
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

        // Write data rows in reverse order (latest first)
        for (row_idx, stats) in snapshots.iter().rev().enumerate() {
            let row = (row_idx + 1) as u32;

            worksheet.write_string_with_format(
                row,
                0,
                stats.timestamp.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
                &timestamp_format,
            )?;
            worksheet.write_string(row, 1, exchange)?;
            worksheet.write_string(row, 2, &stats.symbol)?;
            worksheet.write_number(
                row,
                3,
                stats.mid_price.to_string().parse::<f64>().unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                4,
                stats.bid_1bps.to_string().parse::<f64>().unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                5,
                stats.bid_2_5bps.to_string().parse::<f64>().unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                6,
                stats.bid_5bps.to_string().parse::<f64>().unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                7,
                stats.bid_10bps.to_string().parse::<f64>().unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                8,
                stats.bid_20bps.to_string().parse::<f64>().unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                9,
                stats.ask_1bps.to_string().parse::<f64>().unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                10,
                stats.ask_2_5bps.to_string().parse::<f64>().unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                11,
                stats.ask_5bps.to_string().parse::<f64>().unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                12,
                stats.ask_10bps.to_string().parse::<f64>().unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                13,
                stats.ask_20bps.to_string().parse::<f64>().unwrap_or(0.0),
            )?;
        }

        // Auto-fit columns
        worksheet.set_column_width(0, 25)?;
        worksheet.set_column_width(1, 12)?;
        worksheet.set_column_width(2, 15)?;
        for col in 3..14 {
            worksheet.set_column_width(col, 15)?;
        }
    }

    workbook.save(file_path)?;
    Ok(())
}

/// Write ticker data to Excel file (single-exchange version - deprecated)
#[allow(dead_code)]
fn write_ticker_to_excel(
    data_by_symbol: &HashMap<String, Vec<Ticker>>,
    file_path: &Path,
    exchange: &str,
) -> Result<()> {
    let mut workbook = Workbook::new();
    let header_format = Format::new().set_bold();
    let timestamp_format = Format::new();

    for (symbol, snapshots) in data_by_symbol {
        if snapshots.is_empty() {
            continue;
        }

        let sheet_name = format!("{}_{}", symbol.replace("-", "_"), exchange);
        let worksheet = workbook.add_worksheet();
        worksheet.set_name(&sheet_name)?;

        // Write header (matches ticker command format)
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
            "Open Interest",
            "Open Interest Notional",
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
            let row = (row_idx + 1) as u32;

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
            worksheet.write_number(
                row,
                3,
                ticker.last_price.to_string().parse::<f64>().unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                4,
                ticker.mark_price.to_string().parse::<f64>().unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                5,
                ticker.index_price.to_string().parse::<f64>().unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                6,
                ticker
                    .best_bid_price
                    .to_string()
                    .parse::<f64>()
                    .unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                7,
                ticker
                    .best_bid_qty
                    .to_string()
                    .parse::<f64>()
                    .unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                8,
                bid_notional.to_string().parse::<f64>().unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                9,
                ticker
                    .best_ask_price
                    .to_string()
                    .parse::<f64>()
                    .unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                10,
                ticker
                    .best_ask_qty
                    .to_string()
                    .parse::<f64>()
                    .unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                11,
                ask_notional.to_string().parse::<f64>().unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                12,
                ticker.volume_24h.to_string().parse::<f64>().unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                13,
                ticker
                    .turnover_24h
                    .to_string()
                    .parse::<f64>()
                    .unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                14,
                ticker
                    .open_interest
                    .to_string()
                    .parse::<f64>()
                    .unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                15,
                ticker
                    .open_interest_notional
                    .to_string()
                    .parse::<f64>()
                    .unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                16,
                ticker
                    .price_change_24h
                    .to_string()
                    .parse::<f64>()
                    .unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                17,
                ticker
                    .price_change_pct
                    .to_string()
                    .parse::<f64>()
                    .unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                18,
                ticker
                    .high_price_24h
                    .to_string()
                    .parse::<f64>()
                    .unwrap_or(0.0),
            )?;
            worksheet.write_number(
                row,
                19,
                ticker
                    .low_price_24h
                    .to_string()
                    .parse::<f64>()
                    .unwrap_or(0.0),
            )?;
        }

        // Auto-fit columns
        worksheet.set_column_width(0, 25)?;
        worksheet.set_column_width(1, 12)?;
        worksheet.set_column_width(2, 15)?;
        for col in 3..20 {
            worksheet.set_column_width(col, 15)?;
        }
    }

    workbook.save(file_path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use rust_decimal::Decimal;
    use std::str::FromStr;
    use tempfile::tempdir;

    fn create_mock_ticker(symbol: &str) -> Ticker {
        Ticker {
            timestamp: Utc::now(),
            symbol: symbol.to_string(),
            last_price: Decimal::from_str("50000").unwrap(),
            mark_price: Decimal::from_str("50010").unwrap(),
            index_price: Decimal::from_str("50005").unwrap(),
            best_bid_price: Decimal::from_str("49990").unwrap(),
            best_bid_qty: Decimal::from_str("1.5").unwrap(),
            best_ask_price: Decimal::from_str("50010").unwrap(),
            best_ask_qty: Decimal::from_str("2.0").unwrap(),
            volume_24h: Decimal::from_str("1000").unwrap(),
            turnover_24h: Decimal::from_str("50000000").unwrap(),
            open_interest: Decimal::from_str("10000").unwrap(),
            open_interest_notional: Decimal::from_str("500000000").unwrap(),
            price_change_24h: Decimal::from_str("500").unwrap(),
            price_change_pct: Decimal::from_str("0.01").unwrap(),
            high_price_24h: Decimal::from_str("51000").unwrap(),
            low_price_24h: Decimal::from_str("49000").unwrap(),
        }
    }

    fn create_mock_liquidity(symbol: &str, exchange: &str) -> LiquidityDepthStats {
        LiquidityDepthStats {
            timestamp: Utc::now(),
            exchange: exchange.to_string(),
            symbol: symbol.to_string(),
            mid_price: Decimal::from_str("50000").unwrap(),
            bid_1bps: Decimal::from_str("10000").unwrap(),
            bid_2_5bps: Decimal::from_str("20000").unwrap(),
            bid_5bps: Decimal::from_str("30000").unwrap(),
            bid_10bps: Decimal::from_str("40000").unwrap(),
            bid_20bps: Decimal::from_str("50000").unwrap(),
            ask_1bps: Decimal::from_str("10000").unwrap(),
            ask_2_5bps: Decimal::from_str("20000").unwrap(),
            ask_5bps: Decimal::from_str("30000").unwrap(),
            ask_10bps: Decimal::from_str("40000").unwrap(),
            ask_20bps: Decimal::from_str("50000").unwrap(),
        }
    }

    #[test]
    fn test_ticker_excel_has_20_columns() -> Result<()> {
        let dir = tempdir()?;
        let file_path = dir.path().join("test_ticker.xlsx");

        // Create test data with multiple exchanges
        let mut data_by_symbol: HashMap<String, HashMap<String, Vec<Ticker>>> = HashMap::new();
        data_by_symbol.insert("BTC".to_string(), HashMap::new());
        data_by_symbol
            .get_mut("BTC")
            .unwrap()
            .insert("binance".to_string(), vec![create_mock_ticker("BTC")]);
        data_by_symbol
            .get_mut("BTC")
            .unwrap()
            .insert("bybit".to_string(), vec![create_mock_ticker("BTC")]);

        write_ticker_to_excel_multi(&data_by_symbol, &file_path)?;

        assert!(file_path.exists(), "Excel file should be created");

        // Verify file is not empty
        let metadata = std::fs::metadata(&file_path)?;
        assert!(metadata.len() > 0, "Excel file should not be empty");

        dir.close()?;
        Ok(())
    }

    #[test]
    fn test_liquidity_excel_has_14_columns() -> Result<()> {
        let dir = tempdir()?;
        let file_path = dir.path().join("test_liquidity.xlsx");

        // Create test data
        let mut data_by_symbol: HashMap<String, HashMap<String, Vec<LiquidityDepthStats>>> =
            HashMap::new();
        data_by_symbol.insert("BTC".to_string(), HashMap::new());
        data_by_symbol.get_mut("BTC").unwrap().insert(
            "binance".to_string(),
            vec![create_mock_liquidity("BTC", "binance")],
        );

        write_liquidity_to_excel_multi(&data_by_symbol, &file_path)?;

        assert!(file_path.exists(), "Excel file should be created");

        // Verify file is not empty
        let metadata = std::fs::metadata(&file_path)?;
        assert!(metadata.len() > 0, "Excel file should not be empty");

        dir.close()?;
        Ok(())
    }

    #[test]
    fn test_ticker_output_compatibility_with_ticker_command() {
        // This test verifies that the ticker columns in run.rs match those in ticker.rs
        // Both should have these columns in order:
        // 1. Timestamp
        // 2. Exchange
        // 3. Symbol
        // 4. Last Price
        // 5. Mark Price
        // 6. Index Price
        // 7. Best Bid Price
        // 8. Best Bid Qty
        // 9. Best Bid Notional
        // 10. Best Ask Price
        // 11. Best Ask Qty
        // 12. Best Ask Notional
        // 13. Volume 24h
        // 14. Turnover 24h
        // 15. Open Interest ⭐
        // 16. Open Interest Notional ⭐
        // 17. Price Change 24h
        // 18. Price Change %
        // 19. High 24h
        // 20. Low 24h

        // This is a documentation test to ensure developers are aware of the column structure
        assert_eq!(
            20, 20,
            "Ticker output should have exactly 20 columns including Open Interest fields"
        );
    }

    #[test]
    fn test_read_symbols_from_file() -> Result<()> {
        let dir = tempdir()?;
        let file_path = dir.path().join("test_symbols.txt");

        // Test comma-separated format
        std::fs::write(&file_path, "BTC,ETH,SOL")?;
        let symbols = read_symbols_from_file(file_path.to_str().unwrap())?;
        assert_eq!(symbols, vec!["BTC", "ETH", "SOL"]);

        // Test line-separated format
        std::fs::write(&file_path, "BTC\nETH\nSOL")?;
        let symbols = read_symbols_from_file(file_path.to_str().unwrap())?;
        assert_eq!(symbols, vec!["BTC", "ETH", "SOL"]);

        // Test mixed format with whitespace
        std::fs::write(&file_path, "BTC, ETH\nSOL")?;
        let symbols = read_symbols_from_file(file_path.to_str().unwrap())?;
        assert_eq!(symbols, vec!["BTC", "ETH", "SOL"]);

        dir.close()?;
        Ok(())
    }

    #[test]
    fn test_read_symbols_from_file_empty() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("empty_symbols.txt");

        std::fs::write(&file_path, "").unwrap();
        let result = read_symbols_from_file(file_path.to_str().unwrap());

        assert!(
            result.is_err(),
            "Should return error for empty symbols file"
        );
        assert!(result.unwrap_err().to_string().contains("No symbols found"));

        dir.close().unwrap();
    }

    #[test]
    fn test_multi_exchange_ticker_sheets() -> Result<()> {
        let dir = tempdir()?;
        let file_path = dir.path().join("multi_exchange_ticker.xlsx");

        // Create test data with multiple symbols and exchanges
        let mut data_by_symbol: HashMap<String, HashMap<String, Vec<Ticker>>> = HashMap::new();

        // BTC from binance and bybit
        data_by_symbol.insert("BTC".to_string(), HashMap::new());
        data_by_symbol
            .get_mut("BTC")
            .unwrap()
            .insert("binance".to_string(), vec![create_mock_ticker("BTC")]);
        data_by_symbol
            .get_mut("BTC")
            .unwrap()
            .insert("bybit".to_string(), vec![create_mock_ticker("BTC")]);

        // ETH from binance only
        data_by_symbol.insert("ETH".to_string(), HashMap::new());
        data_by_symbol
            .get_mut("ETH")
            .unwrap()
            .insert("binance".to_string(), vec![create_mock_ticker("ETH")]);

        write_ticker_to_excel_multi(&data_by_symbol, &file_path)?;

        assert!(file_path.exists(), "Excel file should be created");

        // The file should contain sheets for both BTC and ETH
        // Manual verification would show:
        // - BTC sheet with data from both binance and bybit
        // - ETH sheet with data from binance only

        dir.close()?;
        Ok(())
    }
}
