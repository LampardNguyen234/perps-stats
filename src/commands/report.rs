//! `report` command: DB-only, multi-symbol x multi-exchange report mirroring the
//! Grafana dashboards (Summary, Liquidity Depth, Spread, Slippage).
//!
//! Unlike the `stats` subcommands, this command allows multiple symbols AND
//! multiple exchanges simultaneously — every section groups by (symbol, exchange).

use crate::commands::volatility::{self, VolatilityMethod};
use anyhow::{Context, Result};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use clap::Args;
use futures::stream::{self, StreamExt};
use perps_core::IPerps;
use perps_exchanges::factory;
use serde_json::json;
use sqlx::PgPool;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Trade sizes reported in the Slippage section (subset of the aggregator's full
/// TRADE_AMOUNTS list — $5M/$10M are excluded as not relevant for this report).
const TRADE_AMOUNTS: [i64; 5] = [1_000, 10_000, 50_000, 100_000, 500_000];

/// Liquidity depth bps levels: (display label, bid column, ask column).
const BPS_LEVELS: [(&str, &str, &str); 5] = [
    ("1", "bid_1bps", "ask_1bps"),
    ("2.5", "bid_2_5bps", "ask_2_5bps"),
    ("5", "bid_5bps", "ask_5bps"),
    ("10", "bid_10bps", "ask_10bps"),
    ("20", "bid_20bps", "ask_20bps"),
];

/// Max (symbol, exchange) groups aggregated by a single query. Bounds Postgres
/// sort/work_mem cost for PERCENTILE_CONT/STDDEV_POP over wide --range windows.
const MAX_PAIRS_PER_QUERY: usize = 40;

/// Max symbol-chunk queries run concurrently against the pool.
const MAX_CONCURRENT_CHUNKS: usize = 6;

/// Max volatility kline fetches run concurrently against Binance.
const MAX_CONCURRENT_VOLATILITY_REQUESTS: usize = 8;

#[derive(Args)]
pub struct ReportArgs {
    /// Comma-separated global symbols (e.g. BTC,ETH). Defaults to all symbols in DB.
    #[arg(short, long)]
    pub symbols: Option<String>,

    /// Comma-separated exchange names. Defaults to all exchanges in DB.
    #[arg(short, long)]
    pub exchanges: Option<String>,

    /// Start datetime (YYYY-MM-DD or RFC3339).
    #[arg(long)]
    pub from: Option<String>,

    /// End datetime (YYYY-MM-DD or RFC3339). Defaults to now.
    #[arg(long)]
    pub to: Option<String>,

    /// Shorthand duration: 1m, 30m, 1h, 4h, 12h, 1d, 7d. Used to derive whichever
    /// of --from/--to is missing. Ignored if both --from and --to are given.
    #[arg(long, default_value = "1d")]
    pub range: String,

    /// Output format (table, json, csv)
    #[arg(short, long, default_value = "table")]
    pub format: String,

    /// Output directory
    #[arg(long, default_value = "out")]
    pub output_dir: String,

    /// Output file name. Defaults to report_YYYYMMDD_hhmmss.<ext>, extension matching --format
    /// (table -> md, json -> json, csv -> csv).
    #[arg(short, long)]
    pub output: Option<String>,

    /// Database URL (required)
    #[arg(long, env = "DATABASE_URL")]
    pub database_url: Option<String>,

    /// Lookback duration for the Volatility overview (Summary section), independent
    /// of --range. Same shorthand syntax as --range (e.g. 30d, 4w).
    #[arg(long, default_value = "90d")]
    pub vol_range: String,

    /// Kline interval for the Volatility overview (e.g. 1h, 4h, 1d).
    #[arg(long, default_value = "1h")]
    pub vol_interval: String,
}

// ─── Row types ────────────────────────────────────────────────────────────────

#[derive(Debug, sqlx::FromRow)]
struct SummaryRow {
    symbol: String,
    exchange: String,
    median_volume: Option<f64>,
    mean_volume: Option<f64>,
    median_oi: Option<f64>,
    mean_oi: Option<f64>,
}

#[derive(Debug, sqlx::FromRow, Clone)]
struct LiquidityRow {
    symbol: String,
    exchange: String,
    bid_mean: Option<f64>,
    bid_median: Option<f64>,
    bid_max: Option<f64>,
    bid_stddev: Option<f64>,
    ask_mean: Option<f64>,
    ask_median: Option<f64>,
    ask_max: Option<f64>,
    ask_stddev: Option<f64>,
}

#[derive(Debug, sqlx::FromRow)]
struct SpreadRow {
    symbol: String,
    exchange: String,
    mean_bps: Option<f64>,
    median_bps: Option<f64>,
    stddev_bps: Option<f64>,
    p95_bps: Option<f64>,
    max_bps: Option<f64>,
}

/// Annualized volatility overview for one symbol, computed from Binance's own
/// candles (see VOLATILITY_LOOKBACK_DAYS/VOLATILITY_INTERVAL) — not per-exchange.
struct VolatilityRow {
    symbol: String,
    bars: usize,
    realized_pct: Option<f64>,
    parkinson_pct: Option<f64>,
    garman_klass_pct: Option<f64>,
}

/// Distribution of per-candle (high-low)/low swing % over the Binance kline window.
struct PriceSwingRow {
    symbol: String,
    bars: usize,
    min: Option<f64>,
    mean: Option<f64>,
    median: Option<f64>,
    stddev: Option<f64>,
    p95: Option<f64>,
    p99: Option<f64>,
    max: Option<f64>,
}

/// The single candle with the largest (high-low)/low swing in the window.
struct MaxSwingRow {
    symbol: String,
    open_time: DateTime<Utc>,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
    swing_pct: f64,
}

#[derive(Debug, sqlx::FromRow, Clone)]
struct SlippageRow {
    symbol: String,
    exchange: String,
    buy_mean: Option<f64>,
    buy_median: Option<f64>,
    buy_stddev: Option<f64>,
    buy_p95: Option<f64>,
    buy_p99: Option<f64>,
    sell_mean: Option<f64>,
    sell_median: Option<f64>,
    sell_stddev: Option<f64>,
    sell_p95: Option<f64>,
    sell_p99: Option<f64>,
}

// ─── Entry point ────────────────────────────────────────────────────────────

pub async fn execute(args: ReportArgs) -> Result<()> {
    let db_url = args.database_url.ok_or_else(|| {
        anyhow::anyhow!(
            "DATABASE_URL is required. Set via --database-url flag or DATABASE_URL environment variable"
        )
    })?;

    tracing::info!("Connecting to database");
    let pool = PgPool::connect(&db_url)
        .await
        .context("Failed to connect to database")?;

    let symbols: Vec<String> = match args.symbols {
        Some(ref s) => s
            .split(',')
            .map(|v| v.trim().to_uppercase())
            .filter(|v| !v.is_empty())
            .collect(),
        None => get_all_symbol_names(&pool).await?,
    };
    if symbols.is_empty() {
        anyhow::bail!("No symbols found in database");
    }

    let exchanges: Vec<String> = match args.exchanges {
        Some(ref e) => e
            .split(',')
            .map(|v| v.trim().to_lowercase())
            .filter(|v| !v.is_empty())
            .collect(),
        None => get_all_exchange_names(&pool).await?,
    };
    if exchanges.is_empty() {
        anyhow::bail!("No exchanges found in database");
    }

    let exchange_ids = get_exchange_ids(&pool, &exchanges).await?;
    if exchange_ids.is_empty() {
        anyhow::bail!("No valid exchanges found in database");
    }

    let (from, to) = resolve_time_range(args.from, args.to, &args.range)?;
    tracing::info!(%from, %to, "Resolved report time range");

    // Chunk along the symbol axis (never split a single symbol×exchange group's
    // rows across two queries — PERCENTILE_CONT/STDDEV_POP aggregates cannot be
    // correctly merged post-hoc, e.g. median-of-medians != true median). Chunking
    // only caps how many groups a single query aggregates at once, bounding
    // Postgres sort/work_mem cost for wide --range windows (30d, 90d, ...).
    let symbol_chunks = chunk_symbols(&symbols, exchange_ids.len());

    let summary_rows = merge_chunks(
        symbol_chunks
            .iter()
            .map(|chunk| fetch_summary(&pool, chunk, &exchange_ids, from, to)),
    )
    .await?;

    let mut liquidity_rows: Vec<(&'static str, Vec<LiquidityRow>)> = Vec::new();
    for (label, bid_col, ask_col) in BPS_LEVELS {
        let rows =
            merge_chunks(symbol_chunks.iter().map(|chunk| {
                fetch_liquidity(&pool, chunk, &exchange_ids, from, to, bid_col, ask_col)
            }))
            .await?;
        liquidity_rows.push((label, rows));
    }

    let spread_rows = merge_chunks(
        symbol_chunks
            .iter()
            .map(|chunk| fetch_spread(&pool, chunk, &exchange_ids, from, to)),
    )
    .await?;

    let mut slippage_rows: Vec<(i64, Vec<SlippageRow>)> = Vec::new();
    for amount in TRADE_AMOUNTS {
        let rows = merge_chunks(
            symbol_chunks
                .iter()
                .map(|chunk| fetch_slippage(&pool, chunk, &exchange_ids, from, to, amount)),
        )
        .await?;
        slippage_rows.push((amount, rows));
    }

    let (volatility_rows, price_swing_rows, max_swing_rows) =
        match fetch_binance_overview(&symbols, &args.vol_range, &args.vol_interval).await {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(%error, "Failed to fetch volatility overview; omitting from report");
                (Vec::new(), Vec::new(), Vec::new())
            }
        };

    let report = ReportData {
        from,
        to,
        symbols: symbols.clone(),
        exchanges: exchanges.clone(),
        summary_rows,
        liquidity_rows,
        spread_rows,
        slippage_rows,
        volatility_rows,
        price_swing_rows,
        max_swing_rows,
        vol_range: args.vol_range.clone(),
        vol_interval: args.vol_interval.clone(),
    };

    let format = args.format.to_lowercase();
    let content = match format.as_str() {
        "json" => render_json(&report)?,
        "csv" => render_csv(&report),
        _ => render_table(&report),
    };

    let path = resolve_output_path(&args.output_dir, args.output.as_deref(), &format)?;
    write_output(&content, &path)
}

/// Resolves the output file path, generating a timestamped default name
/// (report_YYYYMMDD_hhmmss.<ext>) when `filename` is not given. Creates
/// `output_dir` if missing.
fn resolve_output_path(
    output_dir: &str,
    filename: Option<&str>,
    format: &str,
) -> Result<std::path::PathBuf> {
    std::fs::create_dir_all(output_dir)
        .with_context(|| format!("Failed to create output directory {}", output_dir))?;

    let ext = match format {
        "json" => "json",
        "csv" => "csv",
        _ => "md",
    };
    let name = match filename {
        Some(f) => f.to_string(),
        None => format!("report_{}.{}", Utc::now().format("%Y%m%d_%H%M%S"), ext),
    };
    Ok(std::path::Path::new(output_dir).join(name))
}

struct ReportData {
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    symbols: Vec<String>,
    exchanges: Vec<String>,
    summary_rows: Vec<SummaryRow>,
    liquidity_rows: Vec<(&'static str, Vec<LiquidityRow>)>,
    spread_rows: Vec<SpreadRow>,
    slippage_rows: Vec<(i64, Vec<SlippageRow>)>,
    volatility_rows: Vec<VolatilityRow>,
    price_swing_rows: Vec<PriceSwingRow>,
    max_swing_rows: Vec<MaxSwingRow>,
    vol_range: String,
    vol_interval: String,
}

// ─── Time range resolution ──────────────────────────────────────────────────

/// Parses a shorthand duration like `1m`, `30m`, `1h`, `4h`, `12h`, `1d`, `7d`, `2w`.
fn parse_range(range: &str) -> Result<Duration> {
    let range = range.trim();
    if range.len() < 2 {
        anyhow::bail!("Invalid --range '{}'. Expected e.g. 1h, 1d, 7d", range);
    }
    let (num_str, unit) = range.split_at(range.len() - 1);
    let n: i64 = num_str
        .parse()
        .with_context(|| format!("Invalid numeric prefix in --range '{}'", range))?;
    match unit {
        "m" => Ok(Duration::minutes(n)),
        "h" => Ok(Duration::hours(n)),
        "d" => Ok(Duration::days(n)),
        "w" => Ok(Duration::weeks(n)),
        other => anyhow::bail!(
            "Unknown range unit '{}' in --range '{}'. Supported: m, h, d, w",
            other,
            range
        ),
    }
}

/// Parses `YYYY-MM-DD` or RFC3339 into a UTC datetime.
fn parse_datetime(s: &str) -> Result<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        let naive = d
            .and_hms_opt(0, 0, 0)
            .context("Failed to construct midnight timestamp")?;
        return Ok(DateTime::from_naive_utc_and_offset(naive, Utc));
    }
    anyhow::bail!("Invalid datetime '{}'. Expected YYYY-MM-DD or RFC3339", s)
}

/// Resolves (from, to) per the rules:
/// - both given -> used directly, --range ignored
/// - only --from -> to = from + range
/// - only --to -> from = to - range
/// - neither -> to = now, from = now - range
fn resolve_time_range(
    from: Option<String>,
    to: Option<String>,
    range: &str,
) -> Result<(DateTime<Utc>, DateTime<Utc>)> {
    match (from, to) {
        (Some(f), Some(t)) => Ok((parse_datetime(&f)?, parse_datetime(&t)?)),
        (Some(f), None) => {
            let f = parse_datetime(&f)?;
            let dur = parse_range(range)?;
            Ok((f, f + dur))
        }
        (None, Some(t)) => {
            let t = parse_datetime(&t)?;
            let dur = parse_range(range)?;
            Ok((t - dur, t))
        }
        (None, None) => {
            let t = Utc::now();
            let dur = parse_range(range)?;
            Ok((t - dur, t))
        }
    }
}

// ─── Small DB helpers (duplicated from stats.rs to avoid touching it) ──────

async fn get_all_symbol_names(pool: &PgPool) -> Result<Vec<String>> {
    sqlx::query_scalar::<_, String>("SELECT DISTINCT symbol FROM tickers ORDER BY symbol")
        .fetch_all(pool)
        .await
        .context("Failed to fetch symbol names")
}

async fn get_all_exchange_names(pool: &PgPool) -> Result<Vec<String>> {
    sqlx::query_scalar::<_, String>("SELECT name FROM exchanges ORDER BY name")
        .fetch_all(pool)
        .await
        .context("Failed to fetch exchange names")
}

async fn get_exchange_ids(pool: &PgPool, exchanges: &[String]) -> Result<Vec<i32>> {
    let placeholders: Vec<String> = (1..=exchanges.len()).map(|i| format!("${}", i)).collect();
    let query = format!(
        "SELECT id FROM exchanges WHERE LOWER(name) IN ({})",
        placeholders.join(", ")
    );
    let mut qb = sqlx::query_scalar::<_, i32>(&query);
    for e in exchanges {
        qb = qb.bind(e.to_lowercase());
    }
    qb.fetch_all(pool)
        .await
        .context("Failed to fetch exchange IDs")
}

/// Builds `$start, $start+1, ...` placeholders for an IN clause.
fn in_placeholders(start: usize, count: usize) -> Vec<String> {
    (start..start + count).map(|i| format!("${}", i)).collect()
}

/// Splits `symbols` into batches so each batch × `exchange_count` stays under
/// `MAX_PAIRS_PER_QUERY`. Splitting only along the symbol axis keeps every
/// (symbol, exchange) group's full row set inside a single query — required
/// for PERCENTILE_CONT/STDDEV_POP correctness (see MAX_PAIRS_PER_QUERY).
fn chunk_symbols(symbols: &[String], exchange_count: usize) -> Vec<Vec<String>> {
    let batch_size = (MAX_PAIRS_PER_QUERY / exchange_count.max(1)).max(1);
    symbols.chunks(batch_size).map(|c| c.to_vec()).collect()
}

/// Runs one query future per symbol-chunk (bounded concurrency) and flattens
/// the results. Safe to concatenate without re-sorting: each source query
/// already orders its own rows by `(symbol, exchange)`, and — since chunking
/// only splits along the symbol axis — every symbol's full row set comes from
/// exactly one chunk, so no cross-chunk merge/re-aggregation is needed.
async fn merge_chunks<T, Fut>(queries: impl Iterator<Item = Fut>) -> Result<Vec<T>>
where
    Fut: std::future::Future<Output = Result<Vec<T>>>,
{
    let mut merged = Vec::new();
    let mut stream = stream::iter(queries).buffer_unordered(MAX_CONCURRENT_CHUNKS);
    while let Some(rows) = stream.next().await {
        merged.extend(rows?);
    }
    Ok(merged)
}

// ─── Fetchers ───────────────────────────────────────────────────────────────

async fn fetch_summary(
    pool: &PgPool,
    symbols: &[String],
    exchange_ids: &[i32],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<SummaryRow>> {
    let symbol_ph = in_placeholders(3, symbols.len());
    let exchange_ph = in_placeholders(3 + symbols.len(), exchange_ids.len());

    let query = format!(
        r#"
        SELECT
            t.symbol AS symbol,
            e.name AS exchange,
            PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY t.turnover_24h)::DOUBLE PRECISION AS median_volume,
            AVG(t.turnover_24h)::DOUBLE PRECISION AS mean_volume,
            PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY t.open_interest_notional)::DOUBLE PRECISION AS median_oi,
            AVG(t.open_interest_notional)::DOUBLE PRECISION AS mean_oi
        FROM tickers t
        JOIN exchanges e ON t.exchange_id = e.id
        WHERE t.ts >= $1 AND t.ts < $2
            AND t.symbol IN ({symbol_ph})
            AND t.exchange_id IN ({exchange_ph})
            AND t.turnover_24h IS NOT NULL AND t.turnover_24h > 0
        GROUP BY t.symbol, e.id, e.name
        ORDER BY t.symbol, e.name
        "#,
        symbol_ph = symbol_ph.join(", "),
        exchange_ph = exchange_ph.join(", "),
    );

    let mut qb = sqlx::query_as::<_, SummaryRow>(&query).bind(from).bind(to);
    for s in symbols {
        qb = qb.bind(s);
    }
    for id in exchange_ids {
        qb = qb.bind(id);
    }
    qb.fetch_all(pool)
        .await
        .context("Failed to fetch summary stats")
}

#[allow(clippy::too_many_arguments)]
async fn fetch_liquidity(
    pool: &PgPool,
    symbols: &[String],
    exchange_ids: &[i32],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    bid_col: &str,
    ask_col: &str,
) -> Result<Vec<LiquidityRow>> {
    let symbol_ph = in_placeholders(3, symbols.len());
    let exchange_ph = in_placeholders(3 + symbols.len(), exchange_ids.len());

    let query = format!(
        r#"
        SELECT
            l.symbol AS symbol,
            e.name AS exchange,
            AVG(l.{bid_col})::DOUBLE PRECISION AS bid_mean,
            PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY l.{bid_col})::DOUBLE PRECISION AS bid_median,
            MAX(l.{bid_col})::DOUBLE PRECISION AS bid_max,
            STDDEV_POP(l.{bid_col})::DOUBLE PRECISION AS bid_stddev,
            AVG(l.{ask_col})::DOUBLE PRECISION AS ask_mean,
            PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY l.{ask_col})::DOUBLE PRECISION AS ask_median,
            MAX(l.{ask_col})::DOUBLE PRECISION AS ask_max,
            STDDEV_POP(l.{ask_col})::DOUBLE PRECISION AS ask_stddev
        FROM liquidity_depth l
        JOIN exchanges e ON l.exchange_id = e.id
        WHERE l.ts >= $1 AND l.ts < $2
            AND l.symbol IN ({symbol_ph})
            AND l.exchange_id IN ({exchange_ph})
        GROUP BY l.symbol, e.id, e.name
        ORDER BY l.symbol, e.name
        "#,
        bid_col = bid_col,
        ask_col = ask_col,
        symbol_ph = symbol_ph.join(", "),
        exchange_ph = exchange_ph.join(", "),
    );

    let mut qb = sqlx::query_as::<_, LiquidityRow>(&query)
        .bind(from)
        .bind(to);
    for s in symbols {
        qb = qb.bind(s);
    }
    for id in exchange_ids {
        qb = qb.bind(id);
    }
    qb.fetch_all(pool)
        .await
        .context("Failed to fetch liquidity depth stats")
}

async fn fetch_spread(
    pool: &PgPool,
    symbols: &[String],
    exchange_ids: &[i32],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<SpreadRow>> {
    let symbol_ph = in_placeholders(3, symbols.len());
    let exchange_ph = in_placeholders(3 + symbols.len(), exchange_ids.len());

    let query = format!(
        r#"
        SELECT
            o.symbol AS symbol,
            e.name AS exchange,
            AVG(o.spread_bps)::DOUBLE PRECISION AS mean_bps,
            PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY o.spread_bps)::DOUBLE PRECISION AS median_bps,
            STDDEV_POP(o.spread_bps)::DOUBLE PRECISION AS stddev_bps,
            PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY o.spread_bps)::DOUBLE PRECISION AS p95_bps,
            MAX(o.spread_bps)::DOUBLE PRECISION AS max_bps
        FROM orderbooks o
        JOIN exchanges e ON o.exchange_id = e.id
        WHERE o.ts >= $1 AND o.ts < $2
            AND o.symbol IN ({symbol_ph})
            AND o.exchange_id IN ({exchange_ph})
            AND o.spread_bps IS NOT NULL
        GROUP BY o.symbol, e.id, e.name
        ORDER BY o.symbol, e.name
        "#,
        symbol_ph = symbol_ph.join(", "),
        exchange_ph = exchange_ph.join(", "),
    );

    let mut qb = sqlx::query_as::<_, SpreadRow>(&query).bind(from).bind(to);
    for s in symbols {
        qb = qb.bind(s);
    }
    for id in exchange_ids {
        qb = qb.bind(id);
    }
    qb.fetch_all(pool)
        .await
        .context("Failed to fetch spread stats")
}

#[allow(clippy::too_many_arguments)]
async fn fetch_slippage(
    pool: &PgPool,
    symbols: &[String],
    exchange_ids: &[i32],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    trade_amount: i64,
) -> Result<Vec<SlippageRow>> {
    let symbol_ph = in_placeholders(3, symbols.len());
    let exchange_ph = in_placeholders(3 + symbols.len(), exchange_ids.len());
    let amount_idx = 3 + symbols.len() + exchange_ids.len();

    let query = format!(
        r#"
        SELECT
            s.symbol AS symbol,
            e.name AS exchange,
            AVG(s.buy_slippage_bps)::DOUBLE PRECISION AS buy_mean,
            PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY s.buy_slippage_bps)::DOUBLE PRECISION AS buy_median,
            STDDEV_POP(s.buy_slippage_bps)::DOUBLE PRECISION AS buy_stddev,
            PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY s.buy_slippage_bps)::DOUBLE PRECISION AS buy_p95,
            PERCENTILE_CONT(0.99) WITHIN GROUP (ORDER BY s.buy_slippage_bps)::DOUBLE PRECISION AS buy_p99,
            AVG(s.sell_slippage_bps)::DOUBLE PRECISION AS sell_mean,
            PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY s.sell_slippage_bps)::DOUBLE PRECISION AS sell_median,
            STDDEV_POP(s.sell_slippage_bps)::DOUBLE PRECISION AS sell_stddev,
            PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY s.sell_slippage_bps)::DOUBLE PRECISION AS sell_p95,
            PERCENTILE_CONT(0.99) WITHIN GROUP (ORDER BY s.sell_slippage_bps)::DOUBLE PRECISION AS sell_p99
        FROM slippage s
        JOIN exchanges e ON s.exchange_id = e.id
        WHERE s.ts >= $1 AND s.ts < $2
            AND s.symbol IN ({symbol_ph})
            AND s.exchange_id IN ({exchange_ph})
            AND s.trade_amount = ${amount_idx}::NUMERIC
        GROUP BY s.symbol, e.id, e.name
        ORDER BY s.symbol, e.name
        "#,
        symbol_ph = symbol_ph.join(", "),
        exchange_ph = exchange_ph.join(", "),
        amount_idx = amount_idx,
    );

    let mut qb = sqlx::query_as::<_, SlippageRow>(&query).bind(from).bind(to);
    for s in symbols {
        qb = qb.bind(s);
    }
    for id in exchange_ids {
        qb = qb.bind(id);
    }
    qb = qb.bind(trade_amount);
    qb.fetch_all(pool)
        .await
        .context("Failed to fetch slippage stats")
}

/// Fetches Binance klines once per symbol and derives three views: annualized
/// volatility, the distribution of per-candle (high-low)/low swings, and the
/// single most-swinging candle. Independent of the DB — this data doesn't
/// exist there. Best-effort per symbol: a missing/failed symbol is logged and
/// skipped rather than failing the report.
async fn fetch_binance_overview(
    symbols: &[String],
    vol_range: &str,
    vol_interval: &str,
) -> Result<(Vec<VolatilityRow>, Vec<PriceSwingRow>, Vec<MaxSwingRow>)> {
    let client = factory::get_exchange("binance")
        .await
        .context("Failed to initialize Binance client for volatility overview")?;
    let client: Arc<dyn IPerps + Send + Sync> = Arc::from(client);

    let lookback = parse_range(vol_range)?;
    let periods_per_year = volatility::periods_per_year(vol_interval)?;
    let end = Utc::now();
    let start = end - lookback;

    let fetched = stream::iter(symbols.iter().map(|symbol| {
        let client = client.clone();
        async move {
            let result = volatility::fetch_remote_klines(
                client.as_ref(),
                symbol,
                vol_interval,
                Some(start),
                Some(end),
            )
            .await;
            (symbol.clone(), result)
        }
    }))
    .buffer_unordered(MAX_CONCURRENT_VOLATILITY_REQUESTS)
    .collect::<Vec<_>>()
    .await;

    let mut volatility_rows = Vec::new();
    let mut swing_rows = Vec::new();
    let mut max_swing_rows = Vec::new();
    for (symbol, result) in fetched {
        let prices = match result {
            Ok(prices) if !prices.is_empty() => prices,
            Ok(_) => {
                tracing::warn!(symbol, "Binance returned no klines for volatility overview");
                continue;
            }
            Err(error) => {
                tracing::warn!(symbol, %error, "Failed to fetch Binance klines for volatility overview");
                continue;
            }
        };

        let bars = prices.len();
        let pct = |method| {
            volatility::calculate_volatility(&prices, method, periods_per_year)
                .0
                .map(|v| v * 100.0)
        };
        volatility_rows.push(VolatilityRow {
            symbol: symbol.clone(),
            bars,
            realized_pct: pct(VolatilityMethod::Realized),
            parkinson_pct: pct(VolatilityMethod::Parkinson),
            garman_klass_pct: pct(VolatilityMethod::GarmanKlass),
        });

        let swings = volatility::price_swings(&prices);
        if let Some(max_candle) = swings
            .iter()
            .max_by(|a, b| a.swing_pct.total_cmp(&b.swing_pct))
        {
            max_swing_rows.push(MaxSwingRow {
                symbol: symbol.clone(),
                open_time: max_candle.open_time,
                open: max_candle.open,
                high: max_candle.high,
                low: max_candle.low,
                close: max_candle.close,
                volume: max_candle.volume,
                swing_pct: max_candle.swing_pct,
            });
        }

        let mut swing_pcts: Vec<f64> = swings.iter().map(|s| s.swing_pct).collect();
        swing_pcts.sort_by(f64::total_cmp);
        let mean_val = mean(&swing_pcts);
        swing_rows.push(PriceSwingRow {
            symbol,
            bars: swing_pcts.len(),
            min: swing_pcts.first().copied(),
            mean: mean_val,
            median: percentile_cont(&swing_pcts, 0.5),
            stddev: mean_val.and_then(|m| stddev_pop(&swing_pcts, m)),
            p95: percentile_cont(&swing_pcts, 0.95),
            p99: percentile_cont(&swing_pcts, 0.99),
            max: swing_pcts.last().copied(),
        });
    }
    volatility_rows.sort_by(|a, b| a.symbol.cmp(&b.symbol));
    swing_rows.sort_by(|a, b| a.symbol.cmp(&b.symbol));
    max_swing_rows.sort_by(|a, b| a.symbol.cmp(&b.symbol));
    Ok((volatility_rows, swing_rows, max_swing_rows))
}

/// Arithmetic mean; `None` for an empty slice.
fn mean(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        None
    } else {
        Some(values.iter().sum::<f64>() / values.len() as f64)
    }
}

/// Population standard deviation (matches STDDEV_POP used elsewhere in this file).
fn stddev_pop(values: &[f64], mean_val: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let variance = values.iter().map(|v| (v - mean_val).powi(2)).sum::<f64>() / values.len() as f64;
    Some(variance.sqrt())
}

/// Linear-interpolation percentile over an already-sorted slice (matches
/// Postgres PERCENTILE_CONT used elsewhere in this file). `p` in `[0, 1]`.
fn percentile_cont(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    if sorted.len() == 1 {
        return Some(sorted[0]);
    }
    let rank = p * (sorted.len() - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    if lo == hi {
        return Some(sorted[lo]);
    }
    let frac = rank - lo as f64;
    Some(sorted[lo] + (sorted[hi] - sorted[lo]) * frac)
}

// ─── Formatting helpers ─────────────────────────────────────────────────────

/// Formats a USD value with K/M/B suffix; `None` renders as "N/A".
fn fmt_usd(value: Option<f64>) -> String {
    match value {
        None => "N/A".to_string(),
        Some(v) if v >= 1_000_000_000.0 => format!("${:.2}B", v / 1_000_000_000.0),
        Some(v) if v >= 1_000_000.0 => format!("${:.2}M", v / 1_000_000.0),
        Some(v) if v >= 1_000.0 => format!("${:.2}K", v / 1_000.0),
        Some(v) => format!("${:.2}", v),
    }
}

/// Formats a plain bps/ratio value to 2 decimals; `None` renders as "N/A".
fn fmt2(value: Option<f64>) -> String {
    match value {
        Some(v) => format!("{:.2}", v),
        None => "N/A".to_string(),
    }
}

/// Formats an annualized volatility percentage to 2 decimals; `None` renders as "N/A".
fn fmt_pct(value: Option<f64>) -> String {
    match value {
        Some(v) => format!("{:.2}%", v),
        None => "N/A".to_string(),
    }
}

/// Formats a raw per-unit price (candle OHLC), not a notional aggregate — unlike
/// `fmt_usd` this never compresses to K/M/B, since that would lose precision on
/// a single candle's exact price. Sub-$1 symbols get more decimals.
fn fmt_price(value: f64) -> String {
    if value >= 1.0 {
        format!("${:.2}", value)
    } else {
        format!("${:.6}", value)
    }
}

fn vol_oi_ratio(median_volume: Option<f64>, median_oi: Option<f64>) -> Option<f64> {
    match (median_volume, median_oi) {
        (Some(v), Some(o)) if o != 0.0 => Some(v / o),
        _ => None,
    }
}

/// Groups rows by symbol into an order-preserving map keyed by the input symbol list.
fn group_by_symbol<'a, T, F: Fn(&T) -> &str>(
    symbols: &'a [String],
    rows: &'a [T],
    key: F,
) -> BTreeMap<&'a str, Vec<&'a T>> {
    let mut map: BTreeMap<&str, Vec<&T>> =
        symbols.iter().map(|s| (s.as_str(), Vec::new())).collect();
    for row in rows {
        if let Some(bucket) = map.get_mut(key(row)) {
            bucket.push(row);
        }
    }
    map
}

// ─── Table rendering ────────────────────────────────────────────────────────

/// A GitHub-flavored markdown table (`| ... |` rows + `|---|` separator),
/// column-padded for readability in raw form.
struct MdTable {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl MdTable {
    fn new(headers: &[&str]) -> Self {
        Self {
            headers: headers.iter().map(|h| h.to_string()).collect(),
            rows: Vec::new(),
        }
    }

    fn add_row(&mut self, cells: Vec<String>) {
        self.rows.push(cells);
    }

    fn render(&self) -> String {
        let mut widths: Vec<usize> = self.headers.iter().map(|h| h.chars().count()).collect();
        for row in &self.rows {
            for (i, cell) in row.iter().enumerate() {
                widths[i] = widths[i].max(cell.chars().count());
            }
        }

        let pad = |s: &str, w: usize| format!("{:<width$}", s, width = w);
        let mut out = String::new();
        out.push_str("| ");
        out.push_str(
            &self
                .headers
                .iter()
                .zip(&widths)
                .map(|(h, w)| pad(h, *w))
                .collect::<Vec<_>>()
                .join(" | "),
        );
        out.push_str(" |\n|");
        out.push_str(
            &widths
                .iter()
                .map(|w| format!("-{}-|", "-".repeat(*w)))
                .collect::<Vec<_>>()
                .join(""),
        );
        out.push('\n');
        for row in &self.rows {
            out.push_str("| ");
            out.push_str(
                &row.iter()
                    .zip(&widths)
                    .map(|(c, w)| pad(c, *w))
                    .collect::<Vec<_>>()
                    .join(" | "),
            );
            out.push_str(" |\n");
        }
        out
    }
}

fn write_table(out: &mut String, table: &MdTable) {
    out.push_str(&table.render());
    out.push('\n');
}

/// True if `symbol` has at least one data point in any section (summary,
/// any liquidity level, spread, or any slippage trade size).
fn symbol_has_data(
    symbol: &str,
    summary_by_symbol: &BTreeMap<&str, Vec<&SummaryRow>>,
    spread_by_symbol: &BTreeMap<&str, Vec<&SpreadRow>>,
    liquidity_by_level_by_symbol: &[(&str, BTreeMap<&str, Vec<&LiquidityRow>>)],
    slippage_by_amount_by_symbol: &[(i64, BTreeMap<&str, Vec<&SlippageRow>>)],
) -> bool {
    fn non_empty<T>(v: Option<&Vec<T>>) -> bool {
        v.is_some_and(|rows| !rows.is_empty())
    }
    non_empty(summary_by_symbol.get(symbol))
        || non_empty(spread_by_symbol.get(symbol))
        || liquidity_by_level_by_symbol
            .iter()
            .any(|(_, m)| non_empty(m.get(symbol)))
        || slippage_by_amount_by_symbol
            .iter()
            .any(|(_, m)| non_empty(m.get(symbol)))
}

fn render_table(report: &ReportData) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();

    let summary_by_symbol = group_by_symbol(&report.symbols, &report.summary_rows, |r| &r.symbol);
    let spread_by_symbol = group_by_symbol(&report.symbols, &report.spread_rows, |r| &r.symbol);
    let liquidity_by_level_by_symbol: Vec<(&str, BTreeMap<&str, Vec<&LiquidityRow>>)> = report
        .liquidity_rows
        .iter()
        .map(|(label, rows)| {
            (
                *label,
                group_by_symbol(&report.symbols, rows, |r| &r.symbol),
            )
        })
        .collect();
    let slippage_by_amount_by_symbol: Vec<(i64, BTreeMap<&str, Vec<&SlippageRow>>)> = report
        .slippage_rows
        .iter()
        .map(|(amount, rows)| {
            (
                *amount,
                group_by_symbol(&report.symbols, rows, |r| &r.symbol),
            )
        })
        .collect();

    let active_symbols: Vec<&String> = report
        .symbols
        .iter()
        .filter(|s| {
            symbol_has_data(
                s,
                &summary_by_symbol,
                &spread_by_symbol,
                &liquidity_by_level_by_symbol,
                &slippage_by_amount_by_symbol,
            )
        })
        .collect();

    let exchanges_with_data: std::collections::HashSet<&str> = report
        .summary_rows
        .iter()
        .map(|r| r.exchange.as_str())
        .chain(report.spread_rows.iter().map(|r| r.exchange.as_str()))
        .chain(
            report
                .liquidity_rows
                .iter()
                .flat_map(|(_, rows)| rows.iter().map(|r| r.exchange.as_str())),
        )
        .chain(
            report
                .slippage_rows
                .iter()
                .flat_map(|(_, rows)| rows.iter().map(|r| r.exchange.as_str())),
        )
        .collect();
    let active_exchanges: Vec<&String> = report
        .exchanges
        .iter()
        .filter(|e| exchanges_with_data.contains(e.as_str()))
        .collect();

    let _ = writeln!(
        out,
        "# Report  [{} → {}]",
        report.from.format("%Y-%m-%d %H:%M UTC"),
        report.to.format("%Y-%m-%d %H:%M UTC")
    );

    // Coverage matrix: which symbol has data on which exchange, across any
    // section — lets the reader spot gaps without scanning every table below.
    let coverage: std::collections::HashSet<(&str, &str)> = report
        .summary_rows
        .iter()
        .map(|r| (r.symbol.as_str(), r.exchange.as_str()))
        .chain(
            report
                .spread_rows
                .iter()
                .map(|r| (r.symbol.as_str(), r.exchange.as_str())),
        )
        .chain(report.liquidity_rows.iter().flat_map(|(_, rows)| {
            rows.iter()
                .map(|r| (r.symbol.as_str(), r.exchange.as_str()))
        }))
        .chain(report.slippage_rows.iter().flat_map(|(_, rows)| {
            rows.iter()
                .map(|r| (r.symbol.as_str(), r.exchange.as_str()))
        }))
        .collect();

    let mut coverage_headers = vec!["Symbol".to_string()];
    coverage_headers.extend(active_exchanges.iter().map(|e| e.to_string()));
    let mut coverage_t = MdTable::new(
        &coverage_headers
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>(),
    );
    for symbol in &active_symbols {
        let mut row = vec![symbol.to_string()];
        for exchange in &active_exchanges {
            row.push(
                if coverage.contains(&(symbol.as_str(), exchange.as_str())) {
                    "✓".to_string()
                } else {
                    String::new()
                },
            );
        }
        coverage_t.add_row(row);
    }

    let _ = writeln!(out, "\n# Summary\n");
    let _ = writeln!(out, "## Coverage");
    write_table(&mut out, &coverage_t);

    let _ = writeln!(
        out,
        "\n## Volatility ({}, Binance, {})",
        report.vol_range, report.vol_interval
    );
    let mut vol_t = MdTable::new(&["Symbol", "Bars", "Realized", "Parkinson", "Garman-Klass"]);
    for row in &report.volatility_rows {
        vol_t.add_row(vec![
            row.symbol.clone(),
            row.bars.to_string(),
            fmt_pct(row.realized_pct),
            fmt_pct(row.parkinson_pct),
            fmt_pct(row.garman_klass_pct),
        ]);
    }
    write_table(&mut out, &vol_t);

    let _ = writeln!(
        out,
        "\n## Price Swing ({}, Binance, {})",
        report.vol_range, report.vol_interval
    );
    let _ = writeln!(out, "\n### Distribution");
    let mut swing_t = MdTable::new(&[
        "Symbol", "Bars", "Min", "Mean", "Median", "StdDev", "P95", "P99", "Max",
    ]);
    for row in &report.price_swing_rows {
        swing_t.add_row(vec![
            row.symbol.clone(),
            row.bars.to_string(),
            fmt_pct(row.min),
            fmt_pct(row.mean),
            fmt_pct(row.median),
            fmt_pct(row.stddev),
            fmt_pct(row.p95),
            fmt_pct(row.p99),
            fmt_pct(row.max),
        ]);
    }
    write_table(&mut out, &swing_t);

    let _ = writeln!(out, "\n### Most Swinging Candle");
    let mut max_swing_t = MdTable::new(&[
        "Symbol", "Time", "Open", "High", "Low", "Close", "Volume", "Swing",
    ]);
    for row in &report.max_swing_rows {
        max_swing_t.add_row(vec![
            row.symbol.clone(),
            row.open_time.format("%Y-%m-%d %H:%M UTC").to_string(),
            fmt_price(row.open),
            fmt_price(row.high),
            fmt_price(row.low),
            fmt_price(row.close),
            fmt_price(row.volume),
            fmt_pct(Some(row.swing_pct)),
        ]);
    }
    write_table(&mut out, &max_swing_t);

    for symbol in &active_symbols {
        let _ = writeln!(out, "\n# {}\n", symbol);

        // Summary
        let _ = writeln!(out, "## Summary");
        let mut t = MdTable::new(&[
            "Exchange",
            "Median Vol",
            "Mean Vol",
            "Median OI",
            "Mean OI",
            "Vol/OI",
        ]);
        for row in summary_by_symbol.get(symbol.as_str()).into_iter().flatten() {
            t.add_row(vec![
                row.exchange.clone(),
                fmt_usd(row.median_volume),
                fmt_usd(row.mean_volume),
                fmt_usd(row.median_oi),
                fmt_usd(row.mean_oi),
                fmt2(vol_oi_ratio(row.median_volume, row.median_oi)),
            ]);
        }
        write_table(&mut out, &t);

        // Liquidity Depth
        let _ = writeln!(out, "\n## Liquidity Depth");
        let _ = writeln!(out, "\n### Liquidity Depth — Summary (Median)");
        let mut titles = vec!["Exchange".to_string()];
        for (label, _, _) in BPS_LEVELS {
            titles.push(format!("Bid {}bps", label));
        }
        for (label, _, _) in BPS_LEVELS {
            titles.push(format!("Ask {}bps", label));
        }
        let mut summary_t = MdTable::new(&titles.iter().map(|s| s.as_str()).collect::<Vec<_>>());
        for exchange in &report.exchanges {
            let bids: Vec<Option<f64>> = liquidity_by_level_by_symbol
                .iter()
                .map(|(_, level_map)| {
                    level_map
                        .get(symbol.as_str())
                        .into_iter()
                        .flatten()
                        .find(|r| &r.exchange == exchange)
                        .and_then(|r| r.bid_median)
                })
                .collect();
            let asks: Vec<Option<f64>> = liquidity_by_level_by_symbol
                .iter()
                .map(|(_, level_map)| {
                    level_map
                        .get(symbol.as_str())
                        .into_iter()
                        .flatten()
                        .find(|r| &r.exchange == exchange)
                        .and_then(|r| r.ask_median)
                })
                .collect();
            // Skip exchanges with no liquidity data at all for this symbol,
            // rather than printing a row of N/A placeholders.
            if bids.iter().all(Option::is_none) && asks.iter().all(Option::is_none) {
                continue;
            }

            let mut cells = vec![exchange.clone()];
            for bid in &bids {
                cells.push(fmt_usd(*bid));
            }
            for ask in &asks {
                cells.push(fmt_usd(*ask));
            }
            summary_t.add_row(cells);
        }
        write_table(&mut out, &summary_t);

        for (label, level_map) in &liquidity_by_level_by_symbol {
            let _ = writeln!(out, "\n### Liquidity Depth @ {} bps", label);
            let mut lt = MdTable::new(&[
                "Exchange",
                "Bid Mean",
                "Bid Median",
                "Bid Max",
                "Bid StdDev",
                "Ask Mean",
                "Ask Median",
                "Ask Max",
                "Ask StdDev",
            ]);
            for row in level_map.get(symbol.as_str()).into_iter().flatten() {
                lt.add_row(vec![
                    row.exchange.clone(),
                    fmt_usd(row.bid_mean),
                    fmt_usd(row.bid_median),
                    fmt_usd(row.bid_max),
                    fmt_usd(row.bid_stddev),
                    fmt_usd(row.ask_mean),
                    fmt_usd(row.ask_median),
                    fmt_usd(row.ask_max),
                    fmt_usd(row.ask_stddev),
                ]);
            }
            write_table(&mut out, &lt);
        }

        // Spread
        let _ = writeln!(out, "\n## Spread (bps)");
        let mut st = MdTable::new(&["Exchange", "Mean", "Median", "StdDev", "P95", "Max"]);
        for row in spread_by_symbol.get(symbol.as_str()).into_iter().flatten() {
            st.add_row(vec![
                row.exchange.clone(),
                fmt2(row.mean_bps),
                fmt2(row.median_bps),
                fmt2(row.stddev_bps),
                fmt2(row.p95_bps),
                fmt2(row.max_bps),
            ]);
        }
        write_table(&mut out, &st);

        // Slippage
        let _ = writeln!(out, "\n## Slippage");
        for (amount, amount_map) in &slippage_by_amount_by_symbol {
            let _ = writeln!(
                out,
                "\n### Slippage — ${} order (bps)",
                format_amount(*amount)
            );
            let mut sl_t = MdTable::new(&[
                "Exchange",
                "Buy Mean",
                "Buy Median",
                "Buy StdDev",
                "Buy P95",
                "Buy P99",
                "Sell Mean",
                "Sell Median",
                "Sell StdDev",
                "Sell P95",
                "Sell P99",
            ]);
            for row in amount_map.get(symbol.as_str()).into_iter().flatten() {
                sl_t.add_row(vec![
                    row.exchange.clone(),
                    fmt2(row.buy_mean),
                    fmt2(row.buy_median),
                    fmt2(row.buy_stddev),
                    fmt2(row.buy_p95),
                    fmt2(row.buy_p99),
                    fmt2(row.sell_mean),
                    fmt2(row.sell_median),
                    fmt2(row.sell_stddev),
                    fmt2(row.sell_p95),
                    fmt2(row.sell_p99),
                ]);
            }
            write_table(&mut out, &sl_t);
        }
    }

    out
}

/// Formats a trade amount with thousands separators, e.g. 1000 -> "1,000".
fn format_amount(amount: i64) -> String {
    let s = amount.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out.chars().rev().collect()
}

// ─── JSON rendering ─────────────────────────────────────────────────────────

fn bps_json_key(label: &str) -> String {
    format!("{}bps", label.replace('.', "_"))
}

fn render_json(report: &ReportData) -> Result<String> {
    let mut per_symbol = serde_json::Map::new();

    let summary_by_symbol = group_by_symbol(&report.symbols, &report.summary_rows, |r| &r.symbol);
    let spread_by_symbol = group_by_symbol(&report.symbols, &report.spread_rows, |r| &r.symbol);

    for symbol in &report.symbols {
        let summary_json: Vec<_> = summary_by_symbol
            .get(symbol.as_str())
            .into_iter()
            .flatten()
            .map(|r| {
                json!({
                    "exchange": r.exchange,
                    "median_volume": r.median_volume,
                    "mean_volume": r.mean_volume,
                    "median_oi": r.median_oi,
                    "mean_oi": r.mean_oi,
                    "vol_oi_ratio": vol_oi_ratio(r.median_volume, r.median_oi),
                })
            })
            .collect();

        let mut liquidity_json = serde_json::Map::new();
        for (label, rows) in &report.liquidity_rows {
            let level_rows: Vec<_> = rows
                .iter()
                .filter(|r| &r.symbol == symbol)
                .map(|r| {
                    json!({
                        "exchange": r.exchange,
                        "bid_mean": r.bid_mean, "bid_median": r.bid_median,
                        "bid_max": r.bid_max, "bid_stddev": r.bid_stddev,
                        "ask_mean": r.ask_mean, "ask_median": r.ask_median,
                        "ask_max": r.ask_max, "ask_stddev": r.ask_stddev,
                    })
                })
                .collect();
            liquidity_json.insert(bps_json_key(label), json!(level_rows));
        }

        let spread_json: Vec<_> = spread_by_symbol
            .get(symbol.as_str())
            .into_iter()
            .flatten()
            .map(|r| {
                json!({
                    "exchange": r.exchange,
                    "mean_bps": r.mean_bps, "median_bps": r.median_bps,
                    "stddev_bps": r.stddev_bps, "p95_bps": r.p95_bps, "max_bps": r.max_bps,
                })
            })
            .collect();

        let mut slippage_json = serde_json::Map::new();
        for (amount, rows) in &report.slippage_rows {
            let amount_rows: Vec<_> = rows
                .iter()
                .filter(|r| &r.symbol == symbol)
                .map(|r| {
                    json!({
                        "exchange": r.exchange,
                        "buy_mean_bps": r.buy_mean, "buy_median_bps": r.buy_median,
                        "buy_stddev_bps": r.buy_stddev, "buy_p95_bps": r.buy_p95, "buy_p99_bps": r.buy_p99,
                        "sell_mean_bps": r.sell_mean, "sell_median_bps": r.sell_median,
                        "sell_stddev_bps": r.sell_stddev, "sell_p95_bps": r.sell_p95, "sell_p99_bps": r.sell_p99,
                    })
                })
                .collect();
            slippage_json.insert(amount.to_string(), json!(amount_rows));
        }

        per_symbol.insert(
            symbol.clone(),
            json!({
                "summary": summary_json,
                "liquidity": liquidity_json,
                "spread": spread_json,
                "slippage": slippage_json,
            }),
        );
    }

    let out = json!({
        "generated_at": Utc::now().to_rfc3339(),
        "from": report.from.to_rfc3339(),
        "to": report.to.to_rfc3339(),
        "symbols": report.symbols,
        "exchanges": report.exchanges,
        "report": per_symbol,
    });

    Ok(serde_json::to_string_pretty(&out)?)
}

// ─── CSV rendering ──────────────────────────────────────────────────────────

fn render_csv(report: &ReportData) -> String {
    let mut out = String::new();

    out.push_str("# Summary\n");
    out.push_str("symbol,exchange,median_volume,mean_volume,median_oi,mean_oi,vol_oi_ratio\n");
    for r in &report.summary_rows {
        out.push_str(&format!(
            "{},{},{},{},{},{},{}\n",
            r.symbol,
            r.exchange,
            csv_opt(r.median_volume),
            csv_opt(r.mean_volume),
            csv_opt(r.median_oi),
            csv_opt(r.mean_oi),
            csv_opt(vol_oi_ratio(r.median_volume, r.median_oi)),
        ));
    }

    out.push_str("\n# Liquidity Depth\n");
    out.push_str("symbol,exchange,bps_level,bid_mean,bid_median,bid_max,bid_stddev,ask_mean,ask_median,ask_max,ask_stddev\n");
    for (label, rows) in &report.liquidity_rows {
        for r in rows {
            out.push_str(&format!(
                "{},{},{},{},{},{},{},{},{},{},{}\n",
                r.symbol,
                r.exchange,
                label,
                csv_opt(r.bid_mean),
                csv_opt(r.bid_median),
                csv_opt(r.bid_max),
                csv_opt(r.bid_stddev),
                csv_opt(r.ask_mean),
                csv_opt(r.ask_median),
                csv_opt(r.ask_max),
                csv_opt(r.ask_stddev),
            ));
        }
    }

    out.push_str("\n# Spread (bps)\n");
    out.push_str("symbol,exchange,mean_bps,median_bps,stddev_bps,p95_bps,max_bps\n");
    for r in &report.spread_rows {
        out.push_str(&format!(
            "{},{},{},{},{},{},{}\n",
            r.symbol,
            r.exchange,
            csv_opt(r.mean_bps),
            csv_opt(r.median_bps),
            csv_opt(r.stddev_bps),
            csv_opt(r.p95_bps),
            csv_opt(r.max_bps),
        ));
    }

    out.push_str("\n# Slippage (bps)\n");
    out.push_str("symbol,exchange,trade_amount,buy_mean,buy_median,buy_stddev,buy_p95,buy_p99,sell_mean,sell_median,sell_stddev,sell_p95,sell_p99\n");
    for (amount, rows) in &report.slippage_rows {
        for r in rows {
            out.push_str(&format!(
                "{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
                r.symbol,
                r.exchange,
                amount,
                csv_opt(r.buy_mean),
                csv_opt(r.buy_median),
                csv_opt(r.buy_stddev),
                csv_opt(r.buy_p95),
                csv_opt(r.buy_p99),
                csv_opt(r.sell_mean),
                csv_opt(r.sell_median),
                csv_opt(r.sell_stddev),
                csv_opt(r.sell_p95),
                csv_opt(r.sell_p99),
            ));
        }
    }

    out
}

fn csv_opt(value: Option<f64>) -> String {
    value.map(|v| format!("{:.4}", v)).unwrap_or_default()
}

fn write_output(content: &str, path: &std::path::Path) -> Result<()> {
    std::fs::write(path, content)
        .with_context(|| format!("Failed to write to {}", path.display()))?;
    println!("Report written to {}", path.display());
    Ok(())
}
