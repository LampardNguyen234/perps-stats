use anyhow::Result;
use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use clap::Args;
use perps_core::types::{FundingRate, Kline, Trade};
use perps_core::IPerps;
use perps_database::{PostgresRepository, Repository};
use perps_exchanges::factory;
use sqlx::PgPool;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::sleep;

#[derive(Args)]
pub struct BackfillArgs {
    /// Exchanges to backfill from (comma-separated). Defaults to all: aster,binance,bybit,extended,gravity,hyperliquid,kucoin,lighter,nado,pacifica,paradex
    #[arg(short, long, value_delimiter = ',')]
    pub exchanges: Vec<String>,

    /// Symbols to backfill (comma-separated)
    #[arg(short, long, value_delimiter = ',')]
    pub symbols: Vec<String>,

    /// Data types to backfill (comma-separated). Defaults to: klines,funding_rates
    #[arg(short, long, value_delimiter = ',')]
    pub data_types: Vec<String>,

    /// Klines intervals/timeframes (comma-separated, e.g., 5m,15m,1h). Defaults to: 5m,15m,30m,1h,1d
    #[arg(long, value_delimiter = ',')]
    pub intervals: Vec<String>,

    /// Start date (format: YYYY-MM-DD). If not provided, automatically discovers the earliest available timestamp from the exchange.
    #[arg(long)]
    pub start_date: Option<String>,

    /// End date (format: YYYY-MM-DD, defaults to today)
    #[arg(long)]
    pub end_date: Option<String>,

    /// Database URL for storing data
    #[arg(long, env = "DATABASE_URL")]
    pub database_url: String,

    /// Batch size for database writes (number of items before writing)
    #[arg(long, default_value = "1000")]
    pub batch_size: usize,

    /// Force refetch all data (ignores existing data in database)
    #[arg(long)]
    pub force_refetch: bool,
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
        "15m" => 1000,              // 1000 * 15m = ~10 days
        "30m" => 1000,              // 1000 * 30m = ~20 days
        "1h" => 1000,               // 1000 hours = ~41 days
        "2h" => 500,                // 500 * 2h = ~41 days
        "4h" => 500,                // 500 * 4h = ~83 days
        "8h" | "12h" => 500,        // ~166 days
        "1d" => 500,                // 500 days
        "1w" => 500,                // 500 weeks
        _ => 500,
    }
}

/// Get interval priority for optimization (higher = longer interval)
/// Used to determine which interval to discover first
fn get_interval_priority(interval: &str) -> u32 {
    match interval {
        "1m" => 1,
        "3m" => 3,
        "5m" => 5,
        "15m" => 15,
        "30m" => 30,
        "1h" => 60,
        "2h" => 120,
        "4h" => 240,
        "8h" => 480,
        "12h" => 720,
        "1d" => 1440,
        "1w" => 10080,
        _ => 0,
    }
}

/// Find the longest interval from a list of intervals
/// This is used for optimization: discover once for the longest interval,
/// then reuse the timestamp for shorter intervals
fn find_longest_interval(intervals: &[String]) -> Option<String> {
    intervals
        .iter()
        .max_by_key(|interval| get_interval_priority(interval))
        .cloned()
}

/// Get or discover the earliest timestamp for a symbol
/// For trades/funding_rates, tries to use cached klines discovery first
/// If no cache exists, discovers using klines endpoint
async fn get_or_discover_earliest_timestamp(
    client: &Arc<Box<dyn IPerps + Send + Sync>>,
    repository: &Arc<Mutex<PostgresRepository>>,
    exchange: &str,
    symbol: &str,
) -> Result<DateTime<Utc>> {
    // Try to find cached discovery for any klines interval for this symbol
    // We'll try common intervals in order of preference: 1d, 1h, 15m, 5m, 1m
    let preferred_intervals = vec!["1d", "1h", "15m", "5m", "1m"];

    for interval in &preferred_intervals {
        let repo = repository.lock().await;
        match repo.get_discovery_cache(exchange, symbol, interval).await {
            Ok(Some((earliest_timestamp, _, _))) => {
                tracing::info!(
                    "Using cached klines discovery for {}/{}: earliest={} (from interval {})",
                    exchange,
                    symbol,
                    earliest_timestamp,
                    interval
                );
                return Ok(earliest_timestamp);
            }
            Ok(None) => continue,
            Err(e) => {
                tracing::debug!(
                    "Failed to query cache for {}/{}/{}: {}",
                    exchange,
                    symbol,
                    interval,
                    e
                );
                continue;
            }
        }
    }

    // No cache found, discover using klines with 1d interval
    tracing::info!(
        "No cached discovery found for {}/{}. Discovering using klines (1d interval)...",
        exchange,
        symbol
    );

    discover_earliest_kline(client, repository, exchange, symbol, "1d").await
}

/// Discover the earliest available kline timestamp for a symbol on an exchange
/// Uses backward expansion search to efficiently find the first available data point
/// Results are cached in the database to avoid redundant discoveries
async fn discover_earliest_kline(
    client: &Arc<Box<dyn IPerps + Send + Sync>>,
    repository: &Arc<Mutex<PostgresRepository>>,
    exchange: &str,
    symbol: &str,
    interval: &str,
) -> Result<DateTime<Utc>> {
    let parsed_symbol = client.parse_symbol(symbol);

    tracing::info!(
        "Discovering earliest available kline for {} ({})",
        symbol,
        interval
    );

    // Check cache first
    {
        let repo = repository.lock().await;
        match repo.get_discovery_cache(exchange, symbol, interval).await {
            Ok(Some((earliest_timestamp, cached_api_calls, cached_duration_ms))) => {
                tracing::info!(
                    "Cache hit for {}/{}/{}: earliest={} (originally took {} API calls, {}ms)",
                    exchange,
                    symbol,
                    interval,
                    earliest_timestamp,
                    cached_api_calls,
                    cached_duration_ms
                );
                return Ok(earliest_timestamp);
            }
            Ok(None) => {
                tracing::debug!(
                    "Cache miss for {}/{}/{}, performing discovery",
                    exchange,
                    symbol,
                    interval
                );
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to query discovery cache: {}, proceeding with discovery",
                    e
                );
            }
        }
    }

    // Track discovery statistics
    let discovery_start = std::time::Instant::now();
    let mut api_calls = 0;

    // First, verify that recent data exists (sanity check)
    tracing::debug!("Checking for recent data availability...");
    let end_time = Utc::now();
    let recent_test = end_time - Duration::days(1);
    match client
        .get_klines(
            &parsed_symbol,
            interval,
            Some(recent_test),
            Some(end_time),
            Some(10),
        )
        .await
    {
        Ok(klines) if !klines.is_empty() => {
            let _ = api_calls + 1;
            tracing::debug!("Recent data available, proceeding with historical search");
        }
        Ok(_) => {
            let _ = api_calls + 1;
            anyhow::bail!(
                "No recent data available for symbol {} ({}). Symbol may not be actively traded.",
                symbol,
                interval
            );
        }
        Err(e) => {
            let _ = api_calls + 1;
            anyhow::bail!(
                "Failed to fetch recent klines for {} ({}): {}",
                symbol,
                interval,
                e
            );
        }
    }

    // Strategy: Search backwards in exponentially increasing time windows
    // Start with 30 days ago, then try 90, 180, 365, 730 days, etc.
    // For each window, fetch a small batch and check if data exists
    // Keep expanding until we find the absolute earliest data

    let search_windows = vec![
        30,   // 1 month
        90,   // 3 months
        180,  // 6 months
        365,  // 1 year
        730,  // 2 years
        1095, // 3 years
        1460, // 4 years
        1825, // 5 years
    ];

    let mut earliest_found: Option<DateTime<Utc>> = None;

    for &days_back in &search_windows {
        let test_time = end_time - Duration::days(days_back);

        tracing::debug!("Testing {} days back ({})", days_back, test_time);

        // Try to fetch klines from this point
        match client
            .get_klines(
                &parsed_symbol,
                interval,
                Some(test_time),
                Some(test_time + Duration::days(7)),
                Some(100),
            )
            .await
        {
            Ok(klines) if !klines.is_empty() => {
                api_calls += 1;
                let earliest_in_batch = klines.iter().map(|k| k.open_time).min().unwrap();

                tracing::debug!(
                    "Found data at {} days back, earliest kline: {}",
                    days_back,
                    earliest_in_batch
                );

                // Update earliest_found if this is older than what we've found
                match earliest_found {
                    None => earliest_found = Some(earliest_in_batch),
                    Some(current_earliest) if earliest_in_batch < current_earliest => {
                        earliest_found = Some(earliest_in_batch);
                    }
                    _ => {}
                }

                // Continue searching further back
            }
            Ok(_) => {
                api_calls += 1;
                tracing::debug!("No data found at {} days back", days_back);

                // If we have found data before and now there's none, we've gone too far back
                // The earliest_found is our answer
                if earliest_found.is_some() {
                    tracing::debug!(
                        "Reached limit of available data, earliest found: {:?}",
                        earliest_found
                    );
                    break;
                }
            }
            Err(e) => {
                api_calls += 1;
                tracing::debug!("Error fetching data at {} days back: {}", days_back, e);

                // If we have found data before and now there's an error, we've likely gone too far back
                if earliest_found.is_some() {
                    tracing::debug!(
                        "Error indicates limit reached, earliest found: {:?}",
                        earliest_found
                    );
                    break;
                }
            }
        }

        // Add a small delay to avoid rate limiting during discovery
        sleep(tokio::time::Duration::from_millis(100)).await;
    }

    // Use the earliest timestamp found, or default to 2 years ago if nothing was found
    let found_start = earliest_found.unwrap_or_else(|| {
        tracing::warn!("Could not determine earliest available data, defaulting to 2 years ago");
        end_time - Duration::days(730)
    });

    let duration_ms = discovery_start.elapsed().as_millis() as i32;
    tracing::info!(
        "Discovered earliest kline timestamp: {} ({} API calls, {}ms)",
        found_start,
        api_calls,
        duration_ms
    );

    // Store in cache
    let repo = repository.lock().await;
    if let Err(e) = repo
        .store_discovery_cache(
            exchange,
            symbol,
            interval,
            found_start,
            api_calls,
            duration_ms,
        )
        .await
    {
        tracing::warn!("Failed to cache discovery result: {}", e);
    }

    Ok(found_start)
}

pub async fn execute(args: BackfillArgs) -> Result<()> {
    // Apply defaults
    let exchanges_to_process = if args.exchanges.is_empty() {
        vec![
            "aster".to_string(),
            "binance".to_string(),
            "bybit".to_string(),
            "extended".to_string(),
            "hyperliquid".to_string(),
            "kucoin".to_string(),
            "lighter".to_string(),
            "pacifica".to_string(),
            "paradex".to_string(),
        ]
    } else {
        args.exchanges.clone()
    };

    let data_types_to_process = if args.data_types.is_empty() {
        vec!["klines".to_string()]
    } else {
        args.data_types.clone()
    };

    tracing::info!("Starting backfill");
    tracing::info!("Exchanges: {:?}", exchanges_to_process);
    tracing::info!("Data types: {:?}", data_types_to_process);
    tracing::info!("Symbols: {:?}", args.symbols);

    // Validate exchanges
    let supported_exchanges = vec![
        "aster",
        "binance",
        "extended",
        "gravity",
        "hyperliquid",
        "bybit",
        "kucoin",
        "lighter",
        "nado",
        "pacifica",
        "paradex",
    ];
    for exchange in &exchanges_to_process {
        if !supported_exchanges.contains(&exchange.as_str()) {
            anyhow::bail!(
                "Unsupported exchange: {}. Supported: {:?}",
                exchange,
                supported_exchanges
            );
        }
    }

    // Validate data types
    let supported_data_types = vec!["klines", "trades", "funding_rates"];
    for data_type in &data_types_to_process {
        if !supported_data_types.contains(&data_type.as_str()) {
            anyhow::bail!(
                "Unsupported data type: {}. Supported: {:?}",
                data_type,
                supported_data_types
            );
        }
    }

    // Check if klines is in the data types, if so, intervals must be specified or use defaults
    let needs_intervals = data_types_to_process.iter().any(|dt| dt == "klines");
    let intervals_to_process = if needs_intervals {
        if args.intervals.is_empty() {
            vec![
                "5m".to_string(),
                "15m".to_string(),
                "30m".to_string(),
                "1h".to_string(),
                "1d".to_string(),
            ]
        } else {
            args.intervals.clone()
        }
    } else {
        vec![] // Not used for trades or funding_rates
    };

    if needs_intervals {
        tracing::info!("Klines intervals: {:?}", intervals_to_process);
    }

    // Initialize database once for all exchanges
    tracing::info!("Connecting to database: {}", args.database_url);
    let pool = PgPool::connect(&args.database_url).await?;
    let repository = Arc::new(Mutex::new(PostgresRepository::new(pool)));

    // Determine start_date: either parse from args or discover automatically
    let (start_date, auto_discover) = if let Some(ref start_str) = args.start_date {
        (parse_date(start_str)?, false)
    } else {
        // Auto-discovery will be handled per exchange/symbol
        tracing::info!("No start-date provided, will auto-discover earliest available timestamp for each exchange/symbol");
        (Utc::now() - Duration::days(365 * 2), true) // Placeholder, will be overridden
    };

    let end_date = if let Some(ref end_str) = args.end_date {
        parse_date(end_str)?
    } else {
        Utc::now()
    };

    // Only validate date range if start_date was provided
    if args.start_date.is_some() && start_date >= end_date {
        anyhow::bail!("Start date must be before end date");
    }

    if args.start_date.is_some() {
        tracing::info!("Date range: {} to {}", start_date, end_date);
    } else {
        tracing::info!("Date range: auto-discover to {}", end_date);
    }

    // Spawn concurrent tasks for each exchange
    tracing::info!(
        "Spawning {} concurrent tasks for exchanges",
        exchanges_to_process.len()
    );

    let mut tasks = Vec::new();

    for (exchange_idx, exchange) in exchanges_to_process.iter().enumerate() {
        // Clone all necessary data for the task
        let exchange = exchange.clone();
        let symbols = args.symbols.clone();
        let data_types = data_types_to_process.clone();
        let intervals = intervals_to_process.clone();
        let repository = repository.clone();
        let batch_size = args.batch_size;
        let force_refetch = args.force_refetch;
        let total_exchanges = exchanges_to_process.len();

        // Spawn task for this exchange
        let task = tokio::spawn(async move {
            tracing::info!(
                "=============== Processing exchange [{}/{}]: {} ===============",
                exchange_idx + 1,
                total_exchanges,
                exchange
            );

            // Get exchange client
            let client = match factory::get_exchange(&exchange).await {
                Ok(c) => Arc::new(c),
                Err(e) => {
                    tracing::error!("Failed to create client for {}: {}", exchange, e);
                    return Err(e);
                }
            };

            // Validate symbols for this exchange
            tracing::info!("Validating {} symbols for {}", symbols.len(), exchange);
            let mut valid_symbols = Vec::new();
            for symbol in &symbols {
                let parsed_symbol = client.parse_symbol(symbol);
                match client.is_supported(&parsed_symbol).await {
                    Ok(true) => valid_symbols.push(symbol.clone()),
                    Ok(false) => {
                        tracing::warn!("Symbol {} not supported on exchange {}", symbol, exchange);
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to validate symbol {} on {}: {}",
                            symbol,
                            exchange,
                            e
                        );
                    }
                }
            }

            if valid_symbols.is_empty() {
                tracing::warn!("No valid symbols found for exchange {}, skipping", exchange);
                return Ok(());
            }

            tracing::info!(
                "Validated {}/{} symbols for {}: {:?}",
                valid_symbols.len(),
                symbols.len(),
                exchange,
                valid_symbols
            );

            // Loop through all data types for this exchange
            for (data_type_idx, data_type) in data_types.iter().enumerate() {
                tracing::info!(
                    "--- Processing data type [{}/{}]: {} for {} ---",
                    data_type_idx + 1,
                    data_types.len(),
                    data_type,
                    exchange
                );

                // Route to appropriate backfill function based on data type
                let result = match data_type.as_str() {
                    "klines" => {
                        // Multi-interval optimization: if auto_discover is enabled and multiple intervals are requested,
                        // discover once for the longest interval, then reuse the timestamp for shorter intervals
                        if auto_discover && intervals.len() > 1 {
                            let longest_interval = find_longest_interval(&intervals)
                                .expect("intervals is non-empty, should have a longest interval");

                            tracing::info!(
                                "Multi-interval optimization: Discovering earliest timestamp for longest interval: {}",
                                longest_interval
                            );
                            tracing::info!(
                                "Once discovered, the timestamp will be reused for all shorter intervals: {:?}",
                                intervals.iter().filter(|i| *i != &longest_interval).collect::<Vec<_>>()
                            );

                            // Discover once per symbol for the longest interval
                            // This will populate the cache for the longest interval
                            for (symbol_idx, symbol) in valid_symbols.iter().enumerate() {
                                tracing::info!(
                                    "Symbol [{}/{}]: Discovering earliest timestamp for interval {}",
                                    symbol_idx + 1,
                                    valid_symbols.len(),
                                    longest_interval
                                );

                                match discover_earliest_kline(
                                    &client,
                                    &repository,
                                    &exchange,
                                    symbol,
                                    &longest_interval,
                                )
                                .await
                                {
                                    Ok(discovered_start) => {
                                        tracing::info!(
                                            "Symbol {}: Earliest timestamp for {} is {}",
                                            symbol,
                                            longest_interval,
                                            discovered_start
                                        );

                                        // Store this timestamp for all shorter intervals as well
                                        // This avoids redundant API calls for each interval
                                        for interval in &intervals {
                                            if interval != &longest_interval {
                                                // Store in cache for this shorter interval
                                                let repo = repository.lock().await;
                                                if let Err(e) = repo
                                                    .store_discovery_cache(
                                                        &exchange,
                                                        symbol,
                                                        interval,
                                                        discovered_start,
                                                        0, // API calls = 0 (extrapolated from longest interval)
                                                        0, // duration_ms = 0 (extrapolated from longest interval)
                                                    )
                                                    .await
                                                {
                                                    tracing::warn!(
                                                        "Failed to cache extrapolated discovery for {}/{}: {}",
                                                        symbol,
                                                        interval,
                                                        e
                                                    );
                                                } else {
                                                    tracing::debug!(
                                                        "Cached extrapolated discovery for {}/{}: {} (from {})",
                                                        symbol,
                                                        interval,
                                                        discovered_start,
                                                        longest_interval
                                                    );
                                                }
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        tracing::error!(
                                            "Symbol {}: Failed to discover earliest timestamp for {}: {}",
                                            symbol,
                                            longest_interval,
                                            e
                                        );
                                        tracing::warn!(
                                            "Symbol {}: Skipping auto-discovery for this symbol. It will be skipped during backfill.",
                                            symbol
                                        );
                                    }
                                }
                            }

                            tracing::info!(
                                "Discovery complete for all {} symbols. API calls saved by optimization: {}",
                                valid_symbols.len(),
                                valid_symbols.len() * (intervals.len() - 1)
                            );
                        }

                        // Loop through all intervals and backfill
                        for (interval_idx, interval) in intervals.iter().enumerate() {
                            tracing::info!(
                                "Processing interval [{}/{}]: {}",
                                interval_idx + 1,
                                intervals.len(),
                                interval
                            );

                            backfill_klines(
                                &exchange,
                                &valid_symbols,
                                interval,
                                start_date,
                                end_date,
                                client.clone(),
                                repository.clone(),
                                batch_size,
                                !force_refetch, // skip_existing is now the default (opposite of force_refetch)
                                auto_discover,
                            )
                            .await?;

                            tracing::info!(
                                "Completed interval {} [{}/{}]",
                                interval,
                                interval_idx + 1,
                                intervals.len()
                            );
                        }
                        Ok(())
                    }
                    "trades" => {
                        backfill_trades(
                            &exchange,
                            &valid_symbols,
                            start_date,
                            end_date,
                            client.clone(),
                            repository.clone(),
                            batch_size,
                            !force_refetch, // skip_existing is now the default (opposite of force_refetch)
                            auto_discover,
                        )
                        .await
                    }
                    "funding_rates" => {
                        backfill_funding_rates(
                            &exchange,
                            &valid_symbols,
                            start_date,
                            end_date,
                            client.clone(),
                            repository.clone(),
                            batch_size,
                            !force_refetch, // skip_existing is now the default (opposite of force_refetch)
                            auto_discover,
                        )
                        .await
                    }
                    _ => unreachable!(),
                };

                if let Err(e) = result {
                    tracing::error!("Failed to backfill {} for {}: {}", data_type, exchange, e);
                    // Continue to next data type instead of failing the entire exchange task
                    continue;
                }

                tracing::info!(
                    "Completed data type {} [{}/{}] for {}",
                    data_type,
                    data_type_idx + 1,
                    data_types.len(),
                    exchange
                );
            }

            tracing::info!(
                "=============== Completed exchange {} [{}/{}] ===============",
                exchange,
                exchange_idx + 1,
                total_exchanges
            );

            Ok(())
        });

        tasks.push(task);
    }

    // Wait for all tasks to complete
    tracing::info!(
        "Waiting for all {} exchange tasks to complete...",
        tasks.len()
    );
    let results = futures::future::join_all(tasks).await;

    // Check results and collect errors
    let mut successful = 0;
    let mut failed = 0;
    for (idx, result) in results.iter().enumerate() {
        match result {
            Ok(Ok(())) => {
                successful += 1;
            }
            Ok(Err(e)) => {
                failed += 1;
                tracing::error!("Exchange task {} failed: {}", idx, e);
            }
            Err(e) => {
                failed += 1;
                tracing::error!("Exchange task {} panicked: {}", idx, e);
            }
        }
    }

    tracing::info!(
        "Backfill completed: {} exchanges successful, {} failed",
        successful,
        failed
    );

    if failed > 0 {
        anyhow::bail!("{} exchange(s) failed during backfill", failed);
    }

    Ok(())
}

async fn backfill_klines(
    exchange: &str,
    symbols: &[String],
    interval: &str,
    start_date: DateTime<Utc>,
    end_date: DateTime<Utc>,
    client: Arc<Box<dyn IPerps + Send + Sync>>,
    repository: Arc<Mutex<PostgresRepository>>,
    batch_size: usize,
    skip_existing: bool,
    auto_discover: bool,
) -> Result<()> {
    tracing::info!("Backfilling klines with interval: {}", interval);

    let interval_ms = get_interval_duration_ms(interval)?;
    let chunk_size = get_chunk_size(interval);

    tracing::info!(
        "Interval duration: {}ms, chunk size: {} klines per request",
        interval_ms,
        chunk_size
    );

    let total_symbols = symbols.len();
    let mut total_klines_stored = 0usize;
    let mut total_api_calls = 0usize;

    for (symbol_idx, symbol) in symbols.iter().enumerate() {
        tracing::info!(
            "[{}/{}] Processing symbol: {}",
            symbol_idx + 1,
            total_symbols,
            symbol
        );

        // Parse symbol to exchange-specific format
        let parsed_symbol = client.parse_symbol(symbol);

        // Discover earliest available timestamp if auto_discover is enabled
        let actual_start_date = if auto_discover {
            match discover_earliest_kline(&client, &repository, exchange, symbol, interval).await {
                Ok(discovered_start) => {
                    tracing::info!(
                        "Symbol {}/{}: Using earliest kline timestamp: {}",
                        exchange,
                        symbol,
                        discovered_start
                    );
                    discovered_start
                }
                Err(e) => {
                    tracing::error!(
                        "Symbol {}/{}: Failed to discover earliest kline: {}. Skipping this symbol.",
                        exchange,
                        symbol,
                        e
                    );
                    continue;
                }
            }
        } else {
            start_date
        };

        // Calculate total time range
        let total_duration = end_date.signed_duration_since(actual_start_date);
        let total_intervals = (total_duration.num_milliseconds() / interval_ms) as usize;

        tracing::info!(
            "Symbol {}/{}: {} intervals to backfill ({}) from {} to {}",
            exchange,
            symbol,
            total_intervals,
            interval,
            actual_start_date,
            end_date
        );

        // Query all existing klines for gap detection
        let existing_timestamps: HashSet<DateTime<Utc>> = if skip_existing {
            tracing::info!("Loading existing klines from database for gap detection...");
            let repo = repository.lock().await;
            match repo
                .get_klines(
                    exchange,
                    symbol,
                    interval,
                    actual_start_date,
                    end_date,
                    None,
                )
                .await
            {
                Ok(klines) => {
                    let _count = klines.len();
                    let timestamps: HashSet<DateTime<Utc>> =
                        klines.iter().map(|k| k.open_time).collect();
                    timestamps
                }
                Err(e) => {
                    tracing::warn!("Failed to load existing klines: {}, will fetch all data", e);
                    HashSet::new()
                }
            }
        } else {
            HashSet::new()
        };

        // Generate expected timestamps for the entire range
        let mut expected_timestamps = Vec::new();
        let mut current_ts = actual_start_date;
        while current_ts < end_date {
            expected_timestamps.push(current_ts);
            current_ts += Duration::milliseconds(interval_ms);
        }

        // Identify missing timestamps (gaps)
        let missing_timestamps: Vec<DateTime<Utc>> =
            if skip_existing && !existing_timestamps.is_empty() {
                expected_timestamps
                    .into_iter()
                    .filter(|ts| !existing_timestamps.contains(ts))
                    .collect()
            } else {
                expected_timestamps
            };

        if missing_timestamps.is_empty() {
            tracing::info!(
                "Symbol {}/{}: All data already exists, skipping",
                exchange,
                symbol
            );
            continue;
        }

        tracing::info!(
            "Symbol {}/{}: {} missing intervals to fetch",
            exchange,
            symbol,
            missing_timestamps.len()
        );

        // Group consecutive missing timestamps into ranges for efficient fetching
        let mut fetch_ranges: Vec<(DateTime<Utc>, DateTime<Utc>)> = Vec::new();
        if !missing_timestamps.is_empty() {
            let mut range_start = missing_timestamps[0];
            let mut range_end = missing_timestamps[0];

            for &timestamp in missing_timestamps.iter().skip(1) {
                let expected_next = range_end + Duration::milliseconds(interval_ms);
                if timestamp == expected_next {
                    // Consecutive, extend range
                    range_end = timestamp;
                } else {
                    // Gap detected, close current range and start new one
                    fetch_ranges
                        .push((range_start, range_end + Duration::milliseconds(interval_ms)));
                    range_start = timestamp;
                    range_end = timestamp;
                }
            }
            // Close the last range
            fetch_ranges.push((range_start, range_end + Duration::milliseconds(interval_ms)));
        }

        tracing::info!(
            "Symbol {}: Identified {} continuous ranges to fetch",
            symbol,
            fetch_ranges.len()
        );

        // Fetch klines for each missing range
        let mut symbol_klines_count = 0usize;
        let mut buffer: Vec<Kline> = Vec::new();

        for (range_idx, (range_start, range_end)) in fetch_ranges.iter().enumerate() {
            tracing::info!(
                "Symbol {}: Fetching range [{}/{}] from {} to {}",
                symbol,
                range_idx + 1,
                fetch_ranges.len(),
                range_start,
                range_end
            );

            let mut current_time = *range_start;

            while current_time < *range_end {
                // Calculate chunk end time
                let chunk_duration = Duration::milliseconds(interval_ms * chunk_size as i64);
                let chunk_end_time = (current_time + chunk_duration).min(*range_end);

                // Fetch klines for this chunk
                tracing::debug!(
                    "Fetching klines for {}/{} from {} to {} (limit: {})",
                    exchange,
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
                            tracing::debug!(
                                "No klines returned for {}/{} in range {} to {}",
                                exchange,
                                symbol,
                                current_time,
                                chunk_end_time
                            );
                            // No data returned, move to next chunk to avoid infinite loop
                            current_time = chunk_end_time;
                            continue;
                        }

                        tracing::debug!(
                            "Fetched {} klines for {}/{} (requested up to {})",
                            klines.len(),
                            symbol,
                            exchange,
                            chunk_size
                        );

                        // Get the latest timestamp from the fetched klines to advance current_time
                        let latest_kline_time = klines.iter().map(|k| k.open_time).max().unwrap();

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
                                        "Symbol {}/{}: Stored {} klines (total: {})",
                                        exchange,
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

                        // Move to the next interval after the latest fetched kline
                        // This ensures we don't skip data or get stuck in an infinite loop
                        current_time = latest_kline_time + Duration::milliseconds(interval_ms);

                        // If we've reached or passed the chunk_end_time, move to end_date check
                        if current_time >= chunk_end_time {
                            current_time = chunk_end_time;
                        }

                        // Note: Rate limiting is handled automatically by the RateLimiter in the exchange client
                    }
                    Err(e) => {
                        tracing::error!(
                            "Failed to fetch klines for {}/{} at {}: {}",
                            exchange,
                            symbol,
                            current_time,
                            e
                        );
                        // On error, skip to next chunk to avoid getting stuck
                        current_time = chunk_end_time;
                    }
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
                        "Symbol {}/{}: Stored final {} klines (total: {})",
                        exchange,
                        symbol,
                        buffer.len(),
                        symbol_klines_count
                    );
                }
                Err(e) => {
                    tracing::error!(
                        "Failed to store final klines for {}/{}: {}",
                        exchange,
                        symbol,
                        e
                    );
                    return Err(e);
                }
            }
        }

        tracing::info!(
            "Symbol {}: Backfill complete - {} klines stored",
            symbol,
            symbol_klines_count
        );
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
    client: Arc<Box<dyn IPerps + Send + Sync>>,
    repository: Arc<Mutex<PostgresRepository>>,
    batch_size: usize,
    skip_existing: bool,
    auto_discover: bool,
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
        tracing::info!(
            "[{}/{}] Processing symbol: {}",
            symbol_idx + 1,
            total_symbols,
            symbol
        );

        // Parse symbol to exchange-specific format
        let parsed_symbol = client.parse_symbol(symbol);

        // Discover or use provided start date
        let actual_start_date = if auto_discover {
            match get_or_discover_earliest_timestamp(&client, &repository, exchange, symbol).await {
                Ok(discovered_start) => {
                    tracing::info!(
                        "Symbol {}: Using earliest timestamp: {}",
                        symbol,
                        discovered_start
                    );
                    discovered_start
                }
                Err(e) => {
                    tracing::error!(
                        "Symbol {}: Failed to discover earliest timestamp: {}. Skipping this symbol.",
                        symbol,
                        e
                    );
                    continue;
                }
            }
        } else {
            start_date
        };

        // Calculate total time range
        let total_duration = end_date.signed_duration_since(actual_start_date);
        let total_hours = total_duration.num_hours();

        tracing::info!("Symbol {}: {} hours to backfill", symbol, total_hours);

        // If skip_existing is enabled, check for existing data
        let actual_start_date = if skip_existing {
            let repo = repository.lock().await;
            match repo
                .get_trades(exchange, symbol, start_date, end_date, Some(1))
                .await
            {
                Ok(trades) if !trades.is_empty() => {
                    let latest_trade = &trades[0];
                    tracing::info!(
                        "Found existing data up to {}, resuming from there",
                        latest_trade.timestamp
                    );
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
            let chunk_end_time =
                (current_time + Duration::hours(CHUNK_DURATION_HOURS)).min(end_date);

            // Fetch recent trades (note: get_recent_trades doesn't support time range)
            // We fetch the maximum and filter by timestamp
            tracing::debug!(
                "Fetching trades for {} from {} to {} (limit: {})",
                symbol,
                current_time,
                chunk_end_time,
                TRADES_PER_REQUEST
            );

            match client
                .get_recent_trades(&parsed_symbol, TRADES_PER_REQUEST)
                .await
            {
                Ok(trades) => {
                    total_api_calls += 1;

                    let total_trades_fetched = trades.len();

                    // Filter trades by timestamp range
                    let filtered_trades: Vec<Trade> = trades
                        .into_iter()
                        .filter(|t| t.timestamp >= current_time && t.timestamp < chunk_end_time)
                        .collect();

                    if filtered_trades.is_empty() {
                        tracing::debug!(
                            "No trades returned for {} in range {} to {}",
                            symbol,
                            current_time,
                            chunk_end_time
                        );
                        // Move to next chunk
                        current_time = chunk_end_time;
                        continue;
                    }

                    tracing::debug!(
                        "Fetched {} trades for {} (filtered to {})",
                        total_trades_fetched,
                        symbol,
                        filtered_trades.len()
                    );

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

                    // Note: Rate limiting is handled automatically by the RateLimiter in the exchange client
                }
                Err(e) => {
                    tracing::error!(
                        "Failed to fetch trades for {} at {}: {}",
                        symbol,
                        current_time,
                        e
                    );
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

        tracing::info!(
            "Symbol {}: Backfill complete - {} trades stored",
            symbol,
            symbol_trades_count
        );
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
    client: Arc<Box<dyn IPerps + Send + Sync>>,
    repository: Arc<Mutex<PostgresRepository>>,
    batch_size: usize,
    skip_existing: bool,
    auto_discover: bool,
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
        tracing::info!(
            "[{}/{}] Processing symbol: {}",
            symbol_idx + 1,
            total_symbols,
            symbol
        );

        // Parse symbol to exchange-specific format
        let parsed_symbol = client.parse_symbol(symbol);

        // Discover or use provided start date
        let actual_start_date = if auto_discover {
            match get_or_discover_earliest_timestamp(&client, &repository, exchange, symbol).await {
                Ok(discovered_start) => {
                    tracing::info!(
                        "Symbol {}: Using earliest timestamp: {}",
                        symbol,
                        discovered_start
                    );
                    discovered_start
                }
                Err(e) => {
                    tracing::error!(
                        "Symbol {}: Failed to discover earliest timestamp: {}. Skipping this symbol.",
                        symbol,
                        e
                    );
                    continue;
                }
            }
        } else {
            start_date
        };

        // Calculate total time range
        let total_duration = end_date.signed_duration_since(actual_start_date);
        let total_days = total_duration.num_days();

        tracing::info!("Symbol {}: {} days to backfill", symbol, total_days);

        // If skip_existing is enabled, check for existing data
        let actual_start_date = if skip_existing {
            let repo = repository.lock().await;
            match repo
                .get_funding_rates(exchange, symbol, start_date, end_date, Some(1))
                .await
            {
                Ok(rates) if !rates.is_empty() => {
                    let latest_rate = &rates[0];
                    tracing::info!(
                        "Found existing data up to {}, resuming from there",
                        latest_rate.funding_time
                    );
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
                        tracing::debug!(
                            "No funding rates returned for {} in range {} to {}",
                            symbol,
                            current_time,
                            chunk_end_time
                        );
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
                        match repo
                            .store_funding_rates_with_exchange(exchange, &buffer)
                            .await
                        {
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
                                tracing::error!(
                                    "Failed to store funding rates for {}: {}",
                                    symbol,
                                    e
                                );
                                return Err(e);
                            }
                        }
                    }

                    // Move to next chunk
                    current_time = chunk_end_time;

                    // Note: Rate limiting is handled automatically by the RateLimiter in the exchange client
                }
                Err(e) => {
                    tracing::error!(
                        "Failed to fetch funding rates for {} at {}: {}",
                        symbol,
                        current_time,
                        e
                    );
                    // Continue to next chunk instead of failing completely
                    current_time = chunk_end_time;
                }
            }
        }

        // Flush remaining buffer
        if !buffer.is_empty() {
            let repo = repository.lock().await;
            match repo
                .store_funding_rates_with_exchange(exchange, &buffer)
                .await
            {
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

        tracing::info!(
            "Symbol {}: Backfill complete - {} funding rates stored",
            symbol,
            symbol_rates_count
        );
    }

    tracing::info!(
        "Backfill summary: {} symbols, {} funding rates stored, {} API calls",
        total_symbols,
        total_rates_stored,
        total_api_calls
    );

    Ok(())
}
