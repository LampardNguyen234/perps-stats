use anyhow::Result;
use chrono::Utc;
use csv::Writer;
use perps_aggregator::{Aggregator, IAggregator};
use perps_core::{IPerps, LiquidityDepthStats};
use perps_database::{PgPool, PostgresRepository, Repository};
use perps_exchanges::{all_exchanges, get_exchange};
use prettytable::{format, Cell, Row, Table};
use rust_xlsxwriter::{Format, Workbook};
use serde_json;
use std::collections::HashMap;
use std::fs::File;
use tokio::time::{sleep, Duration};

/// Configuration for periodic fetcher output
enum OutputConfig {
    File {
        output_file: String,
        output_dir: Option<String>,
        format: String,
    },
    Database {
        repository: PostgresRepository,
    },
}

pub async fn execute(
    exchange: Option<String>,
    symbols: String,
    format: String,
    output: Option<String>,
    output_dir: Option<String>,
    interval: Option<u64>,
    max_snapshots: usize,
    database_url: Option<String>,
) -> Result<()> {
    let aggregator = Aggregator::new();
    let requested_symbols: Vec<String> = symbols.split(',').map(|s| s.trim().to_string()).collect();

    if let Some(exchange_name) = exchange {
        execute_single_exchange(
            &aggregator,
            exchange_name,
            requested_symbols,
            format,
            output,
            output_dir,
            interval,
            max_snapshots,
            database_url,
        )
        .await
    } else {
        execute_all_exchanges(
            &aggregator,
            requested_symbols,
            format,
            output,
            output_dir,
            interval,
            max_snapshots,
            database_url,
        )
        .await
    }
}

async fn execute_all_exchanges(
    aggregator: &dyn IAggregator,
    symbols: Vec<String>,
    format: String,
    output: Option<String>,
    output_dir: Option<String>,
    interval: Option<u64>,
    max_snapshots: usize,
    database_url: Option<String>,
) -> Result<()> {
    let exchanges = all_exchanges();
    let exchange_clients: Vec<Box<dyn IPerps + Send + Sync>> = exchanges.into_iter().map(|(_, client)| client).collect();

    if let Some(interval_secs) = interval {
        let format_lower = format.to_lowercase();

        // Determine output configuration
        let output_config = if format_lower == "table" {
            // When format is table (default) and interval is set, use database
            if let Some(db_url) = database_url {
                let pool = PgPool::connect(&db_url).await?;
                let repository = PostgresRepository::new(pool);
                OutputConfig::Database { repository }
            } else {
                anyhow::bail!("Periodic fetching (--interval) without explicit format requires --database-url or DATABASE_URL environment variable");
            }
        } else if format_lower == "csv" || format_lower == "excel" {
            let output_file = output.ok_or_else(|| anyhow::anyhow!("Periodic fetching (--interval) with --format csv/excel requires --output <file>"))?;
            if let Some(ref dir) = output_dir {
                std::fs::create_dir_all(dir)?;
            }
            OutputConfig::File {
                output_file,
                output_dir,
                format: format_lower,
            }
        } else {
            anyhow::bail!("Periodic fetching (--interval) requires --format csv, --format excel, or database storage (no format specified)");
        };

        run_periodic_fetcher_all(
            aggregator,
            &exchange_clients,
            &symbols,
            interval_secs,
            max_snapshots,
            output_config,
        ).await

    } else {
        let mut all_stats = Vec::new();
        for symbol in symbols {
            match aggregator.calculate_liquidity_depth_all(&exchange_clients, &symbol).await {
                Ok(stats) => all_stats.extend(stats),
                Err(e) => tracing::error!("Failed to calculate aggregated liquidity for {}: {}", symbol, e),
            }
        }

        match format.as_str() {
            "json" => println!("{}", serde_json::to_string_pretty(&all_stats)?),
            "table" => display_aggregated_table(&all_stats)?,
            "csv" => {
                let output_file = output.ok_or_else(|| anyhow::anyhow!("CSV output requires --output <file>"))?;
                write_to_csv_all(&all_stats, &output_file, &output_dir)?;
            }
            "excel" => {
                let output_file = output.ok_or_else(|| anyhow::anyhow!("Excel output requires --output <file>"))?;
                write_to_excel_all(&all_stats, &output_file, &output_dir)?;
            }
            _ => display_aggregated_table(&all_stats)?,
        }
        Ok(())
    }
}


async fn execute_single_exchange(
    aggregator: &dyn IAggregator,
    exchange: String,
    symbols: Vec<String>,
    format: String,
    output: Option<String>,
    output_dir: Option<String>,
    interval: Option<u64>,
    max_snapshots: usize,
    database_url: Option<String>,
) -> Result<()> {
    let client = get_exchange(&exchange)?;
    let parsed_symbols: Vec<String> = symbols.iter().map(|s| client.parse_symbol(s)).collect();
    let valid_symbols = validate_symbols(client.as_ref(), &parsed_symbols).await?;

    if valid_symbols.is_empty() {
        anyhow::bail!("No valid symbols found for exchange {}", exchange);
    }

    let symbol_map: HashMap<String, String> = parsed_symbols.into_iter().zip(symbols.into_iter()).collect();

    if let Some(interval_secs) = interval {
        let format_lower = format.to_lowercase();

        // Determine output configuration
        let output_config = if format_lower == "table" {
            // When format is table (default) and interval is set, use database
            if let Some(db_url) = database_url {
                let pool = PgPool::connect(&db_url).await?;
                let repository = PostgresRepository::new(pool);
                OutputConfig::Database { repository }
            } else {
                anyhow::bail!("Periodic fetching (--interval) without explicit format requires --database-url or DATABASE_URL environment variable");
            }
        } else if format_lower == "csv" || format_lower == "excel" {
            let output_file = output.ok_or_else(|| anyhow::anyhow!("Periodic fetching (--interval) with --format csv/excel requires --output <file>"))?;
            if let Some(ref dir) = output_dir {
                std::fs::create_dir_all(dir)?;
            }
            OutputConfig::File {
                output_file,
                output_dir,
                format: format_lower,
            }
        } else {
            anyhow::bail!("Periodic fetching (--interval) requires --format csv, --format excel, or database storage (no format specified)");
        };

        run_periodic_fetcher(
            client.as_ref(),
            aggregator,
            &valid_symbols,
            &symbol_map,
            &exchange,
            interval_secs,
            max_snapshots,
            output_config,
        ).await
    } else {
        for symbol in &valid_symbols {
            let global_symbol = symbol_map.get(symbol).unwrap();
            match fetch_and_display_liquidity_data(client.as_ref(), aggregator, symbol, global_symbol, &format, &exchange).await {
                Ok(_) => (),
                Err(e) => tracing::error!("Failed to fetch liquidity for {}: {}", symbol, e),
            }
        }
        Ok(())
    }
}

async fn run_periodic_fetcher_all(
    aggregator: &dyn IAggregator,
    clients: &[Box<dyn IPerps + Send + Sync>],
    symbols: &[String],
    interval_secs: u64,
    max_snapshots: usize,
    config: OutputConfig,
) -> Result<()> {
    let output_desc = match &config {
        OutputConfig::File { format, output_file, output_dir } => {
            format!("format: {}, output: {}{}",
                format,
                output_dir.as_ref().map(|d| format!("{}/", d)).unwrap_or_default(),
                output_file)
        }
        OutputConfig::Database { .. } => "database".to_string(),
    };

    tracing::info!(
        "Starting periodic liquidity fetcher for all exchanges (interval: {}s, max_snapshots: {}, {})",
        interval_secs,
        if max_snapshots == 0 { "unlimited".to_string() } else { max_snapshots.to_string() },
        output_desc
    );

    let mut all_snapshots: Vec<LiquidityDepthStats> = Vec::new();
    let mut snapshot_count = 0;
    let unlimited = max_snapshots == 0;

    loop {
        let now = Utc::now();
        tracing::info!("[Snapshot #{}] Fetching liquidity at {}", snapshot_count + 1, now.format("%Y-%m-%d %H:%M:%S UTC"));

        let mut current_snapshot = Vec::new();
        for symbol in symbols {
            match aggregator.calculate_liquidity_depth_all(clients, symbol).await {
                Ok(stats) => current_snapshot.extend(stats),
                Err(e) => tracing::error!("Failed to calculate aggregated liquidity for {}: {}", symbol, e),
            }
        }

        snapshot_count += 1;

        match &config {
            OutputConfig::File { format, output_file, output_dir } => {
                all_snapshots.extend(current_snapshot);
                match format.as_str() {
                    "csv" => write_to_csv_all(&all_snapshots, output_file, output_dir)?,
                    "excel" => write_to_excel_all(&all_snapshots, output_file, output_dir)?,
                    _ => unreachable!("Format already validated"),
                }
                tracing::info!("✓ Written snapshot #{} to {}{}",
                    snapshot_count,
                    output_dir.as_ref().map(|d| format!("{}/", d)).unwrap_or_default(),
                    output_file
                );
            }
            OutputConfig::Database { repository } => {
                repository.store_liquidity_depth(&current_snapshot).await?;
                tracing::info!("✓ Stored snapshot #{} to database ({} records)", snapshot_count, current_snapshot.len());
            }
        }

        if !unlimited && snapshot_count >= max_snapshots {
            tracing::info!("Reached maximum snapshots ({}). Stopping.", max_snapshots);
            break;
        }

        tracing::debug!("Waiting {} seconds until next snapshot...", interval_secs);
        sleep(Duration::from_secs(interval_secs)).await;
    }

    tracing::info!("✓ Periodic fetcher completed. Total snapshots: {}", snapshot_count);
    Ok(())
}


/// Run periodic liquidity fetcher and write to file or database
async fn run_periodic_fetcher(
    client: &dyn IPerps,
    aggregator: &dyn IAggregator,
    symbols: &[String],
    symbol_map: &HashMap<String, String>,
    exchange: &str,
    interval_secs: u64,
    max_snapshots: usize,
    config: OutputConfig,
) -> Result<()> {
    let output_desc = match &config {
        OutputConfig::File { format, output_file, output_dir } => {
            format!("format: {}, output: {}{}",
                format,
                output_dir.as_ref().map(|d| format!("{}/", d)).unwrap_or_default(),
                output_file)
        }
        OutputConfig::Database { .. } => "database".to_string(),
    };

    tracing::info!(
        "Starting periodic liquidity fetcher (interval: {}s, max_snapshots: {}, {})",
        interval_secs,
        if max_snapshots == 0 { "unlimited".to_string() } else { max_snapshots.to_string() },
        output_desc
    );

    let mut data_by_symbol: HashMap<String, Vec<LiquidityDepthStats>> = HashMap::new();
    for global_symbol in symbol_map.values() {
        data_by_symbol.insert(global_symbol.clone(), Vec::new());
    }

    let mut snapshot_count = 0;
    let unlimited = max_snapshots == 0;

    loop {
        let now = Utc::now();
        tracing::info!("[Snapshot #{}] Fetching liquidity at {}", snapshot_count + 1, now.format("%Y-%m-%d %H:%M:%S UTC"));

        let mut current_snapshot = Vec::new();
        for symbol in symbols {
            let global_symbol = symbol_map.get(symbol).unwrap();
            match fetch_liquidity_data(client, aggregator, symbol, global_symbol, exchange).await {
                Ok(stats) => {
                    tracing::debug!("✓ {} - Mid: ${:.2}, Bid(10bps): ${:.2}, Ask(10bps): ${:.2}",
                        stats.symbol,
                        stats.mid_price,
                        stats.bid_10bps,
                        stats.ask_10bps
                    );
                    data_by_symbol.get_mut(global_symbol).unwrap().push(stats.clone());
                    current_snapshot.push(stats);
                }
                Err(e) => {
                    tracing::error!("Failed to fetch liquidity for {}: {}", symbol, e);
                }
            }
        }

        snapshot_count += 1;

        match &config {
            OutputConfig::File { format, output_file, output_dir } => {
                match format.as_str() {
                    "csv" => write_to_csv(&data_by_symbol, output_file, output_dir)?,
                    "excel" => write_to_excel(&data_by_symbol, output_file, output_dir)?,
                    _ => unreachable!("Format already validated"),
                }
                tracing::info!("✓ Written snapshot #{} to {}{}",
                    snapshot_count,
                    output_dir.as_ref().map(|d| format!("{}/", d)).unwrap_or_default(),
                    output_file
                );
            }
            OutputConfig::Database { repository } => {
                repository.store_liquidity_depth(&current_snapshot).await?;
                tracing::info!("✓ Stored snapshot #{} to database ({} records)", snapshot_count, current_snapshot.len());
            }
        }

        if !unlimited && snapshot_count >= max_snapshots {
            tracing::info!("Reached maximum snapshots ({}). Stopping.", max_snapshots);
            break;
        }

        tracing::debug!("Waiting {} seconds until next snapshot...", interval_secs);
        sleep(Duration::from_secs(interval_secs)).await;
    }

    tracing::info!("✓ Periodic fetcher completed. Total snapshots: {}", snapshot_count);
    Ok(())
}

/// Fetch liquidity data for a single symbol (without displaying)
async fn fetch_liquidity_data(
    client: &dyn IPerps,
    aggregator: &dyn IAggregator,
    symbol: &str,
    global_symbol: &str,
    exchange: &str,
) -> Result<LiquidityDepthStats> {
    let orderbook = client.get_orderbook(symbol, 500).await?;
    let stats = aggregator.calculate_liquidity_depth(&orderbook, exchange, global_symbol).await?;
    Ok(stats)
}

fn write_to_csv_all(
    data: &[LiquidityDepthStats],
    base_filename: &str,
    output_dir: &Option<String>,
) -> Result<()> {
    let mut data_by_symbol: HashMap<String, Vec<LiquidityDepthStats>> = HashMap::new();
    for stat in data {
        data_by_symbol.entry(stat.symbol.clone()).or_default().push(stat.clone());
    }
    write_to_csv(&data_by_symbol, base_filename, output_dir)
}

fn write_to_excel_all(
    data: &[LiquidityDepthStats],
    base_filename: &str,
    output_dir: &Option<String>,
) -> Result<()> {
    let mut data_by_symbol: HashMap<String, Vec<LiquidityDepthStats>> = HashMap::new();
    for stat in data {
        data_by_symbol.entry(stat.symbol.clone()).or_default().push(stat.clone());
    }
    write_to_excel(&data_by_symbol, base_filename, output_dir)
}


/// Write liquidity data to CSV file with separate files per symbol
fn write_to_csv(
    data_by_symbol: &HashMap<String, Vec<LiquidityDepthStats>>,
    _base_filename: &str,
    output_dir: &Option<String>,
) -> Result<()> {
    for (symbol, snapshots) in data_by_symbol {
        if snapshots.is_empty() {
            continue;
        }

        let file_name = format!("{}.csv", symbol.replace("-", "_"));

        let full_path = if let Some(dir) = output_dir {
            format!("{}/{}", dir, file_name)
        } else {
            file_name
        };

        let file = File::create(&full_path)?;
        let mut wtr = Writer::from_writer(file);

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

        for stats in snapshots.iter().rev() {
            wtr.write_record(&[
                stats.timestamp.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
                stats.exchange.clone(),
                stats.symbol.clone(),
                stats.mid_price.to_string(),
                stats.bid_1bps.to_string(),
                stats.bid_2_5bps.to_string(),
                stats.bid_5bps.to_string(),
                stats.bid_10bps.to_string(),
                stats.bid_20bps.to_string(),
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
fn write_to_excel(
    data_by_symbol: &HashMap<String, Vec<LiquidityDepthStats>>,
    base_filename: &str,
    output_dir: &Option<String>,
) -> Result<()> {
    let full_path = if let Some(dir) = output_dir {
        format!("{}/{}", dir, base_filename)
    } else {
        base_filename.to_string()
    };

    let mut workbook = Workbook::new();
    let header_format = Format::new().set_bold();
    let timestamp_format = Format::new();

    for (symbol, snapshots) in data_by_symbol {
        if snapshots.is_empty() {
            continue;
        }

        let sheet_name = symbol.replace("-", "_");

        let worksheet = workbook.add_worksheet();
        worksheet.set_name(&sheet_name)?;

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

        for (row_idx, stats) in snapshots.iter().rev().enumerate() {
            let row = (row_idx + 1) as u32;
            worksheet.write_string_with_format(row, 0, &stats.timestamp.format("%Y-%m-%d %H:%M:%S UTC").to_string(), &timestamp_format)?;
            worksheet.write_string(row, 1, &stats.exchange)?;
            worksheet.write_string(row, 2, &stats.symbol)?;
            worksheet.write_number(row, 3, stats.mid_price.to_string().parse::<f64>().unwrap_or(0.0))?;
            worksheet.write_number(row, 4, stats.bid_1bps.to_string().parse::<f64>().unwrap_or(0.0))?;
            worksheet.write_number(row, 5, stats.bid_2_5bps.to_string().parse::<f64>().unwrap_or(0.0))?;
            worksheet.write_number(row, 6, stats.bid_5bps.to_string().parse::<f64>().unwrap_or(0.0))?;
            worksheet.write_number(row, 7, stats.bid_10bps.to_string().parse::<f64>().unwrap_or(0.0))?;
            worksheet.write_number(row, 8, stats.bid_20bps.to_string().parse::<f64>().unwrap_or(0.0))?;
            worksheet.write_number(row, 9, stats.ask_1bps.to_string().parse::<f64>().unwrap_or(0.0))?;
            worksheet.write_number(row, 10, stats.ask_2_5bps.to_string().parse::<f64>().unwrap_or(0.0))?;
            worksheet.write_number(row, 11, stats.ask_5bps.to_string().parse::<f64>().unwrap_or(0.0))?;
            worksheet.write_number(row, 12, stats.ask_10bps.to_string().parse::<f64>().unwrap_or(0.0))?;
            worksheet.write_number(row, 13, stats.ask_20bps.to_string().parse::<f64>().unwrap_or(0.0))?;
        }

        worksheet.set_column_width(0, 25)?;
        worksheet.set_column_width(1, 12)?;
        worksheet.set_column_width(2, 15)?;
        for col in 3..14 {
            worksheet.set_column_width(col, 15)?;
        }
    }

    workbook.save(&full_path)?;

    Ok(())
}

async fn fetch_and_display_liquidity_data(
    client: &dyn IPerps,
    aggregator: &dyn IAggregator,
    symbol: &str,
    global_symbol: &str,
    format: &str,
    exchange: &str,
) -> Result<()> {
    let orderbook = client.get_orderbook(symbol, 1000).await?;
    let stats = aggregator.calculate_liquidity_depth(&orderbook, exchange, global_symbol).await?;

    match format.to_lowercase().as_str() {
        "json" => display_json(&stats)?,
        "table" => display_table(&stats)?,
        _ => display_table(&stats)?,
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

fn display_aggregated_table(stats: &[LiquidityDepthStats]) -> Result<()> {
    if stats.is_empty() {
        println!("No liquidity data found.");
        return Ok(());
    }

    let mut table = Table::new();
    table.set_format(*format::consts::FORMAT_NO_LINESEP_WITH_TITLE);

    table.set_titles(Row::new(vec![Cell::new(&format!(
        "Aggregated Liquidity Depth for {}",
        stats[0].symbol
    ))
    .with_hspan(7)]));

    table.add_row(Row::new(vec![
        Cell::new("Exchange").with_style(prettytable::Attr::Bold),
        Cell::new("Mid Price").with_style(prettytable::Attr::Bold),
        Cell::new("1 bps").with_style(prettytable::Attr::Bold),
        Cell::new("2.5 bps").with_style(prettytable::Attr::Bold),
        Cell::new("5 bps").with_style(prettytable::Attr::Bold),
        Cell::new("10 bps").with_style(prettytable::Attr::Bold),
        Cell::new("20 bps").with_style(prettytable::Attr::Bold),
    ]));

    for stat in stats {
        table.add_row(Row::new(vec![
            Cell::new(&stat.exchange.to_uppercase()),
            Cell::new_align(&format!("${:.2}", stat.mid_price), format::Alignment::RIGHT),
            Cell::new_align(
                &format!("${:.2}", stat.bid_1bps + stat.ask_1bps),
                format::Alignment::RIGHT,
            ),
            Cell::new_align(
                &format!("${:.2}", stat.bid_2_5bps + stat.ask_2_5bps),
                format::Alignment::RIGHT,
            ),
            Cell::new_align(
                &format!("${:.2}", stat.bid_5bps + stat.ask_5bps),
                format::Alignment::RIGHT,
            ),
            Cell::new_align(
                &format!("${:.2}", stat.bid_10bps + stat.ask_10bps),
                format::Alignment::RIGHT,
            ),
            Cell::new_align(
                &format!("${:.2}", stat.bid_20bps + stat.ask_20bps),
                format::Alignment::RIGHT,
            ),
        ]));
    }

    println!();
    table.printstd();
    println!();

    Ok(())
}

fn display_json(stats: &LiquidityDepthStats) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(stats)?);
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

#[cfg(test)]
mod tests {
    use super::*;
    use perps_core::LiquidityDepthStats;
    use rust_decimal_macros::dec;
    use std::collections::HashMap;
    use tempfile::tempdir;

    fn create_mock_stats(exchange: &str, symbol: &str) -> LiquidityDepthStats {
        LiquidityDepthStats {
            timestamp: Utc::now(),
            exchange: exchange.to_string(),
            symbol: symbol.to_string(),
            mid_price: dec!(50000),
            bid_1bps: dec!(1000),
            bid_2_5bps: dec!(2000),
            bid_5bps: dec!(3000),
            bid_10bps: dec!(4000),
            bid_20bps: dec!(5000),
            ask_1bps: dec!(1000),
            ask_2_5bps: dec!(2000),
            ask_5bps: dec!(3000),
            ask_10bps: dec!(4000),
            ask_20bps: dec!(5000),
        }
    }

    #[test]
    fn test_write_to_csv_naming() -> Result<()> {
        let dir = tempdir()?;
        let output_dir = Some(dir.path().to_str().unwrap().to_string());

        // Test that data for a symbol, even from multiple exchanges, goes into one file named after the symbol.
        let mut multi_exchange_data = HashMap::new();
        multi_exchange_data.insert(
            "BTC-USDT".to_string(),
            vec![
                create_mock_stats("binance", "BTC-USDT"),
                create_mock_stats("lighter", "BTC-USDT"),
            ],
        );
        write_to_csv(&multi_exchange_data, "ignored.csv", &output_dir)?;
        
        let expected_file = dir.path().join("BTC-USDT.csv");
        assert!(expected_file.exists());

        dir.close()?;
        Ok(())
    }

    #[test]
    fn test_write_to_excel_naming() -> Result<()> {
        let dir = tempdir()?;
        let output_dir = Some(dir.path().to_str().unwrap().to_string());
        let filename = "test.xlsx";

        // Test that data for a symbol, even from multiple exchanges, goes into one sheet named after the symbol.
        let mut multi_exchange_data = HashMap::new();
        multi_exchange_data.insert(
            "BTC-USDT".to_string(),
            vec![
                create_mock_stats("binance", "BTC-USDT"),
                create_mock_stats("lighter", "BTC-USDT"),
            ],
        );
        write_to_excel(&multi_exchange_data, filename, &output_dir)?;
        
        let full_path = dir.path().join(filename);
        assert!(full_path.exists());

        // We can't easily check the sheet name here, but the implementation is changed to use the symbol name.
        // This test now primarily ensures the file is created with the simplified logic.

        dir.close()?;
        Ok(())
    }
}
