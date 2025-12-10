use anyhow::Result;
use chrono::Utc;
use csv::Writer;
use perps_aggregator::{Aggregator, IAggregator};
use perps_core::{IPerps, LiquidityDepthStats, MultiResolutionOrderbook, Orderbook, Slippage};
use perps_database::Repository;
use perps_exchanges::{all_exchanges, get_exchange};
use prettytable::{format, Cell, Row, Table};
use rust_xlsxwriter::{Format, Workbook};
use serde_json;
use std::collections::HashMap;
use std::fs::File;
use tokio::time::{sleep, Duration};

use super::common::{
    determine_output_config, format_output_description, validate_symbols, OutputConfig,
};

/// Combined data structure for liquidity depth, slippage, and orderbook
#[derive(Clone, Debug, serde::Serialize)]
struct LiquidityData {
    liquidity: LiquidityDepthStats,
    slippage: Vec<Slippage>,
    #[serde(skip_serializing)]
    orderbook: MultiResolutionOrderbook,
}

/// Arguments for the liquidity command
pub struct LiquidityArgs {
    pub exchange: Option<String>,
    pub symbols: String,
    pub format: String,
    pub output: Option<String>,
    pub output_dir: Option<String>,
    pub interval: Option<u64>,
    pub max_snapshots: usize,
    pub database_url: Option<String>,
    pub exclude_fees: bool,
    pub override_fee: Option<f64>,
}

pub async fn execute(args: LiquidityArgs) -> Result<()> {
    let LiquidityArgs {
        exchange,
        symbols,
        format,
        output,
        output_dir,
        interval,
        max_snapshots,
        database_url,
        exclude_fees,
        override_fee,
    } = args;
    let aggregator = Aggregator::new();
    let requested_symbols: Vec<String> = symbols.split(',').map(|s| s.trim().to_string()).collect();

    if let Some(exchange_str) = exchange {
        // Check if comma-separated exchanges
        if exchange_str.contains(',') {
            let exchange_names: Vec<String> = exchange_str
                .split(',')
                .map(|s| s.trim().to_string())
                .collect();
            execute_selected_exchanges(
                &aggregator,
                exchange_names,
                requested_symbols,
                format,
                output,
                output_dir,
                interval,
                max_snapshots,
                database_url,
                exclude_fees,
                override_fee,
            )
            .await
        } else {
            execute_single_exchange(
                &aggregator,
                exchange_str,
                requested_symbols,
                format,
                output,
                output_dir,
                interval,
                max_snapshots,
                database_url,
                exclude_fees,
                override_fee,
            )
            .await
        }
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
            exclude_fees,
            override_fee,
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
    exclude_fees: bool,
    override_fee: Option<f64>,
) -> Result<()> {
    let exchanges = all_exchanges().await;
    let exchange_names: Vec<String> = exchanges.iter().map(|(name, _)| name.clone()).collect();
    let exchange_clients: Vec<Box<dyn IPerps + Send + Sync>> =
        exchanges.into_iter().map(|(_, client)| client).collect();

    if let Some(interval_secs) = interval {
        let output_config =
            determine_output_config(&format, output, &output_dir, database_url).await?;

        run_periodic_fetcher_all(
            aggregator,
            &exchange_clients,
            &exchange_names,
            &symbols,
            interval_secs,
            max_snapshots,
            output_config,
            exclude_fees,
            override_fee,
        )
        .await
    } else {
        let mut all_stats = Vec::new();
        for symbol in symbols {
            match aggregator
                .calculate_liquidity_depth_all(&exchange_clients, &symbol)
                .await
            {
                Ok(stats) => all_stats.extend(stats),
                Err(e) => tracing::error!(
                    "Failed to calculate aggregated liquidity for {}: {}",
                    symbol,
                    e
                ),
            }
        }

        match format.as_str() {
            "json" => println!("{}", serde_json::to_string_pretty(&all_stats)?),
            "table" => display_aggregated_table(&all_stats)?,
            "csv" => {
                let output_file =
                    output.ok_or_else(|| anyhow::anyhow!("CSV output requires --output <file>"))?;
                write_to_csv_all(&all_stats, &output_file, &output_dir)?;
            }
            "excel" => {
                let output_file = output
                    .ok_or_else(|| anyhow::anyhow!("Excel output requires --output <file>"))?;
                write_to_excel_all(&all_stats, &output_file, &output_dir)?;
            }
            _ => display_aggregated_table(&all_stats)?,
        }
        Ok(())
    }
}

async fn execute_selected_exchanges(
    aggregator: &dyn IAggregator,
    exchange_names: Vec<String>,
    symbols: Vec<String>,
    format: String,
    output: Option<String>,
    output_dir: Option<String>,
    interval: Option<u64>,
    max_snapshots: usize,
    database_url: Option<String>,
    exclude_fees: bool,
    override_fee: Option<f64>,
) -> Result<()> {
    // Initialize clients for selected exchanges
    let mut exchange_clients: Vec<Box<dyn IPerps + Send + Sync>> = Vec::new();
    for exchange_name in &exchange_names {
        match get_exchange(exchange_name).await {
            Ok(client) => exchange_clients.push(client),
            Err(e) => {
                tracing::error!("Failed to initialize exchange {}: {}", exchange_name, e);
                anyhow::bail!("Failed to initialize exchange {}: {}", exchange_name, e);
            }
        }
    }

    if exchange_clients.is_empty() {
        anyhow::bail!("No valid exchanges found");
    }

    if let Some(interval_secs) = interval {
        let output_config =
            determine_output_config(&format, output, &output_dir, database_url).await?;

        run_periodic_fetcher_selected(
            aggregator,
            &exchange_clients,
            &exchange_names,
            &symbols,
            interval_secs,
            max_snapshots,
            output_config,
            exclude_fees,
            override_fee,
        )
        .await
    } else {
        let mut all_stats = Vec::new();
        for symbol in symbols {
            match aggregator
                .calculate_liquidity_depth_all(&exchange_clients, &symbol)
                .await
            {
                Ok(stats) => all_stats.extend(stats),
                Err(e) => tracing::error!(
                    "Failed to calculate aggregated liquidity for {}: {}",
                    symbol,
                    e
                ),
            }
        }

        match format.as_str() {
            "json" => println!("{}", serde_json::to_string_pretty(&all_stats)?),
            "table" => display_aggregated_table(&all_stats)?,
            "csv" => {
                let output_file =
                    output.ok_or_else(|| anyhow::anyhow!("CSV output requires --output <file>"))?;
                write_to_csv_all(&all_stats, &output_file, &output_dir)?;
            }
            "excel" => {
                let output_file = output
                    .ok_or_else(|| anyhow::anyhow!("Excel output requires --output <file>"))?;
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
    exclude_fees: bool,
    override_fee: Option<f64>,
) -> Result<()> {
    let client = get_exchange(&exchange).await?;
    let parsed_symbols: Vec<String> = symbols.iter().map(|s| client.parse_symbol(s)).collect();
    let valid_symbols = validate_symbols(client.as_ref(), &parsed_symbols).await?;

    if valid_symbols.is_empty() {
        anyhow::bail!("No valid symbols found for exchange {}", exchange);
    }

    let symbol_map: HashMap<String, String> = parsed_symbols
        .into_iter()
        .zip(symbols.into_iter())
        .collect();

    if let Some(interval_secs) = interval {
        let output_config =
            determine_output_config(&format, output, &output_dir, database_url).await?;

        run_periodic_fetcher(
            client.as_ref(),
            aggregator,
            &valid_symbols,
            &symbol_map,
            &exchange,
            interval_secs,
            max_snapshots,
            output_config,
            exclude_fees,
            override_fee,
        )
        .await
    } else {
        // Non-periodic mode (immediate display) - no database, so no fees
        for symbol in &valid_symbols {
            let global_symbol = symbol_map.get(symbol).unwrap();
            match fetch_and_display_liquidity_data(
                client.as_ref(),
                aggregator,
                symbol,
                global_symbol,
                &format,
                &exchange,
            )
            .await
            {
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
    exchange_names: &[String],
    symbols: &[String],
    interval_secs: u64,
    max_snapshots: usize,
    config: OutputConfig,
    _exclude_fees: bool,
    _override_fee: Option<f64>,
) -> Result<()> {
    let output_desc = format_output_description(&config);

    tracing::info!(
        "Starting periodic liquidity fetcher for all exchanges (interval: {}s, max_snapshots: {}, {})",
        interval_secs,
        if max_snapshots == 0 { "unlimited".to_string() } else { max_snapshots.to_string() },
        output_desc
    );

    let mut snapshot_count = 0;
    let unlimited = max_snapshots == 0;

    // For file output, we need to accumulate data by symbol
    let mut data_by_symbol: HashMap<String, Vec<LiquidityData>> = HashMap::new();

    loop {
        let now = Utc::now();
        tracing::info!(
            "[Snapshot #{}] Fetching liquidity and slippage at {}",
            snapshot_count + 1,
            now.format("%Y-%m-%d %H:%M:%S UTC")
        );

        let mut current_liquidity_snapshot = Vec::new();
        let mut slippage_by_exchange: HashMap<String, Vec<Slippage>> = HashMap::new();
        let mut orderbooks_by_exchange: HashMap<String, Vec<Orderbook>> = HashMap::new();

        // Fetch data for each exchange-symbol combination
        for (client, exchange_name) in clients.iter().zip(exchange_names.iter()) {
            for symbol in symbols {
                let parsed_symbol = client.parse_symbol(symbol);
                match fetch_liquidity_data(
                    client.as_ref(),
                    aggregator,
                    &parsed_symbol,
                    symbol,
                    exchange_name,
                )
                .await
                {
                    Ok(data) => {
                        tracing::debug!("✓ {}/{} - Mid: ${:.2}, Bid(10bps): ${:.2}, Ask(10bps): ${:.2}, Slippage(10K): {:.2} bps / {:.2} bps",
                            exchange_name,
                            data.liquidity.symbol,
                            data.liquidity.mid_price,
                            data.liquidity.bid_10bps,
                            data.liquidity.ask_10bps,
                            data.slippage.get(1).and_then(|s| s.buy_slippage_bps).unwrap_or_default(),
                            data.slippage.get(1).and_then(|s| s.sell_slippage_bps).unwrap_or_default()
                        );

                        // For file output
                        data_by_symbol
                            .entry(symbol.clone())
                            .or_default()
                            .push(data.clone());

                        // For database output - group by exchange
                        current_liquidity_snapshot.push(data.liquidity.clone());
                        slippage_by_exchange
                            .entry(exchange_name.clone())
                            .or_default()
                            .extend(data.slippage.clone());
                        // Extract best orderbook for storage (prefer tight spreads)
                        if let Some(orderbook) = data.orderbook.best_for_tight_spreads() {
                            orderbooks_by_exchange
                                .entry(exchange_name.clone())
                                .or_default()
                                .push(orderbook.clone());
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            "Failed to fetch liquidity for {}/{}: {}",
                            exchange_name,
                            symbol,
                            e
                        );
                    }
                }
            }
        }

        snapshot_count += 1;

        match &config {
            OutputConfig::File {
                format,
                output_file,
                output_dir,
            } => {
                match format.as_str() {
                    "csv" => write_to_csv(&data_by_symbol, output_file, output_dir)?,
                    "excel" => write_to_excel(&data_by_symbol, output_file, output_dir)?,
                    _ => unreachable!("Format already validated"),
                }
                tracing::info!(
                    "✓ Written snapshot #{} to {}{}",
                    snapshot_count,
                    output_dir
                        .as_ref()
                        .map(|d| format!("{}/", d))
                        .unwrap_or_default(),
                    output_file
                );
            }
            OutputConfig::Database { repository } => {
                // Store liquidity depth
                repository
                    .store_liquidity_depth(&current_liquidity_snapshot)
                    .await?;

                // Store slippage for each exchange
                for (exchange_name, slippages) in &slippage_by_exchange {
                    if !slippages.is_empty() {
                        repository
                            .store_slippage_with_exchange(exchange_name, slippages)
                            .await?;
                    }
                }

                // Store orderbooks for each exchange
                for (exchange_name, orderbooks) in &orderbooks_by_exchange {
                    if !orderbooks.is_empty() {
                        repository
                            .store_orderbooks_with_exchange(exchange_name, orderbooks)
                            .await?;
                    }
                }

                let total_slippage: usize = slippage_by_exchange.values().map(|v| v.len()).sum();
                let total_orderbooks: usize =
                    orderbooks_by_exchange.values().map(|v| v.len()).sum();
                tracing::info!("✓ Stored snapshot #{} to database ({} liquidity + {} slippage + {} orderbook records)",
                    snapshot_count, current_liquidity_snapshot.len(), total_slippage, total_orderbooks);
            }
        }

        if !unlimited && snapshot_count >= max_snapshots {
            tracing::info!("Reached maximum snapshots ({}). Stopping.", max_snapshots);
            break;
        }

        tracing::debug!("Waiting {} seconds until next snapshot...", interval_secs);
        sleep(Duration::from_secs(interval_secs)).await;
    }

    tracing::info!(
        "✓ Periodic fetcher completed. Total snapshots: {}",
        snapshot_count
    );
    Ok(())
}

/// Run periodic liquidity fetcher for selected exchanges with full data storage
async fn run_periodic_fetcher_selected(
    aggregator: &dyn IAggregator,
    clients: &[Box<dyn IPerps + Send + Sync>],
    exchange_names: &[String],
    symbols: &[String],
    interval_secs: u64,
    max_snapshots: usize,
    config: OutputConfig,
    _exclude_fees: bool,
    _override_fee: Option<f64>,
) -> Result<()> {
    let output_desc = format_output_description(&config);

    tracing::info!(
        "Starting periodic liquidity fetcher for selected exchanges [{}] (interval: {}s, max_snapshots: {}, {})",
        exchange_names.join(", "),
        interval_secs,
        if max_snapshots == 0 { "unlimited".to_string() } else { max_snapshots.to_string() },
        output_desc
    );

    let mut snapshot_count = 0;
    let unlimited = max_snapshots == 0;

    // For file output, we need to accumulate data by symbol
    let mut data_by_symbol: HashMap<String, Vec<LiquidityData>> = HashMap::new();

    loop {
        let now = Utc::now();
        tracing::info!(
            "[Snapshot #{}] Fetching liquidity and slippage at {}",
            snapshot_count + 1,
            now.format("%Y-%m-%d %H:%M:%S UTC")
        );

        let mut current_liquidity_snapshot = Vec::new();
        let mut slippage_by_exchange: HashMap<String, Vec<Slippage>> = HashMap::new();
        let mut orderbooks_by_exchange: HashMap<String, Vec<Orderbook>> = HashMap::new();

        // Fetch data for each exchange-symbol combination
        for (client, exchange_name) in clients.iter().zip(exchange_names.iter()) {
            for symbol in symbols {
                let parsed_symbol = client.parse_symbol(symbol);
                match fetch_liquidity_data(
                    client.as_ref(),
                    aggregator,
                    &parsed_symbol,
                    symbol,
                    exchange_name,
                )
                .await
                {
                    Ok(data) => {
                        tracing::debug!("✓ {}/{} - Mid: ${:.2}, Bid(10bps): ${:.2}, Ask(10bps): ${:.2}, Slippage(10K): {:.2} bps / {:.2} bps",
                            exchange_name,
                            data.liquidity.symbol,
                            data.liquidity.mid_price,
                            data.liquidity.bid_10bps,
                            data.liquidity.ask_10bps,
                            data.slippage.get(1).and_then(|s| s.buy_slippage_bps).unwrap_or_default(),
                            data.slippage.get(1).and_then(|s| s.sell_slippage_bps).unwrap_or_default()
                        );

                        // For file output
                        data_by_symbol
                            .entry(symbol.clone())
                            .or_default()
                            .push(data.clone());

                        // For database output - group by exchange
                        current_liquidity_snapshot.push(data.liquidity.clone());
                        slippage_by_exchange
                            .entry(exchange_name.clone())
                            .or_default()
                            .extend(data.slippage.clone());
                        // Extract best orderbook for storage (prefer tight spreads)
                        if let Some(orderbook) = data.orderbook.best_for_tight_spreads() {
                            orderbooks_by_exchange
                                .entry(exchange_name.clone())
                                .or_default()
                                .push(orderbook.clone());
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            "Failed to fetch liquidity for {}/{}: {}",
                            exchange_name,
                            symbol,
                            e
                        );
                    }
                }
            }
        }

        snapshot_count += 1;

        match &config {
            OutputConfig::File {
                format,
                output_file,
                output_dir,
            } => {
                match format.as_str() {
                    "csv" => write_to_csv(&data_by_symbol, output_file, output_dir)?,
                    "excel" => write_to_excel(&data_by_symbol, output_file, output_dir)?,
                    _ => unreachable!("Format already validated"),
                }
                tracing::info!(
                    "✓ Written snapshot #{} to {}{}",
                    snapshot_count,
                    output_dir
                        .as_ref()
                        .map(|d| format!("{}/", d))
                        .unwrap_or_default(),
                    output_file
                );
            }
            OutputConfig::Database { repository } => {
                // Store liquidity depth
                repository
                    .store_liquidity_depth(&current_liquidity_snapshot)
                    .await?;

                // Store slippage for each exchange
                for (exchange_name, slippages) in &slippage_by_exchange {
                    if !slippages.is_empty() {
                        repository
                            .store_slippage_with_exchange(exchange_name, slippages)
                            .await?;
                    }
                }

                // Store orderbooks for each exchange
                for (exchange_name, orderbooks) in &orderbooks_by_exchange {
                    if !orderbooks.is_empty() {
                        repository
                            .store_orderbooks_with_exchange(exchange_name, orderbooks)
                            .await?;
                    }
                }

                let total_slippage: usize = slippage_by_exchange.values().map(|v| v.len()).sum();
                let total_orderbooks: usize =
                    orderbooks_by_exchange.values().map(|v| v.len()).sum();
                tracing::info!("✓ Stored snapshot #{} to database ({} liquidity + {} slippage + {} orderbook records)",
                    snapshot_count, current_liquidity_snapshot.len(), total_slippage, total_orderbooks);
            }
        }

        if !unlimited && snapshot_count >= max_snapshots {
            tracing::info!("Reached maximum snapshots ({}). Stopping.", max_snapshots);
            break;
        }

        tracing::debug!("Waiting {} seconds until next snapshot...", interval_secs);
        sleep(Duration::from_secs(interval_secs)).await;
    }

    tracing::info!(
        "✓ Periodic fetcher completed. Total snapshots: {}",
        snapshot_count
    );
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
    _exclude_fees: bool,
    _override_fee: Option<f64>,
) -> Result<()> {
    let output_desc = format_output_description(&config);

    tracing::info!(
        "Starting periodic liquidity fetcher (interval: {}s, max_snapshots: {}, {})",
        interval_secs,
        if max_snapshots == 0 {
            "unlimited".to_string()
        } else {
            max_snapshots.to_string()
        },
        output_desc
    );

    let mut data_by_symbol: HashMap<String, Vec<LiquidityData>> = HashMap::new();
    for global_symbol in symbol_map.values() {
        data_by_symbol.insert(global_symbol.clone(), Vec::new());
    }

    let mut snapshot_count = 0;
    let unlimited = max_snapshots == 0;

    loop {
        let now = Utc::now();
        tracing::info!(
            "[Snapshot #{}] Fetching liquidity and slippage at {}",
            snapshot_count + 1,
            now.format("%Y-%m-%d %H:%M:%S UTC")
        );

        let mut current_liquidity_snapshot = Vec::new();
        let mut current_slippage_snapshot = Vec::new();
        let mut current_orderbook_snapshot = Vec::new();
        for symbol in symbols {
            let global_symbol = symbol_map.get(symbol).unwrap();
            match fetch_liquidity_data(client, aggregator, symbol, global_symbol, exchange).await {
                Ok(data) => {
                    tracing::debug!("✓ {} - Mid: ${:.2}, Bid(10bps): ${:.2}, Ask(10bps): ${:.2}, Slippage(10K): {:.2} bps / {:.2} bps",
                        data.liquidity.symbol,
                        data.liquidity.mid_price,
                        data.liquidity.bid_10bps,
                        data.liquidity.ask_10bps,
                        data.slippage.get(1).and_then(|s| s.buy_slippage_bps).unwrap_or_default(),
                        data.slippage.get(1).and_then(|s| s.sell_slippage_bps).unwrap_or_default()
                    );
                    data_by_symbol
                        .get_mut(global_symbol)
                        .unwrap()
                        .push(data.clone());
                    current_liquidity_snapshot.push(data.liquidity.clone());
                    current_slippage_snapshot.extend(data.slippage.clone());
                    // Extract best orderbook for storage (prefer tight spreads)
                    if let Some(orderbook) = data.orderbook.best_for_tight_spreads() {
                        current_orderbook_snapshot.push(orderbook.clone());
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to fetch liquidity for {}: {}", symbol, e);
                }
            }
        }

        snapshot_count += 1;

        match &config {
            OutputConfig::File {
                format,
                output_file,
                output_dir,
            } => {
                match format.as_str() {
                    "csv" => write_to_csv(&data_by_symbol, output_file, output_dir)?,
                    "excel" => write_to_excel(&data_by_symbol, output_file, output_dir)?,
                    _ => unreachable!("Format already validated"),
                }
                tracing::info!(
                    "✓ Written snapshot #{} to {}{}",
                    snapshot_count,
                    output_dir
                        .as_ref()
                        .map(|d| format!("{}/", d))
                        .unwrap_or_default(),
                    output_file
                );
            }
            OutputConfig::Database { repository } => {
                repository
                    .store_liquidity_depth(&current_liquidity_snapshot)
                    .await?;
                repository
                    .store_slippage_with_exchange(exchange, &current_slippage_snapshot)
                    .await?;
                repository
                    .store_orderbooks_with_exchange(exchange, &current_orderbook_snapshot)
                    .await?;
                tracing::info!("✓ Stored snapshot #{} to database ({} liquidity + {} slippage + {} orderbook records)",
                    snapshot_count, current_liquidity_snapshot.len(), current_slippage_snapshot.len(), current_orderbook_snapshot.len());
            }
        }

        if !unlimited && snapshot_count >= max_snapshots {
            tracing::info!("Reached maximum snapshots ({}). Stopping.", max_snapshots);
            break;
        }

        tracing::debug!("Waiting {} seconds until next snapshot...", interval_secs);
        sleep(Duration::from_secs(interval_secs)).await;
    }

    tracing::info!(
        "✓ Periodic fetcher completed. Total snapshots: {}",
        snapshot_count
    );
    Ok(())
}

/// Fetch liquidity data and slippage for a single symbol (without displaying)
async fn fetch_liquidity_data(
    client: &dyn IPerps,
    aggregator: &dyn IAggregator,
    symbol: &str,
    global_symbol: &str,
    exchange: &str,
) -> Result<LiquidityData> {
    // Fetch orderbook once and calculate both liquidity depth and slippage
    let multi_orderbook = client.get_orderbook(symbol, 1000).await?;

    // MultiResolutionOrderbook methods automatically select best resolution
    let liquidity = aggregator
        .calculate_liquidity_depth(&multi_orderbook, exchange, global_symbol)
        .await?;
    let slippage = aggregator.calculate_all_slippages(&multi_orderbook);

    Ok(LiquidityData {
        liquidity,
        slippage,
        orderbook: multi_orderbook,
    })
}

fn write_to_csv_all(
    data: &[LiquidityDepthStats],
    base_filename: &str,
    output_dir: &Option<String>,
) -> Result<()> {
    let mut data_by_symbol: HashMap<String, Vec<LiquidityDepthStats>> = HashMap::new();
    for stat in data {
        data_by_symbol
            .entry(stat.symbol.clone())
            .or_default()
            .push(stat.clone());
    }
    write_to_csv_liquidity_only(&data_by_symbol, base_filename, output_dir)
}

fn write_to_excel_all(
    data: &[LiquidityDepthStats],
    base_filename: &str,
    output_dir: &Option<String>,
) -> Result<()> {
    let mut data_by_symbol: HashMap<String, Vec<LiquidityDepthStats>> = HashMap::new();
    for stat in data {
        data_by_symbol
            .entry(stat.symbol.clone())
            .or_default()
            .push(stat.clone());
    }
    write_to_excel_liquidity_only(&data_by_symbol, base_filename, output_dir)
}

/// Write liquidity data only (no slippage) to CSV - used for multi-exchange mode
fn write_to_csv_liquidity_only(
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

/// Write liquidity data only (no slippage) to Excel - used for multi-exchange mode
fn write_to_excel_liquidity_only(
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
            worksheet.write_string_with_format(
                row,
                0,
                stats.timestamp.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
                &timestamp_format,
            )?;
            worksheet.write_string(row, 1, &stats.exchange)?;
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

/// Write liquidity data and slippage to CSV file with separate files per symbol
fn write_to_csv(
    data_by_symbol: &HashMap<String, Vec<LiquidityData>>,
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
            "Slippage $1K Buy (bps)",
            "Slippage $1K Sell (bps)",
            "Slippage $10K Buy (bps)",
            "Slippage $10K Sell (bps)",
            "Slippage $50K Buy (bps)",
            "Slippage $50K Sell (bps)",
            "Slippage $100K Buy (bps)",
            "Slippage $100K Sell (bps)",
            "Slippage $500K Buy (bps)",
            "Slippage $500K Sell (bps)",
            "Slippage $5M Buy (bps)",
            "Slippage $5M Sell (bps)",
            "Slippage $10M Buy (bps)",
            "Slippage $10M Sell (bps)",
        ])?;

        for data in snapshots.iter().rev() {
            let stats = &data.liquidity;
            let slippages = &data.slippage;

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
                slippages
                    .get(0)
                    .and_then(|s| s.buy_slippage_bps)
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
                slippages
                    .get(0)
                    .and_then(|s| s.sell_slippage_bps)
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
                slippages
                    .get(1)
                    .and_then(|s| s.buy_slippage_bps)
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
                slippages
                    .get(1)
                    .and_then(|s| s.sell_slippage_bps)
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
                slippages
                    .get(2)
                    .and_then(|s| s.buy_slippage_bps)
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
                slippages
                    .get(2)
                    .and_then(|s| s.sell_slippage_bps)
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
                slippages
                    .get(3)
                    .and_then(|s| s.buy_slippage_bps)
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
                slippages
                    .get(3)
                    .and_then(|s| s.sell_slippage_bps)
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
                slippages
                    .get(4)
                    .and_then(|s| s.buy_slippage_bps)
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
                slippages
                    .get(4)
                    .and_then(|s| s.sell_slippage_bps)
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
                slippages
                    .get(5)
                    .and_then(|s| s.buy_slippage_bps)
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
                slippages
                    .get(5)
                    .and_then(|s| s.sell_slippage_bps)
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
                slippages
                    .get(6)
                    .and_then(|s| s.buy_slippage_bps)
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
                slippages
                    .get(6)
                    .and_then(|s| s.sell_slippage_bps)
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
            ])?;
        }

        wtr.flush()?;
    }

    Ok(())
}

/// Write liquidity data and slippage to Excel file with separate sheets per symbol
fn write_to_excel(
    data_by_symbol: &HashMap<String, Vec<LiquidityData>>,
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
            "Slip $1K Buy",
            "Slip $1K Sell",
            "Slip $10K Buy",
            "Slip $10K Sell",
            "Slip $50K Buy",
            "Slip $50K Sell",
            "Slip $100K Buy",
            "Slip $100K Sell",
            "Slip $500K Buy",
            "Slip $500K Sell",
            "Slip $5M Buy",
            "Slip $5M Sell",
            "Slip $10M Buy",
            "Slip $10M Sell",
        ];

        for (col, header) in headers.iter().enumerate() {
            worksheet.write_string_with_format(0, col as u16, *header, &header_format)?;
        }

        for (row_idx, data) in snapshots.iter().rev().enumerate() {
            let row = (row_idx + 1) as u32;
            let stats = &data.liquidity;
            let slippages = &data.slippage;

            worksheet.write_string_with_format(
                row,
                0,
                stats.timestamp.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
                &timestamp_format,
            )?;
            worksheet.write_string(row, 1, &stats.exchange)?;
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

            // Write slippage columns
            for (i, slippage) in slippages.iter().take(7).enumerate() {
                let base_col = 14 + i * 2;
                let buy_val = slippage
                    .buy_slippage_bps
                    .map(|v| v.to_string().parse::<f64>().unwrap_or(0.0))
                    .unwrap_or(0.0);
                let sell_val = slippage
                    .sell_slippage_bps
                    .map(|v| v.to_string().parse::<f64>().unwrap_or(0.0))
                    .unwrap_or(0.0);
                worksheet.write_number(row, base_col as u16, buy_val)?;
                worksheet.write_number(row, (base_col + 1) as u16, sell_val)?;
            }
        }

        worksheet.set_column_width(0, 25)?;
        worksheet.set_column_width(1, 12)?;
        worksheet.set_column_width(2, 15)?;
        for col in 3..28 {
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
    // Fetch orderbook once and calculate both liquidity depth and slippage
    let multi_orderbook = client.get_orderbook(symbol, 1000).await?;

    // MultiResolutionOrderbook methods automatically select best resolution
    let liquidity = aggregator
        .calculate_liquidity_depth(&multi_orderbook, exchange, global_symbol)
        .await?;
    let slippage = aggregator.calculate_all_slippages(&multi_orderbook);

    let data = LiquidityData {
        liquidity,
        slippage,
        orderbook: multi_orderbook,
    };

    match format.to_lowercase().as_str() {
        "json" => display_json_combined(&data)?,
        "table" => display_table_combined(&data)?,
        _ => display_table_combined(&data)?,
    }

    Ok(())
}

fn _display_table(stats: &LiquidityDepthStats) -> Result<()> {
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

    table.add_row(Row::new(vec![
        Cell::new("Timestamp").with_style(prettytable::Attr::Bold),
        Cell::new_align(
            &stats.timestamp.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
            format::Alignment::RIGHT,
        )
        .with_hspan(2),
    ]));

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

fn _display_json(stats: &LiquidityDepthStats) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(stats)?);
    Ok(())
}

fn display_table_combined(data: &LiquidityData) -> Result<()> {
    let stats = &data.liquidity;

    // Display liquidity depth table
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

    println!();
    table.printstd();
    println!();

    // Display slippage table
    let mut slip_table = Table::new();
    slip_table.set_format(*format::consts::FORMAT_NO_LINESEP_WITH_TITLE);

    slip_table.set_titles(Row::new(vec![Cell::new(&format!(
        "Slippage Analysis: {}/{} (Mid Price: {})",
        stats.exchange.to_uppercase(),
        stats.symbol,
        stats.mid_price.round_dp(4)
    ))
    .with_hspan(3)]));

    slip_table.add_row(Row::new(vec![
        Cell::new("Trade Amount").with_style(prettytable::Attr::Bold),
        Cell::new("Buy Slippage (bps)").with_style(prettytable::Attr::Bold),
        Cell::new("Sell Slippage (bps)").with_style(prettytable::Attr::Bold),
    ]));

    let trade_labels = ["$1K", "$10K", "$50K", "$100K", "$500K", "$5M", "$10M"];
    for (i, label) in trade_labels.iter().enumerate() {
        if let Some(slip) = data.slippage.get(i) {
            let buy_text = if slip.buy_feasible {
                format!("{:.2}", slip.buy_slippage_bps.unwrap_or_default())
            } else {
                "Insufficient Liquidity".to_string()
            };
            let sell_text = if slip.sell_feasible {
                format!("{:.2}", slip.sell_slippage_bps.unwrap_or_default())
            } else {
                "Insufficient Liquidity".to_string()
            };

            slip_table.add_row(Row::new(vec![
                Cell::new(*label),
                Cell::new_align(&buy_text, format::Alignment::RIGHT),
                Cell::new_align(&sell_text, format::Alignment::RIGHT),
            ]));
        }
    }

    slip_table.add_row(Row::new(vec![
        Cell::new("Timestamp").with_style(prettytable::Attr::Bold),
        Cell::new_align(
            &stats.timestamp.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
            format::Alignment::RIGHT,
        )
        .with_hspan(2),
    ]));

    println!();
    slip_table.printstd();
    println!();

    Ok(())
}

fn display_json_combined(data: &LiquidityData) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(data)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use perps_core::LiquidityDepthStats;
    use rust_decimal::Decimal;
    use std::collections::HashMap;
    use std::str::FromStr;
    use tempfile::tempdir;

    fn create_mock_stats(exchange: &str, symbol: &str) -> LiquidityDepthStats {
        LiquidityDepthStats {
            timestamp: Utc::now(),
            exchange: exchange.to_string(),
            symbol: symbol.to_string(),
            mid_price: Decimal::from_str("50000").unwrap(),
            bid_1bps: Decimal::from_str("1000").unwrap(),
            bid_2_5bps: Decimal::from_str("2000").unwrap(),
            bid_5bps: Decimal::from_str("3000").unwrap(),
            bid_10bps: Decimal::from_str("4000").unwrap(),
            bid_20bps: Decimal::from_str("5000").unwrap(),
            ask_1bps: Decimal::from_str("1000").unwrap(),
            ask_2_5bps: Decimal::from_str("2000").unwrap(),
            ask_5bps: Decimal::from_str("3000").unwrap(),
            ask_10bps: Decimal::from_str("4000").unwrap(),
            ask_20bps: Decimal::from_str("5000").unwrap(),
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
        write_to_csv_liquidity_only(&multi_exchange_data, "ignored.csv", &output_dir)?;

        let expected_file = dir.path().join("BTC_USDT.csv");
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
        write_to_excel_liquidity_only(&multi_exchange_data, filename, &output_dir)?;

        let full_path = dir.path().join(filename);
        assert!(full_path.exists());

        // We can't easily check the sheet name here, but the implementation is changed to use the symbol name.
        // This test now primarily ensures the file is created with the simplified logic.

        dir.close()?;
        Ok(())
    }
}
