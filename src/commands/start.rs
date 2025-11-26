use anyhow::Result;
use chrono;
use clap::Args;
use futures::StreamExt as _;
use perps_aggregator::{Aggregator, IAggregator};
use perps_core::streaming::StreamEvent;
use perps_core::types::{FundingRate, Kline, Orderbook, Ticker, Trade};
use perps_database::{PostgresRepository, Repository};
use perps_exchanges::factory;
use rust_decimal::Decimal;
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::signal;
use tokio::sync::Mutex;
use tokio::time::{interval, Duration};

#[derive(Args)]
pub struct StartArgs {
    /// Exchanges to stream from (comma-separated: aster,binance,hyperliquid,bybit,kucoin,lighter,paradex). If not specified, uses all supported exchanges.
    #[arg(short, long, value_delimiter = ',')]
    pub exchanges: Option<Vec<String>>,

    /// Path to symbols file (one symbol per line or comma-separated)
    #[arg(short, long, default_value = "symbols.txt")]
    pub symbols_file: String,

    /// Database URL for storing data
    #[arg(long, env = "DATABASE_URL")]
    pub database_url: String,

    /// Batch size for database writes (number of events before writing)
    #[arg(long, default_value = "100")]
    pub batch_size: usize,

    /// Klines fetch interval in seconds
    #[arg(long, default_value = "60")]
    pub klines_interval: u64,

    /// Report generation interval in seconds (applies to both liquidity depth and ticker reports)
    #[arg(long, default_value = "30")]
    pub report_interval: u64,

    /// Klines intervals/timeframes for fetching (comma-separated, e.g., 5m,15m,1h). Defaults to 5m,15m,1h
    #[arg(long, value_delimiter = ',')]
    pub klines_timeframes: Vec<String>,

    /// Maximum number of reconnection attempts (0 = unlimited)
    #[arg(long, default_value = "0")]
    pub max_reconnect_attempts: usize,

    /// Initial reconnection delay in seconds
    #[arg(long, default_value = "5")]
    pub reconnect_delay_seconds: u64,

    /// Maximum reconnection delay in seconds (for exponential backoff)
    #[arg(long, default_value = "300")]
    pub max_reconnect_delay_seconds: u64,

    /// Enable backfilling of historical klines data on startup
    #[arg(long, default_value = "false")]
    pub enable_backfill: bool,

    /// Exclude fees from slippage calculations (force fee=None)
    #[arg(long)]
    pub exclude_fees: bool,

    /// Override taker fee with custom value (e.g., 0.0005 for 0.05%)
    #[arg(long)]
    pub override_fee: Option<f64>,
}

/// Load symbols from file (supports both line-separated and comma-separated formats)
async fn load_symbols(file_path: &str) -> Result<Vec<String>> {
    let content = tokio::fs::read_to_string(file_path).await?;

    let mut symbols = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue; // Skip empty lines and comments
        }

        // Support comma-separated symbols on a single line
        if line.contains(',') {
            symbols.extend(
                line.split(',')
                    .map(|s| s.trim().to_uppercase())
                    .filter(|s| !s.is_empty()),
            );
        } else {
            symbols.push(line.to_uppercase());
        }
    }

    Ok(symbols)
}

/// Validate symbols against exchange
async fn validate_symbols(exchange: &str, symbols: &[String]) -> Result<Vec<String>> {
    tracing::info!(
        "Validating {} symbols for exchange: {}",
        symbols.len(),
        exchange
    );

    let client = factory::get_exchange(exchange).await?;
    let mut valid_symbols = Vec::new();

    for symbol in symbols {
        // Parse symbol to exchange-specific format first
        let parsed_symbol = client.parse_symbol(symbol);
        if client.is_supported(&parsed_symbol).await? {
            valid_symbols.push(symbol.clone()); // Store the global format
        } else {
            tracing::warn!("Symbol {} not supported on exchange {}", symbol, exchange);
        }
    }

    tracing::info!(
        "Validated {}/{} symbols for exchange {}",
        valid_symbols.len(),
        symbols.len(),
        exchange
    );
    Ok(valid_symbols)
}

/// Get supported data types for a specific exchange
/// NOTE: Ticker is excluded from all exchanges because the ticker report task
/// fetches complete ticker data via REST API every 30 seconds. WebSocket tickers
/// often have incomplete 24h statistics (zero values), so we rely solely on REST API.
///
/// WARNING: This function is currently unused in the `start` command since WebSocket streaming is disabled.
/// It's kept for reference and potential future use.
#[allow(dead_code)]
fn get_supported_data_types(exchange: &str) -> Vec<perps_core::streaming::StreamDataType> {
    use perps_core::streaming::StreamDataType;

    match exchange {
        "binance" => vec![
            // Note: Ticker excluded - use ticker report task for complete data via REST API
            StreamDataType::Trade,
            StreamDataType::Orderbook,
            StreamDataType::FundingRate,
        ],
        "hyperliquid" => vec![
            // Note: Ticker excluded - use ticker report task for complete data via REST API
            StreamDataType::Trade,
            StreamDataType::Orderbook,
        ],
        "bybit" => vec![
            // Note: Ticker excluded - use ticker report task for complete data via REST API
            StreamDataType::Trade,
            StreamDataType::Orderbook,
        ],
        "kucoin" => vec![
            // Note: Ticker excluded - use ticker report task for complete data via REST API
            StreamDataType::Trade,
            // Note: Orderbook requires incremental update handling (not implemented)
            // Note: Klines added separately based on configuration
        ],
        "lighter" => vec![
            // Note: Ticker excluded - use ticker report task for complete data via REST API
            StreamDataType::Trade,
            StreamDataType::Orderbook,
        ],
        "paradex" => vec![
            // Note: Ticker excluded - use ticker report task for complete data via REST API
            StreamDataType::Trade,
            StreamDataType::Orderbook,
            StreamDataType::FundingRate,
        ],
        _ => vec![],
    }
}

/// Spawn WebSocket streaming task for a specific exchange
///
/// WARNING: This function is currently unused in the `start` command since WebSocket streaming is disabled.
/// It's kept for reference and potential future use.
#[allow(dead_code)]
async fn spawn_streaming_task(
    exchange: String,
    symbols: Vec<String>,
    batch_size: usize,
    klines_timeframe: Option<String>,
    repository: Arc<Mutex<PostgresRepository>>,
    shutdown: Arc<AtomicBool>,
    max_reconnect_attempts: usize,
    reconnect_delay_seconds: u64,
    max_reconnect_delay_seconds: u64,
) -> Result<tokio::task::JoinHandle<Result<()>>> {
    Ok(tokio::spawn(async move {
        tracing::info!(
            "Starting WebSocket streaming task for exchange: {} (auto-reconnect enabled)",
            exchange
        );

        // Parse symbols to exchange-specific format (done once, reused across reconnections)
        let rest_client = factory::get_exchange(&exchange).await?;
        let parsed_symbols: Vec<String> = symbols
            .iter()
            .map(|s| {
                let normalized = rest_client.parse_symbol(s);
                // Remove hyphens for exchanges that don't use them in WebSocket
                match exchange.as_str() {
                    "binance" | "bybit" => normalized.replace("-", ""),
                    _ => normalized,
                }
            })
            .collect();

        tracing::info!("Streaming symbols: {:?}", parsed_symbols);

        // Get exchange-specific supported data types (done once)
        use perps_core::streaming::{IPerpsStream, StreamConfig, StreamDataType};
        let mut data_types = get_supported_data_types(&exchange);

        // For KuCoin, add klines to WebSocket streaming (no REST API available)
        let kline_interval = if exchange == "kucoin" && klines_timeframe.is_some() {
            data_types.push(StreamDataType::Kline);
            klines_timeframe.clone()
        } else {
            None
        };

        // Reconnection loop
        let mut reconnect_attempt = 0;
        let mut current_delay = reconnect_delay_seconds;
        let mut total_events = 0u64;

        loop {
            // Check shutdown signal before attempting connection
            if shutdown.load(Ordering::Relaxed) {
                tracing::info!(
                    "Shutdown signal received before connection attempt for exchange {}",
                    exchange
                );
                break;
            }

            // Log connection attempt
            if reconnect_attempt == 0 {
                tracing::info!("Connecting to {} WebSocket (initial attempt)", exchange);
            } else {
                tracing::info!(
                    "Reconnecting to {} WebSocket (attempt {}/{})",
                    exchange,
                    reconnect_attempt,
                    if max_reconnect_attempts == 0 {
                        "unlimited".to_string()
                    } else {
                        max_reconnect_attempts.to_string()
                    }
                );
            }

            // Create WebSocket stream client
            let stream_client: Box<dyn IPerpsStream + Send + Sync> = match exchange.as_str() {
                "binance" => Box::new(perps_exchanges::binance::BinanceWsClient::new()),
                "hyperliquid" => Box::new(perps_exchanges::hyperliquid::HyperliquidWsClient::new()),
                "bybit" => Box::new(perps_exchanges::bybit::BybitWsClient::new()),
                "kucoin" => Box::new(perps_exchanges::kucoin::KuCoinWsClient::new()),
                "lighter" => Box::new(perps_exchanges::lighter::LighterWsClient::new()),
                "paradex" => Box::new(perps_exchanges::paradex::ParadexWsClient::new()),
                _ => anyhow::bail!("Unsupported exchange for streaming: {}", exchange),
            };

            let config = StreamConfig {
                symbols: parsed_symbols.clone(),
                data_types: data_types.clone(),
                auto_reconnect: true,
                kline_interval: kline_interval.clone(),
            };

            // Attempt to establish stream
            let mut stream = match stream_client.stream_multi(config).await {
                Ok(s) => {
                    tracing::info!("Successfully connected to {} WebSocket", exchange);
                    reconnect_attempt = 0; // Reset on successful connection
                    current_delay = reconnect_delay_seconds; // Reset backoff
                    s
                }
                Err(e) => {
                    tracing::error!("Failed to connect to {} WebSocket: {}", exchange, e);
                    reconnect_attempt += 1;

                    // Check if we should retry
                    if max_reconnect_attempts > 0 && reconnect_attempt >= max_reconnect_attempts {
                        tracing::error!(
                            "Max reconnection attempts ({}) reached for exchange {}",
                            max_reconnect_attempts,
                            exchange
                        );
                        return Err(anyhow::anyhow!(
                            "Failed to connect after {} attempts",
                            max_reconnect_attempts
                        ));
                    }

                    // Wait before retrying with exponential backoff
                    tracing::info!(
                        "Waiting {} seconds before reconnecting to {}...",
                        current_delay,
                        exchange
                    );
                    tokio::time::sleep(Duration::from_secs(current_delay)).await;

                    // Exponential backoff with max cap
                    current_delay = std::cmp::min(current_delay * 2, max_reconnect_delay_seconds);

                    continue; // Retry connection
                }
            };

            // Batch buffers (reset on reconnection)
            let mut ticker_buffer: Vec<Ticker> = Vec::new();
            let mut trade_buffer: Vec<Trade> = Vec::new();
            let mut orderbook_buffer: Vec<Orderbook> = Vec::new();
            let mut funding_rate_buffer: Vec<FundingRate> = Vec::new();
            let mut kline_buffer: Vec<Kline> = Vec::new();

            let mut event_count = 0u64;
            let mut stream_active = true;

            // Event processing loop for this connection
            while stream_active {
                // Use tokio::select! to handle both stream events and shutdown signal
                tokio::select! {
                    maybe_event = stream.next() => {
                        match maybe_event {
                    Some(Ok(event)) => {
                    event_count += 1;

                    match event {
                        StreamEvent::Ticker(ticker) => {
                            ticker_buffer.push(ticker);
                            if ticker_buffer.len() >= batch_size {
                                let repo = repository.lock().await;
                                repo.store_tickers_with_exchange(&exchange, &ticker_buffer).await?;
                                tracing::debug!("Stored {} tickers for exchange {}", ticker_buffer.len(), exchange);
                                ticker_buffer.clear();
                            }
                        }
                        StreamEvent::Trade(trade) => {
                            trade_buffer.push(trade);
                            if trade_buffer.len() >= batch_size {
                                let repo = repository.lock().await;
                                repo.store_trades_with_exchange(&exchange, &trade_buffer).await?;
                                tracing::debug!("Stored {} trades for exchange {}", trade_buffer.len(), exchange);
                                trade_buffer.clear();
                            }
                        }
                        StreamEvent::Orderbook(orderbook) => {
                            orderbook_buffer.push(orderbook);
                            if orderbook_buffer.len() >= batch_size {
                                let repo = repository.lock().await;
                                repo.store_orderbooks_with_exchange(&exchange, &orderbook_buffer).await?;
                                tracing::debug!("Stored {} orderbooks for exchange {}", orderbook_buffer.len(), exchange);
                                orderbook_buffer.clear();
                            }
                        }
                        StreamEvent::FundingRate(funding_rate) => {
                            funding_rate_buffer.push(funding_rate);
                            if funding_rate_buffer.len() >= batch_size {
                                let repo = repository.lock().await;
                                repo.store_funding_rates_with_exchange(&exchange, &funding_rate_buffer).await?;
                                tracing::debug!("Stored {} funding rates for exchange {}", funding_rate_buffer.len(), exchange);
                                funding_rate_buffer.clear();
                            }
                        }
                        StreamEvent::Kline(kline) => {
                            // Store klines from WebSocket (KuCoin only)
                            kline_buffer.push(kline);
                            if kline_buffer.len() >= batch_size {
                                let repo = repository.lock().await;
                                repo.store_klines_with_exchange(&exchange, &kline_buffer).await?;
                                tracing::debug!("Stored {} klines for exchange {}", kline_buffer.len(), exchange);
                                kline_buffer.clear();
                            }
                        }
                    }

                    if event_count % 100 == 0 {
                        tracing::info!("Exchange {}: Processed {} events (total: {})", exchange, event_count, total_events + event_count);
                    }
                }
                Some(Err(e)) => {
                    tracing::error!("Exchange {} stream error: {}", exchange, e);
                    // Flush buffers before reconnecting
                    let repo = repository.lock().await;
                    if !ticker_buffer.is_empty() {
                        if let Err(e) = repo.store_tickers_with_exchange(&exchange, &ticker_buffer).await {
                            tracing::error!("Failed to flush ticker buffer: {}", e);
                        }
                        ticker_buffer.clear();
                    }
                    if !trade_buffer.is_empty() {
                        if let Err(e) = repo.store_trades_with_exchange(&exchange, &trade_buffer).await {
                            tracing::error!("Failed to flush trade buffer: {}", e);
                        }
                        trade_buffer.clear();
                    }
                    if !orderbook_buffer.is_empty() {
                        if let Err(e) = repo.store_orderbooks_with_exchange(&exchange, &orderbook_buffer).await {
                            tracing::error!("Failed to flush orderbook buffer: {}", e);
                        }
                        orderbook_buffer.clear();
                    }
                    if !funding_rate_buffer.is_empty() {
                        if let Err(e) = repo.store_funding_rates_with_exchange(&exchange, &funding_rate_buffer).await {
                            tracing::error!("Failed to flush funding rate buffer: {}", e);
                        }
                        funding_rate_buffer.clear();
                    }
                    if !kline_buffer.is_empty() {
                        if let Err(e) = repo.store_klines_with_exchange(&exchange, &kline_buffer).await {
                            tracing::error!("Failed to flush kline buffer: {}", e);
                        }
                        kline_buffer.clear();
                    }

                    // Update total events and trigger reconnection
                    total_events += event_count;
                    reconnect_attempt += 1;
                    stream_active = false; // Exit stream loop to trigger reconnection
                }
                None => {
                    tracing::warn!("Exchange {} stream ended unexpectedly", exchange);
                    total_events += event_count;
                    reconnect_attempt += 1;
                    stream_active = false; // Exit stream loop to trigger reconnection
                }
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {
                        // Check shutdown signal periodically
                        if shutdown.load(Ordering::Relaxed) {
                            tracing::info!("Shutdown signal received for exchange {}, flushing buffers...", exchange);
                            stream_active = false; // Exit stream loop
                        }
                    }
                }
            }

            // Final flush for this connection before reconnecting
            let repo = repository.lock().await;
            if !ticker_buffer.is_empty() {
                if let Err(e) = repo
                    .store_tickers_with_exchange(&exchange, &ticker_buffer)
                    .await
                {
                    tracing::error!("Failed to flush ticker buffer: {}", e);
                }
            }
            if !trade_buffer.is_empty() {
                if let Err(e) = repo
                    .store_trades_with_exchange(&exchange, &trade_buffer)
                    .await
                {
                    tracing::error!("Failed to flush trade buffer: {}", e);
                }
            }
            if !orderbook_buffer.is_empty() {
                if let Err(e) = repo
                    .store_orderbooks_with_exchange(&exchange, &orderbook_buffer)
                    .await
                {
                    tracing::error!("Failed to flush orderbook buffer: {}", e);
                }
            }
            if !funding_rate_buffer.is_empty() {
                if let Err(e) = repo
                    .store_funding_rates_with_exchange(&exchange, &funding_rate_buffer)
                    .await
                {
                    tracing::error!("Failed to flush funding rate buffer: {}", e);
                }
            }
            if !kline_buffer.is_empty() {
                if let Err(e) = repo
                    .store_klines_with_exchange(&exchange, &kline_buffer)
                    .await
                {
                    tracing::error!("Failed to flush kline buffer: {}", e);
                }
            }

            // Check if we should exit (shutdown signal)
            if shutdown.load(Ordering::Relaxed) {
                tracing::info!(
                    "Exchange {} streaming task stopped due to shutdown signal. Total events: {}",
                    exchange,
                    total_events
                );
                break; // Exit reconnection loop
            }

            // Check reconnection limit
            if max_reconnect_attempts > 0 && reconnect_attempt >= max_reconnect_attempts {
                tracing::error!(
                    "Max reconnection attempts ({}) reached for exchange {}",
                    max_reconnect_attempts,
                    exchange
                );
                return Err(anyhow::anyhow!(
                    "Failed to maintain connection after {} attempts",
                    max_reconnect_attempts
                ));
            }

            // Wait before reconnecting with exponential backoff
            if reconnect_attempt > 0 {
                tracing::info!(
                    "Waiting {} seconds before reconnecting to {}...",
                    current_delay,
                    exchange
                );
                tokio::time::sleep(Duration::from_secs(current_delay)).await;

                // Exponential backoff with max cap
                current_delay = std::cmp::min(current_delay * 2, max_reconnect_delay_seconds);
            }

            // Loop back to reconnect
        }

        tracing::info!(
            "Exchange {} streaming task completed. Total events: {}",
            exchange,
            total_events
        );
        Ok(())
    }))
}

/// Spawn klines fetching task for a specific exchange
async fn spawn_klines_task(
    exchange: String,
    symbols: Vec<String>,
    klines_interval: u64,
    klines_timeframe: String,
    repository: Arc<Mutex<PostgresRepository>>,
    shutdown: Arc<AtomicBool>,
) -> Result<tokio::task::JoinHandle<Result<()>>> {
    Ok(tokio::spawn(async move {
        tracing::info!(
            "Starting klines fetching task for exchange: {} (interval: {}s, timeframe: {})",
            exchange,
            klines_interval,
            klines_timeframe
        );

        let client = factory::get_exchange(&exchange).await?;

        // Perform initial historical backfill for each symbol (if no data exists)
        tracing::info!(
            "Checking for existing klines data for {} symbols on {}",
            symbols.len(),
            exchange
        );
        for symbol in &symbols {
            if shutdown.load(Ordering::Relaxed) {
                tracing::info!("Shutdown signal received during initial backfill check");
                return Ok(());
            }

            let parsed_symbol = client.parse_symbol(symbol);
            let repo = repository.lock().await;

            // Check if we have any klines for this symbol/exchange/interval
            let latest_kline = repo
                .get_latest_kline(&exchange, symbol, &klines_timeframe)
                .await?;
            drop(repo); // Release lock before long operation

            match latest_kline {
                None => {
                    // No data exists - fetch from earliest available until now
                    tracing::info!(
                        "No existing klines for {}/{} ({}), fetching from earliest available data",
                        exchange,
                        symbol,
                        klines_timeframe
                    );

                    // Determine start time based on exchange capabilities
                    // Most exchanges support 1-2 years of history via REST API
                    let now = chrono::Utc::now();
                    let start_time = match exchange.as_str() {
                        "binance" => now - chrono::Duration::days(365 * 2), // 2 years
                        "bybit" => now - chrono::Duration::days(365 * 2),   // 2 years
                        "hyperliquid" => now - chrono::Duration::days(365), // 1 year (conservative)
                        "kucoin" => now - chrono::Duration::days(365),      // 1 year
                        "lighter" => now - chrono::Duration::days(180), // 6 months (conservative)
                        "paradex" => now - chrono::Duration::days(180), // 6 months (conservative)
                        _ => now - chrono::Duration::days(365),         // 1 year default
                    };

                    // Use smart chunking based on interval (from backfill logic)
                    let interval_ms = match &klines_timeframe[..] {
                        "1m" => 60_000i64,
                        "3m" => 180_000,
                        "5m" => 300_000,
                        "15m" => 900_000,
                        "30m" => 1_800_000,
                        "1h" => 3_600_000,
                        "2h" => 7_200_000,
                        "4h" => 14_400_000,
                        "8h" => 28_800_000,
                        "12h" => 43_200_000,
                        "1d" => 86_400_000,
                        "1w" => 604_800_000,
                        _ => 3_600_000, // default to 1 hour
                    };

                    // Calculate optimal chunk size based on interval
                    let chunk_size = match &klines_timeframe[..] {
                        "1m" | "3m" | "5m" => 1000,
                        "15m" | "30m" | "1h" => 1000,
                        "2h" | "4h" => 500,
                        "8h" | "12h" => 500,
                        "1d" | "1w" => 500,
                        _ => 1000,
                    };

                    tracing::info!(
                        "Fetching historical klines for {}/{} from {} to {} (chunk size: {})",
                        exchange,
                        symbol,
                        start_time,
                        now,
                        chunk_size
                    );

                    // Fetch in chunks with progress tracking
                    let mut current_start = start_time;
                    let mut total_fetched = 0;

                    loop {
                        if shutdown.load(Ordering::Relaxed) {
                            tracing::info!("Shutdown signal received during historical fetch");
                            break;
                        }

                        // Calculate chunk end time
                        let chunk_duration =
                            chrono::Duration::milliseconds(interval_ms * chunk_size as i64);
                        let chunk_end = (current_start + chunk_duration).min(now);

                        // Fetch chunk
                        match client
                            .get_klines(
                                &parsed_symbol,
                                &klines_timeframe,
                                Some(current_start),
                                Some(chunk_end),
                                Some(chunk_size as u32),
                            )
                            .await
                        {
                            Ok(klines) => {
                                if klines.is_empty() {
                                    tracing::debug!(
                                        "No more klines available for {}/{}",
                                        exchange,
                                        symbol
                                    );
                                    break;
                                }

                                let klines_count = klines.len();
                                total_fetched += klines_count;

                                // Get latest timestamp to advance correctly
                                let latest_kline_time =
                                    klines.iter().map(|k| k.open_time).max().unwrap();

                                // Store klines
                                let repo = repository.lock().await;
                                match repo.store_klines_with_exchange(&exchange, &klines).await {
                                    Ok(_) => {
                                        tracing::debug!(
                                            "Stored {} klines for {}/{} (total: {})",
                                            klines_count,
                                            exchange,
                                            symbol,
                                            total_fetched
                                        );
                                    }
                                    Err(e) => {
                                        tracing::error!(
                                            "Failed to store klines for {}/{}: {}",
                                            exchange,
                                            symbol,
                                            e
                                        );
                                        break;
                                    }
                                }
                                drop(repo);

                                // Move to next interval after latest fetched kline
                                current_start =
                                    latest_kline_time + chrono::Duration::milliseconds(interval_ms);

                                // If we've reached or passed the chunk_end, adjust
                                if current_start >= chunk_end {
                                    current_start = chunk_end;
                                }

                                // If we've reached now, we're done
                                if current_start >= now || klines_count < chunk_size {
                                    break;
                                }

                                // Note: Rate limiting is handled automatically by the RateLimiter in the exchange client
                            }
                            Err(e) => {
                                let error_msg = e.to_string();
                                if error_msg.contains("not available")
                                    || error_msg.contains("not implemented")
                                {
                                    tracing::warn!(
                                        "Klines not available for {}/{}: {}",
                                        exchange,
                                        symbol,
                                        e
                                    );
                                } else {
                                    tracing::error!(
                                        "Failed to fetch klines for {}/{}: {}",
                                        exchange,
                                        symbol,
                                        e
                                    );
                                }
                                break;
                            }
                        }
                    }

                    tracing::info!(
                        "Initial backfill complete for {}/{} ({}): {} klines fetched",
                        exchange,
                        symbol,
                        klines_timeframe,
                        total_fetched
                    );
                }
                Some(latest) => {
                    // Data exists - fetch from last kline until now to fill any gaps
                    tracing::info!(
                        "Found existing klines for {}/{} ({}), latest at {}",
                        exchange,
                        symbol,
                        klines_timeframe,
                        latest.open_time
                    );

                    let now = chrono::Utc::now();
                    let start_from = latest.open_time + chrono::Duration::milliseconds(1);

                    if start_from < now {
                        // Use smart chunking for gap filling too
                        let interval_ms = match &klines_timeframe[..] {
                            "1m" => 60_000i64,
                            "3m" => 180_000,
                            "5m" => 300_000,
                            "15m" => 900_000,
                            "30m" => 1_800_000,
                            "1h" => 3_600_000,
                            "2h" => 7_200_000,
                            "4h" => 14_400_000,
                            "8h" => 28_800_000,
                            "12h" => 43_200_000,
                            "1d" => 86_400_000,
                            "1w" => 604_800_000,
                            _ => 3_600_000,
                        };

                        let chunk_size = match &klines_timeframe[..] {
                            "1m" | "3m" | "5m" | "15m" | "30m" | "1h" => 1000,
                            "2h" | "4h" | "8h" | "12h" | "1d" | "1w" => 500,
                            _ => 1000,
                        };

                        tracing::info!(
                            "Fetching missing klines for {}/{} from {} to {} (chunk size: {})",
                            exchange,
                            symbol,
                            start_from,
                            now,
                            chunk_size
                        );

                        let mut current_start = start_from;
                        let mut total_filled = 0;

                        loop {
                            if shutdown.load(Ordering::Relaxed) {
                                tracing::info!("Shutdown signal received during gap fill");
                                break;
                            }

                            let chunk_duration =
                                chrono::Duration::milliseconds(interval_ms * chunk_size as i64);
                            let chunk_end = (current_start + chunk_duration).min(now);

                            match client
                                .get_klines(
                                    &parsed_symbol,
                                    &klines_timeframe,
                                    Some(current_start),
                                    Some(chunk_end),
                                    Some(chunk_size as u32),
                                )
                                .await
                            {
                                Ok(klines) => {
                                    if klines.is_empty() {
                                        break;
                                    }

                                    let klines_count = klines.len();
                                    total_filled += klines_count;

                                    let latest_kline_time =
                                        klines.iter().map(|k| k.open_time).max().unwrap();

                                    let repo = repository.lock().await;
                                    match repo.store_klines_with_exchange(&exchange, &klines).await
                                    {
                                        Ok(_) => {
                                            tracing::debug!(
                                                "Filled {} klines for {}/{} (total: {})",
                                                klines_count,
                                                exchange,
                                                symbol,
                                                total_filled
                                            );
                                        }
                                        Err(e) => {
                                            tracing::error!(
                                                "Failed to store klines for {}/{}: {}",
                                                exchange,
                                                symbol,
                                                e
                                            );
                                            break;
                                        }
                                    }
                                    drop(repo);

                                    // Move to next interval
                                    current_start = latest_kline_time
                                        + chrono::Duration::milliseconds(interval_ms);
                                    if current_start >= chunk_end {
                                        current_start = chunk_end;
                                    }

                                    if current_start >= now || klines_count < chunk_size {
                                        break;
                                    }

                                    // Note: Rate limiting is handled automatically by the RateLimiter
                                }
                                Err(e) => {
                                    tracing::error!(
                                        "Failed to fetch missing klines for {}/{}: {}",
                                        exchange,
                                        symbol,
                                        e
                                    );
                                    break;
                                }
                            }
                        }

                        if total_filled > 0 {
                            tracing::info!(
                                "Filled {} missing klines for {}/{}",
                                total_filled,
                                exchange,
                                symbol
                            );
                        }
                    }
                }
            }
        }

        tracing::info!(
            "Initial klines check/backfill complete for {} on {} ({})",
            symbols.len(),
            exchange,
            klines_timeframe
        );

        // Now start periodic fetching
        let mut ticker = interval(Duration::from_secs(klines_interval));

        loop {
            // Use tokio::select! to check shutdown signal while waiting for next tick
            tokio::select! {
                _ = ticker.tick() => {
                    // Continue with periodic klines fetch
                }
                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    // Check shutdown signal periodically
                    if shutdown.load(Ordering::Relaxed) {
                        tracing::info!("Shutdown signal received for klines task ({})", exchange);
                        break;
                    }
                    continue; // Wait for next tick or shutdown check
                }
            }

            tracing::debug!(
                "Fetching recent klines for exchange {} ({}, {} symbols)",
                exchange,
                klines_timeframe,
                symbols.len()
            );

            let now = chrono::Utc::now();

            for symbol in &symbols {
                // Check shutdown before processing each symbol
                if shutdown.load(Ordering::Relaxed) {
                    tracing::info!("Shutdown signal received during klines fetch");
                    break;
                }

                let parsed_symbol = client.parse_symbol(symbol);
                let repo = repository.lock().await;
                let latest_kline = repo
                    .get_latest_kline(&exchange, symbol, &klines_timeframe)
                    .await?;
                drop(repo);

                // Fetch from latest kline or last hour if no data
                let start_time = match latest_kline {
                    Some(kline) => kline.open_time + chrono::Duration::milliseconds(1),
                    None => now - chrono::Duration::hours(1), // Fallback to last hour
                };

                match client
                    .get_klines(
                        &parsed_symbol,
                        &klines_timeframe,
                        Some(start_time),
                        Some(now),
                        Some(500),
                    )
                    .await
                {
                    Ok(klines) => {
                        if !klines.is_empty() {
                            let repo = repository.lock().await;
                            match repo.store_klines_with_exchange(&exchange, &klines).await {
                                Ok(_) => {
                                    tracing::debug!(
                                        "Stored {} klines for {}/{}",
                                        klines.len(),
                                        exchange,
                                        symbol
                                    );
                                }
                                Err(e) => {
                                    tracing::error!(
                                        "Failed to store klines for {}/{}: {}",
                                        exchange,
                                        symbol,
                                        e
                                    );
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let error_msg = e.to_string();
                        if error_msg.contains("not available")
                            || error_msg.contains("not implemented")
                        {
                            tracing::warn!(
                                "Klines not available for {}/{}: {}",
                                exchange,
                                symbol,
                                e
                            );
                        } else {
                            tracing::error!(
                                "Failed to fetch klines for {}/{}: {}",
                                exchange,
                                symbol,
                                e
                            );
                        }
                    }
                }
                tracing::info!(
                    "Symbol {}/{} ({}): Completed periodic klines fetch cycle",
                    exchange,
                    symbol,
                    klines_timeframe
                );
            }
        }

        tracing::info!("Klines fetching task for {} stopped", exchange);
        Ok(())
    }))
}

/// Spawn liquidity depth report generation task
async fn spawn_liquidity_report_task(
    exchange_symbols: HashMap<String, Vec<String>>,
    report_interval: u64,
    repository: Arc<Mutex<PostgresRepository>>,
    shutdown: Arc<AtomicBool>,
    exclude_fees: bool,
    override_fee: Option<f64>,
) -> Result<tokio::task::JoinHandle<Result<()>>> {
    Ok(tokio::spawn(async move {
        tracing::info!(
            "Starting liquidity depth report generation task (interval: {}s)",
            report_interval
        );

        let mut ticker = interval(Duration::from_secs(report_interval));
        let aggregator = Aggregator::new();

        // Create REST API clients for each exchange
        let mut clients: HashMap<String, Box<dyn perps_core::IPerps + Send + Sync>> =
            HashMap::new();
        for exchange in exchange_symbols.keys() {
            match factory::get_exchange(exchange).await {
                Ok(client) => {
                    clients.insert(exchange.clone(), client);
                    tracing::debug!("Initialized REST client for exchange: {}", exchange);
                }
                Err(e) => {
                    tracing::error!("Failed to initialize REST client for {}: {}", exchange, e);
                }
            }
        }


        loop {
            // Use tokio::select! to check shutdown signal while waiting for next tick
            tokio::select! {
                _ = ticker.tick() => {
                    // Continue with report generation
                }
                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    // Check shutdown signal periodically
                    if shutdown.load(Ordering::Relaxed) {
                        tracing::info!("Shutdown signal received for report generation task");
                        break;
                    }
                    continue; // Wait for next tick or shutdown check
                }
            }

            tracing::info!(
                "Generating liquidity depth report for {} exchanges",
                exchange_symbols.len()
            );

            for (exchange, symbols) in &exchange_symbols {
                // Check shutdown before processing each exchange
                if shutdown.load(Ordering::Relaxed) {
                    tracing::info!("Shutdown signal received during liquidity report generation");
                    break;
                }

                // Get REST client for this exchange
                let client = match clients.get(exchange) {
                    Some(c) => c,
                    None => {
                        tracing::error!("No REST client available for exchange: {}", exchange);
                        continue;
                    }
                };

                for symbol in symbols {
                    // Check shutdown before processing each symbol
                    if shutdown.load(Ordering::Relaxed) {
                        tracing::info!(
                            "Shutdown signal received during liquidity report generation"
                        );
                        break;
                    }

                    // Parse symbol to exchange-specific format
                    let parsed_symbol = client.parse_symbol(symbol);

                    // Fetch orderbook once and calculate both liquidity depth and slippage
                    match client.get_orderbook(&parsed_symbol, 1000).await {
                        Ok(multi_orderbook) => {
                            // MultiResolutionOrderbook automatically selects best resolution for each calculation

                            // Calculate liquidity depth
                            match aggregator
                                .calculate_liquidity_depth(&multi_orderbook, exchange, symbol)
                                .await
                            {
                                Ok(depth_stats) => {
                                    // Store liquidity depth
                                    let repo = repository.lock().await;
                                    match repo.store_liquidity_depth(&[depth_stats]).await {
                                        Ok(_) => {
                                            tracing::debug!(
                                                "Stored liquidity depth for {}/{}",
                                                exchange,
                                                symbol
                                            );
                                        }
                                        Err(e) => {
                                            tracing::error!(
                                                "Failed to store liquidity depth for {}/{}: {}",
                                                exchange,
                                                symbol,
                                                e
                                            );
                                        }
                                    }
                                    drop(repo);
                                }
                                Err(e) => {
                                    tracing::error!(
                                        "Failed to calculate liquidity depth for {}/{}: {}",
                                        exchange,
                                        symbol,
                                        e
                                    );
                                }
                            }

                            // Calculate slippage for all trade amounts (raw, without fees)
                            let slippages = aggregator.calculate_all_slippages(&multi_orderbook);

                            // Store slippage
                            let repo = repository.lock().await;
                            match repo
                                .store_slippage_with_exchange(exchange, &slippages)
                                .await
                            {
                                Ok(_) => {
                                    tracing::debug!(
                                        "Stored {} slippage entries for {}/{}",
                                        slippages.len(),
                                        exchange,
                                        symbol
                                    );
                                }
                                Err(e) => {
                                    tracing::error!(
                                        "Failed to store slippage for {}/{}: {}",
                                        exchange,
                                        symbol,
                                        e
                                    );
                                }
                            }
                            drop(repo);

                            // Store orderbook (use finest resolution for storage)
                            if let Some(finest_book) = multi_orderbook.best_for_tight_spreads() {
                                let repo = repository.lock().await;
                                match repo
                                    .store_orderbooks_with_exchange(exchange, &[finest_book.clone()])
                                    .await
                                {
                                Ok(_) => {
                                    tracing::debug!("Stored orderbook for {}/{}", exchange, symbol);
                                }
                                Err(e) => {
                                    tracing::error!(
                                        "Failed to store orderbook for {}/{}: {}",
                                        exchange,
                                        symbol,
                                        e
                                    );
                                }
                            }
                            drop(repo);
                        }
                        }
                        Err(e) => {
                            tracing::error!(
                                "Failed to fetch orderbook for {}/{}: {}",
                                exchange,
                                symbol,
                                e
                            );
                        }
                    }
                }
            }

            tracing::info!("Completed liquidity depth report generation");
        }

        tracing::info!("Liquidity depth report generation task stopped");
        Ok(())
    }))
}

/// Spawn ticker report generation task
async fn spawn_ticker_report_task(
    exchange_symbols: HashMap<String, Vec<String>>,
    report_interval: u64,
    repository: Arc<Mutex<PostgresRepository>>,
    shutdown: Arc<AtomicBool>,
) -> Result<tokio::task::JoinHandle<Result<()>>> {
    Ok(tokio::spawn(async move {
        tracing::info!(
            "Starting ticker report generation task (interval: {}s)",
            report_interval
        );

        let mut ticker_interval = interval(Duration::from_secs(report_interval));

        // Create REST API clients for each exchange
        let mut clients: HashMap<String, Box<dyn perps_core::IPerps + Send + Sync>> =
            HashMap::new();
        for exchange in exchange_symbols.keys() {
            match factory::get_exchange(exchange).await {
                Ok(client) => {
                    clients.insert(exchange.clone(), client);
                    tracing::debug!("Initialized REST client for exchange: {}", exchange);
                }
                Err(e) => {
                    tracing::error!("Failed to initialize REST client for {}: {}", exchange, e);
                }
            }
        }

        loop {
            // Use tokio::select! to check shutdown signal while waiting for next tick
            tokio::select! {
                _ = ticker_interval.tick() => {
                    // Continue with ticker report generation
                }
                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    // Check shutdown signal periodically
                    if shutdown.load(Ordering::Relaxed) {
                        tracing::info!("Shutdown signal received for ticker report generation task");
                        break;
                    }
                    continue; // Wait for next tick or shutdown check
                }
            }

            tracing::info!(
                "Generating ticker report for {} exchanges",
                exchange_symbols.len()
            );

            // Group tickers by exchange for storage
            let mut tickers_by_exchange: HashMap<String, Vec<Ticker>> = HashMap::new();

            for (exchange, symbols) in &exchange_symbols {
                // Check shutdown before processing each exchange
                if shutdown.load(Ordering::Relaxed) {
                    tracing::info!("Shutdown signal received during ticker report generation");
                    break;
                }

                // Get REST client for this exchange
                let client = match clients.get(exchange) {
                    Some(c) => c,
                    None => {
                        tracing::error!("No REST client available for exchange: {}", exchange);
                        continue;
                    }
                };

                for symbol in symbols {
                    // Check shutdown before processing each symbol
                    if shutdown.load(Ordering::Relaxed) {
                        tracing::info!("Shutdown signal received during ticker report generation");
                        break;
                    }

                    // Parse symbol to exchange-specific format
                    let parsed_symbol = client.parse_symbol(symbol);

                    // Fetch ticker from REST API
                    match client.get_ticker(&parsed_symbol).await {
                        Ok(ticker) => {
                            if !ticker.is_empty() {
                                tickers_by_exchange
                                    .entry(exchange.clone())
                                    .or_insert_with(Vec::new)
                                    .push(ticker);
                            } else {
                                tracing::error!(
                                    "Ticker {} empty for exchange {}",
                                    symbol,
                                    exchange
                                );
                            }
                        }
                        Err(e) => {
                            tracing::error!(
                                "Failed to fetch ticker for {}/{}: {}",
                                exchange,
                                symbol,
                                e
                            );
                        }
                    }
                }
            }

            // Store all tickers in batch
            if !tickers_by_exchange.is_empty() {
                let repo = repository.lock().await;

                // Store tickers for each exchange
                for (exchange, tickers) in tickers_by_exchange {
                    match repo.store_tickers_with_exchange(&exchange, &tickers).await {
                        Ok(_) => {
                            tracing::debug!(
                                "Stored {} tickers for exchange {}",
                                tickers.len(),
                                exchange
                            );
                        }
                        Err(e) => {
                            tracing::error!(
                                "Failed to store tickers for exchange {}: {}",
                                exchange,
                                e
                            );
                        }
                    }
                }
            }

            tracing::info!("Completed ticker report generation");
        }

        tracing::info!("Ticker report generation task stopped");
        Ok(())
    }))
}

pub async fn execute(args: StartArgs) -> Result<()> {
    tracing::info!("Starting unified data collection service");

    // Determine which exchanges to use
    let supported_exchanges = vec![
        "aster",
        "binance",
        "extended",
        "hyperliquid",
        "bybit",
        "kucoin",
        "lighter",
        "pacifica",
        "paradex",
    ];
    let exchanges = match args.exchanges {
        Some(ref exs) if !exs.is_empty() => {
            // Validate provided exchanges
            for exchange in exs {
                if !supported_exchanges.contains(&exchange.as_str()) {
                    anyhow::bail!(
                        "Unsupported exchange: {}. Supported: {:?}",
                        exchange,
                        supported_exchanges
                    );
                }
            }
            exs.clone()
        }
        _ => {
            tracing::info!("No exchanges specified, using all supported exchanges");
            supported_exchanges.iter().map(|&s| s.to_string()).collect()
        }
    };

    // Apply default klines timeframes if not provided
    let klines_timeframes = if args.klines_timeframes.is_empty() {
        vec!["1h".to_string()]
    } else {
        args.klines_timeframes.clone()
    };

    tracing::info!("Exchanges: {:?}", exchanges);
    tracing::info!("Symbols file: {}", args.symbols_file);
    tracing::info!("Batch size: {}", args.batch_size);
    tracing::info!("Klines backfill enabled: {}", args.enable_backfill);
    tracing::info!(
        "Klines interval: {}s (timeframes: {:?})",
        args.klines_interval,
        klines_timeframes
    );
    tracing::info!("Report interval: {}s", args.report_interval);

    // Load symbols from file
    tracing::info!("Loading symbols from file: {}", args.symbols_file);
    let symbols = load_symbols(&args.symbols_file).await?;
    tracing::info!("Loaded {} symbols: {:?}", symbols.len(), symbols);

    if symbols.is_empty() {
        anyhow::bail!("No symbols found in file: {}", args.symbols_file);
    }

    // Validate symbols for each exchange
    let mut exchange_symbols: HashMap<String, Vec<String>> = HashMap::new();
    for exchange in &exchanges {
        let valid_symbols = validate_symbols(exchange, &symbols).await?;
        if valid_symbols.is_empty() {
            tracing::warn!("No valid symbols found for exchange: {}", exchange);
        } else {
            exchange_symbols.insert(exchange.clone(), valid_symbols);
        }
    }

    if exchange_symbols.is_empty() {
        anyhow::bail!("No valid symbols found for any exchange");
    }

    // Initialize database connection
    tracing::info!("Connecting to database: {}", args.database_url);
    let pool = PgPool::connect(&args.database_url).await?;
    let repository = Arc::new(Mutex::new(PostgresRepository::new(pool)));

    // Create shutdown signal
    let shutdown = Arc::new(AtomicBool::new(false));

    // Spawn tasks
    let mut tasks = Vec::new();

    // Note: WebSocket streaming is disabled. All data is fetched via REST API for better data quality.
    // The ticker and liquidity reports provide complete data with all 24h statistics.

    // Spawn klines fetching tasks (one per exchange per timeframe) - only if backfill is enabled
    if args.enable_backfill {
        tracing::info!(
            "Klines backfill enabled, spawning klines tasks for {} exchanges and {} timeframes",
            exchange_symbols.len(),
            klines_timeframes.len()
        );
        for (exchange, symbols) in &exchange_symbols {
            for timeframe in &klines_timeframes {
                tracing::info!(
                    "Spawning klines task for exchange: {} with timeframe: {}",
                    exchange,
                    timeframe
                );
                let task = spawn_klines_task(
                    exchange.clone(),
                    symbols.clone(),
                    args.klines_interval,
                    timeframe.clone(),
                    repository.clone(),
                    shutdown.clone(),
                )
                .await?;
                tasks.push(task);
            }
        }
    } else {
        tracing::info!("Klines backfill disabled, skipping klines tasks");
    }

    // Spawn liquidity depth report generation task (single task for all exchanges)
    let liquidity_report_task = spawn_liquidity_report_task(
        exchange_symbols.clone(),
        args.report_interval,
        repository.clone(),
        shutdown.clone(),
        args.exclude_fees,
        args.override_fee,
    )
    .await?;
    tasks.push(liquidity_report_task);

    // Spawn ticker report generation task (single task for all exchanges)
    let ticker_report_task = spawn_ticker_report_task(
        exchange_symbols.clone(),
        args.report_interval,
        repository.clone(),
        shutdown.clone(),
    )
    .await?;
    tasks.push(ticker_report_task);

    tracing::info!(
        "All tasks spawned successfully. Running {} tasks total",
        tasks.len()
    );
    tracing::info!("Press Ctrl+C to stop");

    // Setup Ctrl+C handler
    let shutdown_clone = shutdown.clone();
    tokio::spawn(async move {
        match signal::ctrl_c().await {
            Ok(()) => {
                tracing::info!("Received Ctrl+C, initiating graceful shutdown...");
                shutdown_clone.store(true, Ordering::Relaxed);
            }
            Err(e) => {
                tracing::error!("Failed to listen for Ctrl+C signal: {}", e);
            }
        }
    });

    // Wait for all tasks to complete
    let results = futures::future::join_all(tasks).await;

    // Check for errors
    let mut had_errors = false;
    for (i, result) in results.iter().enumerate() {
        match result {
            Ok(Ok(_)) => {
                tracing::info!("Task {} completed successfully", i);
            }
            Ok(Err(e)) => {
                tracing::error!("Task {} failed: {}", i, e);
                had_errors = true;
            }
            Err(e) => {
                tracing::error!("Task {} panicked: {}", i, e);
                had_errors = true;
            }
        }
    }

    tracing::info!("Unified data collection service stopped");

    if had_errors {
        anyhow::bail!("One or more tasks failed during execution");
    }

    Ok(())
}
