use anyhow::Result;
use clap::Args;
use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use perps_core::types::{FundingRate, Kline, Trade};
use perps_core::IPerps;
use perps_database::{PostgresRepository, Repository};
use perps_exchanges::factory;
use sqlx::PgPool;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::sleep;

#[derive(Args)]
pub struct BackfillArgs {
    /// Exchange to backfill from (supported: binance, hyperliquid, bybit, lighter, paradex)
    #[arg(short, long)]
    pub exchange: String,

    /// Symbols to backfill (comma-separated)
    #[arg(short, long, value_delimiter = ',')]
    pub symbols: Vec<String>,

    /// Data type to backfill (klines, trades, funding_rates)
    #[arg(short, long, default_value = "klines")]
    pub data_type: String,

    /// Klines interval/timeframe (e.g., 1m, 5m, 15m, 1h, 4h, 1d). Required when backfilling klines.
    #[arg(long)]
    pub interval: Option<String>,

    /// Start date (format: YYYY-MM-DD)
    #[arg(long)]
    pub start_date: String,

    /// End date (format: YYYY-MM-DD, defaults to today)
    #[arg(long)]
    pub end_date: Option<String>,

    /// Database URL for storing data
    #[arg(long, env = "DATABASE_URL")]
    pub database_url: String,

    /// Batch size for database writes (number of items before writing)
    #[arg(long, default_value = "1000")]
    pub batch_size: usize,

    /// Rate limit: delay between API requests in milliseconds
    #[arg(long, default_value = "200")]
    pub rate_limit_ms: u64,

    /// Skip existing data (resume capability)
    #[arg(long)]
    pub skip_existing: bool,
}

/// Parse date string (YYYY-MM-DD) to DateTime<Utc>
fn parse_date(date_str: &str) -> Result<DateTime<Utc>> {
    let naive_date = NaiveDate::parse_from_str(date_str, "%Y-%m-%d")?;
    Ok(Utc.from_utc_datetime(&naive_date.and_hms_opt(0, 0, 0).unwrap()))
}

/// Get interval duration in milliseconds for API pagination
fn get_interval_duration_ms(interval: &str) -> Result<i64> {
    let duration = match interval {
        "1m" => 60_000,
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
        _ => anyhow::bail!("Unsupported interval: {}", interval),
    };
    Ok(duration)
}

/// Calculate chunk size (how many intervals to fetch per API call)
/// Most exchanges support 500-1500 klines per request
fn get_chunk_size(interval: &str) -> usize {
    match interval {
        "1m" | "3m" | "5m" => 1000, // 1000 minutes = ~16 hours
        "15m" => 1000,               // 1000 * 15m = ~10 days
        "30m" => 1000,               // 1000 * 30m = ~20 days
        "1h" => 1000,                // 1000 hours = ~41 days
        "2h" => 500,                 // 500 * 2h = ~41 days
        "4h" => 500,                 // 500 * 4h = ~83 days
        "8h" | "12h" => 500,         // ~166 days
        "1d" => 500,                 // 500 days
        "1w" => 500,                 // 500 weeks
        _ => 500,
    }
}

pub async fn execute(args: BackfillArgs) -> Result<()> {
    tracing::info!("Starting backfill for exchange: {}", args.exchange);
    tracing::info!("Symbols: {:?}", args.symbols);
    tracing::info!("Data type: {}", args.data_type);
    tracing::info!("Date range: {} to {}", args.start_date, args.end_date.as_ref().unwrap_or(&"today".to_string()));

    // Validate exchange
    let supported_exchanges = vec!["binance", "hyperliquid", "bybit", "lighter", "paradex"];
    if !supported_exchanges.contains(&args.exchange.as_str()) {
        anyhow::bail!("Unsupported exchange: {}. Supported: {:?}", args.exchange, supported_exchanges);
    }

    // Validate data type
    match args.data_type.as_str() {
        "klines" => {
            if args.interval.is_none() {
                anyhow::bail!("--interval is required when backfilling klines. Example: --interval 1h");
            }
        }
        "trades" | "funding_rates" => {
            // No special validation for trades and funding_rates
        }
        _ => anyhow::bail!("Unsupported data type: {}. Supported: klines, trades, funding_rates", args.data_type),
    }

    // Special handling for KuCoin
    if args.exchange == "kucoin" {
        anyhow::bail!("KuCoin does not provide REST API for historical klines. Use the 'start' command with WebSocket streaming instead.");
    }

    // Parse dates
    let start_date = parse_date(&args.start_date)?;
    let end_date = if let Some(ref end_str) = args.end_date {
        parse_date(end_str)?
    } else {
        Utc::now()
    };

    if start_date >= end_date {
        anyhow::bail!("Start date must be before end date");
    }

    tracing::info!("Parsed date range: {} to {}", start_date, end_date);

    // Initialize database
    tracing::info!("Connecting to database: {}", args.database_url);
    let pool = PgPool::connect(&args.database_url).await?;
    let repository = Arc::new(Mutex::new(PostgresRepository::new(pool)));

    // Get exchange client
    let client = factory::get_exchange(&args.exchange)?;

    // Validate symbols
    tracing::info!("Validating {} symbols", args.symbols.len());
    let mut valid_symbols = Vec::new();
    for symbol in &args.symbols {
        let parsed_symbol = client.parse_symbol(symbol);
        if client.is_supported(&parsed_symbol).await? {
            valid_symbols.push(symbol.clone());
        } else {
            tracing::warn!("Symbol {} not supported on exchange {}", symbol, args.exchange);
        }
    }

    if valid_symbols.is_empty() {
        anyhow::bail!("No valid symbols found");
    }

    tracing::info!("Validated {}/{} symbols", valid_symbols.len(), args.symbols.len());

    // Backfill based on data type
    match args.data_type.as_str() {
        "klines" => {
            backfill_klines(
                &args.exchange,
                &valid_symbols,
                args.interval.as_ref().unwrap(),
                start_date,
                end_date,
                client,
                repository,
                args.batch_size,
                args.rate_limit_ms,
                args.skip_existing,
            )
            .await?;
        }
        "trades" => {
            backfill_trades(
                &args.exchange,
                &valid_symbols,
                start_date,
                end_date,
                client,
                repository,
                args.batch_size,
                args.rate_limit_ms,
                args.skip_existing,
            )
            .await?;
        }
        "funding_rates" => {
            backfill_funding_rates(
                &args.exchange,
                &valid_symbols,
                start_date,
                end_date,
                client,
                repository,
                args.batch_size,
                args.rate_limit_ms,
                args.skip_existing,
            )
            .await?;
        }
        _ => unreachable!(),
    }

    tracing::info!("Backfill completed successfully");
    Ok(())
}

async fn backfill_klines(
    exchange: &str,
    symbols: &[String],
    interval: &str,
    start_date: DateTime<Utc>,
    end_date: DateTime<Utc>,
    client: Box<dyn IPerps + Send + Sync>,
    repository: Arc<Mutex<PostgresRepository>>,
    batch_size: usize,
    rate_limit_ms: u64,
    skip_existing: bool,
) -> Result<()> {
    tracing::info!("Backfilling klines with interval: {}", interval);

    let interval_ms = get_interval_duration_ms(interval)?;
    let chunk_size = get_chunk_size(interval);

    tracing::info!("Interval duration: {}ms, chunk size: {} klines per request", interval_ms, chunk_size);

    let total_symbols = symbols.len();
    let mut total_klines_stored = 0usize;
    let mut total_api_calls = 0usize;

    for (symbol_idx, symbol) in symbols.iter().enumerate() {
        tracing::info!("[{}/{}] Processing symbol: {}", symbol_idx + 1, total_symbols, symbol);

        // Parse symbol to exchange-specific format
        let parsed_symbol = client.parse_symbol(symbol);

        // Calculate total time range
        let total_duration = end_date.signed_duration_since(start_date);
        let total_intervals = (total_duration.num_milliseconds() / interval_ms) as usize;

        tracing::info!("Symbol {}: {} intervals to backfill ({})", symbol, total_intervals, interval);

        // If skip_existing is enabled, check for existing data
        let actual_start_date = if skip_existing {
            let repo = repository.lock().await;
            match repo.get_klines(exchange, symbol, interval, start_date, end_date, Some(1)).await {
                Ok(klines) if !klines.is_empty() => {
                    let latest_kline = &klines[0];
                    tracing::info!("Found existing data up to {}, resuming from there", latest_kline.open_time);
                    latest_kline.open_time + Duration::milliseconds(interval_ms)
                }
                _ => start_date,
            }
        } else {
            start_date
        };

        if actual_start_date >= end_date {
            tracing::info!("Symbol {}: All data already exists, skipping", symbol);
            continue;
        }

        // Fetch klines in chunks
        let mut current_time = actual_start_date;
        let mut symbol_klines_count = 0usize;
        let mut buffer: Vec<Kline> = Vec::new();

        while current_time < end_date {
            // Calculate chunk end time
            let chunk_duration = Duration::milliseconds(interval_ms * chunk_size as i64);
            let chunk_end_time = (current_time + chunk_duration).min(end_date);

            // Fetch klines for this chunk
            tracing::debug!(
                "Fetching klines for {} from {} to {} (limit: {})",
                symbol,
                current_time,
                chunk_end_time,
                chunk_size
            );

            match client
                .get_klines(
                    &parsed_symbol,
                    interval,
                    Some(current_time),
                    Some(chunk_end_time),
                    Some(chunk_size as u32),
                )
                .await
            {
                Ok(klines) => {
                    total_api_calls += 1;

                    if klines.is_empty() {
                        tracing::debug!("No klines returned for {} in range {} to {}", symbol, current_time, chunk_end_time);
                        // Move to next chunk
                        current_time = chunk_end_time;
                        continue;
                    }

                    tracing::debug!("Fetched {} klines for {}", klines.len(), symbol);

                    // Add to buffer
                    buffer.extend(klines);

                    // Store when buffer is full
                    if buffer.len() >= batch_size {
                        let repo = repository.lock().await;
                        match repo.store_klines_with_exchange(exchange, &buffer).await {
                            Ok(_) => {
                                symbol_klines_count += buffer.len();
                                total_klines_stored += buffer.len();
                                tracing::info!(
                                    "Symbol {}: Stored {} klines (total: {})",
                                    symbol,
                                    buffer.len(),
                                    symbol_klines_count
                                );
                                buffer.clear();
                            }
                            Err(e) => {
                                tracing::error!("Failed to store klines for {}: {}", symbol, e);
                                return Err(e);
                            }
                        }
                    }

                    // Move to next chunk
                    current_time = chunk_end_time;

                    // Rate limiting
                    if rate_limit_ms > 0 {
                        sleep(tokio::time::Duration::from_millis(rate_limit_ms)).await;
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to fetch klines for {} at {}: {}", symbol, current_time, e);
                    // Continue to next chunk instead of failing completely
                    current_time = chunk_end_time;
                }
            }
        }

        // Flush remaining buffer
        if !buffer.is_empty() {
            let repo = repository.lock().await;
            match repo.store_klines_with_exchange(exchange, &buffer).await {
                Ok(_) => {
                    symbol_klines_count += buffer.len();
                    total_klines_stored += buffer.len();
                    tracing::info!(
                        "Symbol {}: Stored final {} klines (total: {})",
                        symbol,
                        buffer.len(),
                        symbol_klines_count
                    );
                }
                Err(e) => {
                    tracing::error!("Failed to store final klines for {}: {}", symbol, e);
                    return Err(e);
                }
            }
        }

        tracing::info!("Symbol {}: Backfill complete - {} klines stored", symbol, symbol_klines_count);
    }

    tracing::info!(
        "Backfill summary: {} symbols, {} klines stored, {} API calls",
        total_symbols,
        total_klines_stored,
        total_api_calls
    );

    Ok(())
}

async fn backfill_trades(
    exchange: &str,
    symbols: &[String],
    start_date: DateTime<Utc>,
    end_date: DateTime<Utc>,
    client: Box<dyn IPerps + Send + Sync>,
    repository: Arc<Mutex<PostgresRepository>>,
    batch_size: usize,
    rate_limit_ms: u64,
    skip_existing: bool,
) -> Result<()> {
    tracing::info!("Backfilling trades");
    tracing::warn!("⚠️  LIMITATION: Most exchanges' get_recent_trades() API only returns very recent trades (typically last few hours).");
    tracing::warn!("⚠️  For comprehensive historical trade data, use the 'stream' command to collect trades in real-time via WebSocket.");
    tracing::warn!("⚠️  This backfill function is best suited for filling small gaps in recent data, not long historical periods.");

    // Most exchanges return trades in descending order (newest first)
    // We'll fetch in 1-hour chunks and filter by timestamp
    const CHUNK_DURATION_HOURS: i64 = 1;
    const TRADES_PER_REQUEST: u32 = 1000;

    let total_symbols = symbols.len();
    let mut total_trades_stored = 0usize;
    let mut total_api_calls = 0usize;

    for (symbol_idx, symbol) in symbols.iter().enumerate() {
        tracing::info!("[{}/{}] Processing symbol: {}", symbol_idx + 1, total_symbols, symbol);

        // Parse symbol to exchange-specific format
        let parsed_symbol = client.parse_symbol(symbol);

        // Calculate total time range
        let total_duration = end_date.signed_duration_since(start_date);
        let total_hours = total_duration.num_hours();

        tracing::info!("Symbol {}: {} hours to backfill", symbol, total_hours);

        // If skip_existing is enabled, check for existing data
        let actual_start_date = if skip_existing {
            let repo = repository.lock().await;
            match repo.get_trades(exchange, symbol, start_date, end_date, Some(1)).await {
                Ok(trades) if !trades.is_empty() => {
                    let latest_trade = &trades[0];
                    tracing::info!("Found existing data up to {}, resuming from there", latest_trade.timestamp);
                    latest_trade.timestamp + Duration::seconds(1)
                }
                _ => start_date,
            }
        } else {
            start_date
        };

        if actual_start_date >= end_date {
            tracing::info!("Symbol {}: All data already exists, skipping", symbol);
            continue;
        }

        // Fetch trades in chunks
        let mut current_time = actual_start_date;
        let mut symbol_trades_count = 0usize;
        let mut buffer: Vec<Trade> = Vec::new();

        while current_time < end_date {
            // Calculate chunk end time
            let chunk_end_time = (current_time + Duration::hours(CHUNK_DURATION_HOURS)).min(end_date);

            // Fetch recent trades (note: get_recent_trades doesn't support time range)
            // We fetch the maximum and filter by timestamp
            tracing::debug!(
                "Fetching trades for {} from {} to {} (limit: {})",
                symbol,
                current_time,
                chunk_end_time,
                TRADES_PER_REQUEST
            );

            match client.get_recent_trades(&parsed_symbol, TRADES_PER_REQUEST).await {
                Ok(trades) => {
                    total_api_calls += 1;

                    let total_trades_fetched = trades.len();

                    // Filter trades by timestamp range
                    let filtered_trades: Vec<Trade> = trades
                        .into_iter()
                        .filter(|t| t.timestamp >= current_time && t.timestamp < chunk_end_time)
                        .collect();

                    if filtered_trades.is_empty() {
                        tracing::debug!("No trades returned for {} in range {} to {}", symbol, current_time, chunk_end_time);
                        // Move to next chunk
                        current_time = chunk_end_time;
                        continue;
                    }

                    tracing::debug!("Fetched {} trades for {} (filtered to {})", total_trades_fetched, symbol, filtered_trades.len());

                    // Add to buffer
                    buffer.extend(filtered_trades);

                    // Store when buffer is full
                    if buffer.len() >= batch_size {
                        let repo = repository.lock().await;
                        match repo.store_trades_with_exchange(exchange, &buffer).await {
                            Ok(_) => {
                                symbol_trades_count += buffer.len();
                                total_trades_stored += buffer.len();
                                tracing::info!(
                                    "Symbol {}: Stored {} trades (total: {})",
                                    symbol,
                                    buffer.len(),
                                    symbol_trades_count
                                );
                                buffer.clear();
                            }
                            Err(e) => {
                                tracing::error!("Failed to store trades for {}: {}", symbol, e);
                                return Err(e);
                            }
                        }
                    }

                    // Move to next chunk
                    current_time = chunk_end_time;

                    // Rate limiting
                    if rate_limit_ms > 0 {
                        sleep(tokio::time::Duration::from_millis(rate_limit_ms)).await;
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to fetch trades for {} at {}: {}", symbol, current_time, e);
                    // Continue to next chunk instead of failing completely
                    current_time = chunk_end_time;
                }
            }
        }

        // Flush remaining buffer
        if !buffer.is_empty() {
            let repo = repository.lock().await;
            match repo.store_trades_with_exchange(exchange, &buffer).await {
                Ok(_) => {
                    symbol_trades_count += buffer.len();
                    total_trades_stored += buffer.len();
                    tracing::info!(
                        "Symbol {}: Stored final {} trades (total: {})",
                        symbol,
                        buffer.len(),
                        symbol_trades_count
                    );
                }
                Err(e) => {
                    tracing::error!("Failed to store final trades for {}: {}", symbol, e);
                    return Err(e);
                }
            }
        }

        tracing::info!("Symbol {}: Backfill complete - {} trades stored", symbol, symbol_trades_count);
    }

    tracing::info!(
        "Backfill summary: {} symbols, {} trades stored, {} API calls",
        total_symbols,
        total_trades_stored,
        total_api_calls
    );

    Ok(())
}

async fn backfill_funding_rates(
    exchange: &str,
    symbols: &[String],
    start_date: DateTime<Utc>,
    end_date: DateTime<Utc>,
    client: Box<dyn IPerps + Send + Sync>,
    repository: Arc<Mutex<PostgresRepository>>,
    batch_size: usize,
    rate_limit_ms: u64,
    skip_existing: bool,
) -> Result<()> {
    tracing::info!("Backfilling funding rates");

    // Funding rates are typically updated every 8 hours
    // Fetch in 30-day chunks with reasonable limit
    const CHUNK_DURATION_DAYS: i64 = 30;
    const RATES_PER_REQUEST: u32 = 500;

    let total_symbols = symbols.len();
    let mut total_rates_stored = 0usize;
    let mut total_api_calls = 0usize;

    for (symbol_idx, symbol) in symbols.iter().enumerate() {
        tracing::info!("[{}/{}] Processing symbol: {}", symbol_idx + 1, total_symbols, symbol);

        // Parse symbol to exchange-specific format
        let parsed_symbol = client.parse_symbol(symbol);

        // Calculate total time range
        let total_duration = end_date.signed_duration_since(start_date);
        let total_days = total_duration.num_days();

        tracing::info!("Symbol {}: {} days to backfill", symbol, total_days);

        // If skip_existing is enabled, check for existing data
        let actual_start_date = if skip_existing {
            let repo = repository.lock().await;
            match repo.get_funding_rates(exchange, symbol, start_date, end_date, Some(1)).await {
                Ok(rates) if !rates.is_empty() => {
                    let latest_rate = &rates[0];
                    tracing::info!("Found existing data up to {}, resuming from there", latest_rate.funding_time);
                    latest_rate.funding_time + Duration::hours(8)
                }
                _ => start_date,
            }
        } else {
            start_date
        };

        if actual_start_date >= end_date {
            tracing::info!("Symbol {}: All data already exists, skipping", symbol);
            continue;
        }

        // Fetch funding rates in chunks
        let mut current_time = actual_start_date;
        let mut symbol_rates_count = 0usize;
        let mut buffer: Vec<FundingRate> = Vec::new();

        while current_time < end_date {
            // Calculate chunk end time
            let chunk_end_time = (current_time + Duration::days(CHUNK_DURATION_DAYS)).min(end_date);

            // Fetch funding rate history
            tracing::debug!(
                "Fetching funding rates for {} from {} to {} (limit: {})",
                symbol,
                current_time,
                chunk_end_time,
                RATES_PER_REQUEST
            );

            match client
                .get_funding_rate_history(
                    &parsed_symbol,
                    Some(current_time),
                    Some(chunk_end_time),
                    Some(RATES_PER_REQUEST),
                )
                .await
            {
                Ok(rates) => {
                    total_api_calls += 1;

                    if rates.is_empty() {
                        tracing::debug!("No funding rates returned for {} in range {} to {}", symbol, current_time, chunk_end_time);
                        // Move to next chunk
                        current_time = chunk_end_time;
                        continue;
                    }

                    tracing::debug!("Fetched {} funding rates for {}", rates.len(), symbol);

                    // Add to buffer
                    buffer.extend(rates);

                    // Store when buffer is full
                    if buffer.len() >= batch_size {
                        let repo = repository.lock().await;
                        match repo.store_funding_rates_with_exchange(exchange, &buffer).await {
                            Ok(_) => {
                                symbol_rates_count += buffer.len();
                                total_rates_stored += buffer.len();
                                tracing::info!(
                                    "Symbol {}: Stored {} funding rates (total: {})",
                                    symbol,
                                    buffer.len(),
                                    symbol_rates_count
                                );
                                buffer.clear();
                            }
                            Err(e) => {
                                tracing::error!("Failed to store funding rates for {}: {}", symbol, e);
                                return Err(e);
                            }
                        }
                    }

                    // Move to next chunk
                    current_time = chunk_end_time;

                    // Rate limiting
                    if rate_limit_ms > 0 {
                        sleep(tokio::time::Duration::from_millis(rate_limit_ms)).await;
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to fetch funding rates for {} at {}: {}", symbol, current_time, e);
                    // Continue to next chunk instead of failing completely
                    current_time = chunk_end_time;
                }
            }
        }

        // Flush remaining buffer
        if !buffer.is_empty() {
            let repo = repository.lock().await;
            match repo.store_funding_rates_with_exchange(exchange, &buffer).await {
                Ok(_) => {
                    symbol_rates_count += buffer.len();
                    total_rates_stored += buffer.len();
                    tracing::info!(
                        "Symbol {}: Stored final {} funding rates (total: {})",
                        symbol,
                        buffer.len(),
                        symbol_rates_count
                    );
                }
                Err(e) => {
                    tracing::error!("Failed to store final funding rates for {}: {}", symbol, e);
                    return Err(e);
                }
            }
        }

        tracing::info!("Symbol {}: Backfill complete - {} funding rates stored", symbol, symbol_rates_count);
    }

    tracing::info!(
        "Backfill summary: {} symbols, {} funding rates stored, {} API calls",
        total_symbols,
        total_rates_stored,
        total_api_calls
    );

    Ok(())
}
