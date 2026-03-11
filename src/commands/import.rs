use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use clap::Args;
use perps_aggregator::{Aggregator, IAggregator};
use perps_core::{MultiResolutionOrderbook, Orderbook, OrderbookLevel};
use perps_database::{create_partitions_for_range, PgPool, PostgresRepository, Repository};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::time::Instant;

#[derive(Args)]
pub struct ImportArgs {
    #[command(subcommand)]
    pub command: ImportCommands,
}

#[derive(clap::Subcommand)]
pub enum ImportCommands {
    /// Import legacy orderbook data from tab-separated dump file
    Orderbooks(ImportOrderbooksArgs),
}

#[derive(Args)]
pub struct ImportOrderbooksArgs {
    /// Path to legacy .sql dump file (tab-separated rows)
    #[arg(short, long)]
    pub file: String,

    /// Rows per DB transaction batch
    #[arg(short, long, default_value = "500")]
    pub batch_size: usize,

    /// Skip first N data rows (for resuming)
    #[arg(long, default_value = "0")]
    pub skip_rows: usize,

    /// Parse and compute but don't write to DB
    #[arg(long)]
    pub dry_run: bool,

    /// Database URL
    #[arg(long, env = "DATABASE_URL")]
    pub database_url: Option<String>,

    /// Which tables to populate: all, orderbooks, liquidity, slippage (comma-separated)
    #[arg(long, default_value = "all")]
    pub tables: String,
}

pub async fn execute(args: ImportArgs) -> Result<()> {
    match args.command {
        ImportCommands::Orderbooks(args) => execute_import_orderbooks(args).await,
    }
}

fn exchange_id_to_name(id: i32) -> Option<&'static str> {
    match id {
        1 => Some("binance"),
        2 => Some("bybit"),
        3 => Some("extended"),
        4 => Some("kucoin"),
        5 => Some("lighter"),
        6 => Some("paradex"),
        7 => Some("hyperliquid"),
        8 => Some("aster"),
        9 => Some("pacifica"),
        10 => Some("nado"),
        11 => Some("gravity"),
        12 => Some("01"),
        _ => None,
    }
}

#[derive(serde::Deserialize)]
struct RawOrderbookJson {
    asks: Vec<OrderbookLevel>,
    bids: Vec<OrderbookLevel>,
}

struct ParsedRow {
    exchange_name: String,
    symbol: String,
    orderbook: Orderbook,
}

fn parse_timestamp(ts_str: &str) -> Result<DateTime<Utc>> {
    // Try parsing with fractional seconds + timezone: "2026-02-21 02:36:51.948563+00"
    if let Ok(dt) = DateTime::parse_from_str(ts_str, "%Y-%m-%d %H:%M:%S%.f%#z") {
        return Ok(dt.with_timezone(&Utc));
    }
    // Try without fractional seconds + timezone: "2026-02-21 02:36:51+00"
    if let Ok(dt) = DateTime::parse_from_str(ts_str, "%Y-%m-%d %H:%M:%S%#z") {
        return Ok(dt.with_timezone(&Utc));
    }
    // Try as naive datetime (no timezone)
    if let Ok(ndt) = NaiveDateTime::parse_from_str(ts_str, "%Y-%m-%d %H:%M:%S%.f") {
        return Ok(DateTime::<Utc>::from_naive_utc_and_offset(ndt, Utc));
    }
    if let Ok(ndt) = NaiveDateTime::parse_from_str(ts_str, "%Y-%m-%d %H:%M:%S") {
        return Ok(DateTime::<Utc>::from_naive_utc_and_offset(ndt, Utc));
    }
    anyhow::bail!("Failed to parse timestamp: {}", ts_str)
}

fn parse_line(line: &str) -> Result<ParsedRow> {
    let fields: Vec<&str> = line.split('\t').collect();
    if fields.len() < 14 {
        anyhow::bail!(
            "Expected at least 14 tab-separated fields, got {}",
            fields.len()
        );
    }

    let exchange_id: i32 = fields[1]
        .trim()
        .parse()
        .context("Failed to parse exchange_id")?;
    let exchange_name = exchange_id_to_name(exchange_id)
        .ok_or_else(|| anyhow::anyhow!("Unknown exchange_id: {}", exchange_id))?
        .to_string();

    let symbol = fields[2].trim().to_string();
    let raw_json_str = fields[5].trim();
    let ts_str = fields[7].trim();

    let timestamp = parse_timestamp(ts_str).context("Failed to parse timestamp field")?;

    let raw: RawOrderbookJson =
        serde_json::from_str(raw_json_str).context("Failed to parse orderbook JSON")?;

    let orderbook = Orderbook {
        symbol: symbol.clone(),
        bids: raw.bids,
        asks: raw.asks,
        timestamp,
    };

    Ok(ParsedRow {
        exchange_name,
        symbol,
        orderbook,
    })
}

/// Scan the file to find the min and max dates in the timestamp column.
fn scan_date_range(file_path: &str) -> Result<(NaiveDate, NaiveDate)> {
    tracing::info!("Scanning file for date range...");
    let start = Instant::now();

    let file = std::fs::File::open(file_path)
        .with_context(|| format!("Failed to open file: {}", file_path))?;
    let reader = BufReader::new(file);

    let mut min_date: Option<NaiveDate> = None;
    let mut max_date: Option<NaiveDate> = None;
    let mut line_count: u64 = 0;

    for line_result in reader.lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("Error reading line {}: {}", line_count + 1, e);
                continue;
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let fields: Vec<&str> = trimmed.split('\t').collect();
        if fields.len() < 8 {
            continue;
        }

        let ts_str = fields[7].trim();
        if let Ok(dt) = parse_timestamp(ts_str) {
            let date = dt.date_naive();
            min_date = Some(min_date.map_or(date, |d: NaiveDate| d.min(date)));
            max_date = Some(max_date.map_or(date, |d: NaiveDate| d.max(date)));
        }

        line_count += 1;
    }

    let elapsed = start.elapsed();
    tracing::info!(
        "Date range scan complete: {} lines in {:.1}s",
        line_count,
        elapsed.as_secs_f64()
    );

    let min = min_date.ok_or_else(|| anyhow::anyhow!("No valid dates found in file"))?;
    let max = max_date.ok_or_else(|| anyhow::anyhow!("No valid dates found in file"))?;

    tracing::info!("Date range: {} to {}", min, max);
    Ok((min, max))
}

/// Determine which table workers should run based on the --tables flag.
fn parse_table_selection(tables: &str) -> (bool, bool, bool) {
    let parts: Vec<String> = tables.split(',').map(|s| s.trim().to_lowercase()).collect();

    if parts.iter().any(|p| p == "all") {
        return (true, true, true);
    }

    let do_orderbooks = parts.iter().any(|p| p == "orderbooks");
    let do_liquidity = parts.iter().any(|p| p == "liquidity");
    let do_slippage = parts.iter().any(|p| p == "slippage");

    (do_orderbooks, do_liquidity, do_slippage)
}

async fn execute_import_orderbooks(args: ImportOrderbooksArgs) -> Result<()> {
    let file_path = args.file.clone();

    // Verify file exists
    if !std::path::Path::new(&file_path).exists() {
        anyhow::bail!("File not found: {}", file_path);
    }

    let (do_orderbooks, do_liquidity, do_slippage) = parse_table_selection(&args.tables);

    if !do_orderbooks && !do_liquidity && !do_slippage {
        anyhow::bail!(
            "No tables selected. Use --tables with: all, orderbooks, liquidity, slippage"
        );
    }

    tracing::info!(
        "Import configuration: file={}, batch_size={}, skip_rows={}, dry_run={}, tables=[{}{}{}]",
        file_path,
        args.batch_size,
        args.skip_rows,
        args.dry_run,
        if do_orderbooks { "orderbooks " } else { "" },
        if do_liquidity { "liquidity " } else { "" },
        if do_slippage { "slippage" } else { "" },
    );

    // Connect to DB and create partitions (unless dry run)
    let pool = if !args.dry_run {
        let db_url = args.database_url.as_deref().ok_or_else(|| {
            anyhow::anyhow!("DATABASE_URL is required (use --database-url or set DATABASE_URL env)")
        })?;

        let pool = PgPool::connect(db_url)
            .await
            .context("Failed to connect to database")?;
        tracing::info!("Connected to database");

        // Step 0: Scan file for date range and create partitions
        let (min_date, max_date) = scan_date_range(&file_path)?;
        create_partitions_for_range(&pool, min_date, max_date).await?;
        tracing::info!(
            "Database partitions created for {} to {}",
            min_date,
            max_date
        );

        Some(pool)
    } else {
        tracing::info!("Dry run mode: skipping database connection and partition creation");
        None
    };

    let batch_size = args.batch_size;
    let skip_rows = args.skip_rows;
    let dry_run = args.dry_run;

    // Spawn workers based on table selection
    let orderbooks_handle = if do_orderbooks {
        let fp = file_path.clone();
        let p = pool.clone();
        Some(tokio::spawn(async move {
            worker_orderbooks(&fp, batch_size, skip_rows, dry_run, p.as_ref()).await
        }))
    } else {
        None
    };

    let liquidity_handle = if do_liquidity {
        let fp = file_path.clone();
        let p = pool.clone();
        Some(tokio::spawn(async move {
            worker_liquidity(&fp, batch_size, skip_rows, dry_run, p.as_ref()).await
        }))
    } else {
        None
    };

    let slippage_handle = if do_slippage {
        let fp = file_path.clone();
        let p = pool.clone();
        Some(tokio::spawn(async move {
            worker_slippage(&fp, batch_size, skip_rows, dry_run, p.as_ref()).await
        }))
    } else {
        None
    };

    // Await all spawned workers
    let mut results: Vec<Result<()>> = Vec::new();

    if let Some(h) = orderbooks_handle {
        results.push(h.await.context("orderbooks worker panicked")?);
    }
    if let Some(h) = liquidity_handle {
        results.push(h.await.context("liquidity worker panicked")?);
    }
    if let Some(h) = slippage_handle {
        results.push(h.await.context("slippage worker panicked")?);
    }

    // Report results
    let mut had_error = false;
    for result in &results {
        if let Err(e) = result {
            tracing::error!("Worker failed: {:?}", e);
            had_error = true;
        }
    }

    if had_error {
        anyhow::bail!("One or more import workers failed");
    }

    tracing::info!("Import complete");
    Ok(())
}

/// Worker that reads the file and stores orderbook summaries.
async fn worker_orderbooks(
    file_path: &str,
    batch_size: usize,
    skip_rows: usize,
    dry_run: bool,
    pool: Option<&PgPool>,
) -> Result<()> {
    let label = "orderbooks";
    tracing::info!("[{}] Starting worker", label);
    let start = Instant::now();

    let repo = match (dry_run, pool) {
        (false, Some(p)) => Some(PostgresRepository::new(p.clone())),
        _ => None,
    };

    let file = std::fs::File::open(file_path)
        .with_context(|| format!("[{}] Failed to open file: {}", label, file_path))?;
    let reader = BufReader::new(file);

    let mut row_count: u64 = 0;
    let mut processed: u64 = 0;
    let mut skipped_parse: u64 = 0;
    // batch: exchange_name -> Vec<Orderbook>
    let mut batch: HashMap<String, Vec<Orderbook>> = HashMap::new();
    let mut batch_len: usize = 0;

    for line_result in reader.lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("[{}] Error reading line: {}", label, e);
                continue;
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        row_count += 1;

        if row_count <= skip_rows as u64 {
            continue;
        }

        match parse_line(trimmed) {
            Ok(parsed) => {
                batch
                    .entry(parsed.exchange_name)
                    .or_default()
                    .push(parsed.orderbook);
                batch_len += 1;
            }
            Err(e) => {
                skipped_parse += 1;
                if skipped_parse <= 10 {
                    tracing::warn!(
                        "[{}] Skipping row {} due to parse error: {}",
                        label,
                        row_count,
                        e
                    );
                }
                continue;
            }
        }

        if batch_len >= batch_size {
            if let Some(ref repo) = repo {
                flush_orderbooks_batch(repo, &mut batch, label).await?;
            } else {
                batch.clear();
            }
            processed += batch_len as u64;
            let elapsed = start.elapsed().as_secs_f64();
            let rate = processed as f64 / elapsed;
            tracing::info!(
                "[{}] Stored {} rows ({:.0} rows/sec)",
                label,
                processed,
                rate
            );
            batch_len = 0;
        }
    }

    // Flush remaining
    if batch_len > 0 {
        if let Some(ref repo) = repo {
            flush_orderbooks_batch(repo, &mut batch, label).await?;
        }
        processed += batch_len as u64;
    }

    let elapsed = start.elapsed().as_secs_f64();
    tracing::info!(
        "[{}] Complete: {} rows processed, {} parse errors, {:.1}s total ({:.0} rows/sec)",
        label,
        processed,
        skipped_parse,
        elapsed,
        processed as f64 / elapsed.max(0.001)
    );

    Ok(())
}

async fn flush_orderbooks_batch(
    repo: &PostgresRepository,
    batch: &mut HashMap<String, Vec<Orderbook>>,
    label: &str,
) -> Result<()> {
    for (exchange, orderbooks) in batch.iter() {
        if let Err(e) = repo
            .store_orderbooks_with_exchange(exchange, orderbooks)
            .await
        {
            tracing::error!(
                "[{}] Failed to store {} orderbooks for {}: {}",
                label,
                orderbooks.len(),
                exchange,
                e
            );
        }
    }
    batch.clear();
    Ok(())
}

/// Worker that reads the file, computes liquidity depth, and stores results.
async fn worker_liquidity(
    file_path: &str,
    batch_size: usize,
    skip_rows: usize,
    dry_run: bool,
    pool: Option<&PgPool>,
) -> Result<()> {
    let label = "liquidity_depth";
    tracing::info!("[{}] Starting worker", label);
    let start = Instant::now();

    let repo = match (dry_run, pool) {
        (false, Some(p)) => Some(PostgresRepository::new(p.clone())),
        _ => None,
    };

    let aggregator = Aggregator::new();

    let file = std::fs::File::open(file_path)
        .with_context(|| format!("[{}] Failed to open file: {}", label, file_path))?;
    let reader = BufReader::new(file);

    let mut row_count: u64 = 0;
    let mut processed: u64 = 0;
    let mut skipped_parse: u64 = 0;
    let mut skipped_compute: u64 = 0;
    let mut batch: Vec<perps_core::LiquidityDepthStats> = Vec::with_capacity(batch_size);

    for line_result in reader.lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("[{}] Error reading line: {}", label, e);
                continue;
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        row_count += 1;

        if row_count <= skip_rows as u64 {
            continue;
        }

        let parsed = match parse_line(trimmed) {
            Ok(p) => p,
            Err(e) => {
                skipped_parse += 1;
                if skipped_parse <= 10 {
                    tracing::warn!(
                        "[{}] Skipping row {} due to parse error: {}",
                        label,
                        row_count,
                        e
                    );
                }
                continue;
            }
        };

        let original_ts = parsed.orderbook.timestamp;
        let multi = MultiResolutionOrderbook::from_single(parsed.orderbook);
        match aggregator
            .calculate_liquidity_depth(&multi, &parsed.exchange_name, &parsed.symbol)
            .await
        {
            Ok(mut stats) => {
                // Override timestamp with original orderbook timestamp
                // (aggregator uses Utc::now() which is wrong for historical import)
                stats.timestamp = original_ts;
                batch.push(stats);
            }
            Err(e) => {
                skipped_compute += 1;
                if skipped_compute <= 10 {
                    tracing::warn!(
                        "[{}] Skipping row {} due to liquidity calc error: {}",
                        label,
                        row_count,
                        e
                    );
                }
                continue;
            }
        }

        if batch.len() >= batch_size {
            let flush_count = batch.len();
            if let Some(ref repo) = repo {
                if let Err(e) = repo.store_liquidity_depth(&batch).await {
                    tracing::error!(
                        "[{}] Failed to store {} liquidity stats: {}",
                        label,
                        flush_count,
                        e
                    );
                }
            }
            batch.clear();
            processed += flush_count as u64;
            let elapsed = start.elapsed().as_secs_f64();
            let rate = processed as f64 / elapsed;
            tracing::info!(
                "[{}] Stored {} rows ({:.0} rows/sec)",
                label,
                processed,
                rate
            );
        }
    }

    // Flush remaining
    if !batch.is_empty() {
        let flush_count = batch.len();
        if let Some(ref repo) = repo {
            if let Err(e) = repo.store_liquidity_depth(&batch).await {
                tracing::error!(
                    "[{}] Failed to store {} liquidity stats: {}",
                    label,
                    flush_count,
                    e
                );
            }
        }
        processed += flush_count as u64;
    }

    let elapsed = start.elapsed().as_secs_f64();
    tracing::info!(
        "[{}] Complete: {} rows processed, {} parse errors, {} compute errors, {:.1}s total ({:.0} rows/sec)",
        label,
        processed,
        skipped_parse,
        skipped_compute,
        elapsed,
        processed as f64 / elapsed.max(0.001)
    );

    Ok(())
}

/// Worker that reads the file, computes slippage for 7 trade amounts, and stores results.
async fn worker_slippage(
    file_path: &str,
    batch_size: usize,
    skip_rows: usize,
    dry_run: bool,
    pool: Option<&PgPool>,
) -> Result<()> {
    let label = "slippage";
    tracing::info!("[{}] Starting worker", label);
    let start = Instant::now();

    let repo = match (dry_run, pool) {
        (false, Some(p)) => Some(PostgresRepository::new(p.clone())),
        _ => None,
    };

    let aggregator = Aggregator::new();

    let file = std::fs::File::open(file_path)
        .with_context(|| format!("[{}] Failed to open file: {}", label, file_path))?;
    let reader = BufReader::new(file);

    let mut row_count: u64 = 0;
    let mut processed: u64 = 0;
    let mut skipped_parse: u64 = 0;
    // batch: exchange_name -> Vec<Slippage>
    let mut batch: HashMap<String, Vec<perps_core::Slippage>> = HashMap::new();
    let mut batch_len: usize = 0;

    for line_result in reader.lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("[{}] Error reading line: {}", label, e);
                continue;
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        row_count += 1;

        if row_count <= skip_rows as u64 {
            continue;
        }

        let parsed = match parse_line(trimmed) {
            Ok(p) => p,
            Err(e) => {
                skipped_parse += 1;
                if skipped_parse <= 10 {
                    tracing::warn!(
                        "[{}] Skipping row {} due to parse error: {}",
                        label,
                        row_count,
                        e
                    );
                }
                continue;
            }
        };

        let original_ts = parsed.orderbook.timestamp;
        let multi = MultiResolutionOrderbook::from_single(parsed.orderbook);
        let mut slippages = aggregator.calculate_all_slippages(&multi);

        // Override timestamps with original orderbook timestamp
        for slip in &mut slippages {
            slip.timestamp = original_ts;
        }

        if !slippages.is_empty() {
            let entry = batch.entry(parsed.exchange_name).or_default();
            batch_len += slippages.len();
            entry.extend(slippages);
        }

        if batch_len >= batch_size {
            if let Some(ref repo) = repo {
                flush_slippage_batch(repo, &mut batch, label).await?;
            } else {
                batch.clear();
            }
            processed += batch_len as u64;
            let elapsed = start.elapsed().as_secs_f64();
            let rate = processed as f64 / elapsed;
            tracing::info!(
                "[{}] Stored {} rows ({:.0} rows/sec)",
                label,
                processed,
                rate
            );
            batch_len = 0;
        }
    }

    // Flush remaining
    if batch_len > 0 {
        if let Some(ref repo) = repo {
            flush_slippage_batch(repo, &mut batch, label).await?;
        }
        processed += batch_len as u64;
    }

    let elapsed = start.elapsed().as_secs_f64();
    tracing::info!(
        "[{}] Complete: {} rows processed, {} parse errors, {:.1}s total ({:.0} rows/sec)",
        label,
        processed,
        skipped_parse,
        elapsed,
        processed as f64 / elapsed.max(0.001)
    );

    Ok(())
}

async fn flush_slippage_batch(
    repo: &PostgresRepository,
    batch: &mut HashMap<String, Vec<perps_core::Slippage>>,
    label: &str,
) -> Result<()> {
    for (exchange, slippages) in batch.iter() {
        if let Err(e) = repo.store_slippage_with_exchange(exchange, slippages).await {
            tracing::error!(
                "[{}] Failed to store {} slippages for {}: {}",
                label,
                slippages.len(),
                exchange,
                e
            );
        }
    }
    batch.clear();
    Ok(())
}
