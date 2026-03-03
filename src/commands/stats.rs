use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::Args;
use plotters::prelude::*;
use prettytable::{format, Cell, Row, Table};
use serde_json::json;
use sqlx::PgPool;

#[derive(Args)]
pub struct OiRateArgs {
    /// Comma-separated list of symbols (e.g., BTC,ETH). Defaults to all symbols if not specified.
    #[arg(short, long)]
    pub symbols: Option<String>,

    /// Comma-separated list of exchanges (e.g., binance,extended). Defaults to all exchanges if not specified.
    #[arg(short, long)]
    pub exchanges: Option<String>,

    /// Output format (table, json)
    #[arg(short, long, default_value = "table")]
    pub format: String,

    /// Generate a time-series plot of open interest (log-scale)
    #[arg(long, default_value_t = false)]
    pub plot: bool,

    /// Database URL (required)
    #[arg(long, env = "DATABASE_URL")]
    pub database_url: Option<String>,
}

/// OI/Volume ratio statistics
#[derive(Debug, sqlx::FromRow)]
pub struct OiVolumeStats {
    pub min_oi_vol_pct: Option<f64>,
    pub mean_oi_vol_pct: Option<f64>,
    pub median_oi_vol_pct: Option<f64>,
    pub p5_oi_vol_pct: Option<f64>,
    pub p95_oi_vol_pct: Option<f64>,
    pub p99_oi_vol_pct: Option<f64>,
    pub max_oi_vol_pct: Option<f64>,
    pub sample_count: Option<i64>,
}

/// OI/Volume ratio statistics with context
#[derive(Debug, sqlx::FromRow)]
pub struct OiVolumeStatsWithContext {
    pub symbol: Option<String>,
    pub exchange: Option<String>,
    pub min_oi_vol_pct: Option<f64>,
    pub mean_oi_vol_pct: Option<f64>,
    pub median_oi_vol_pct: Option<f64>,
    pub p5_oi_vol_pct: Option<f64>,
    pub p95_oi_vol_pct: Option<f64>,
    pub p99_oi_vol_pct: Option<f64>,
    pub max_oi_vol_pct: Option<f64>,
    pub sample_count: Option<i64>,
}

/// Time-series data point for OI/Volume ratio
#[derive(Debug, sqlx::FromRow)]
pub struct OiTimeSeriesPoint {
    pub ts: DateTime<Utc>,
    pub oi_vol_ratio: f64,
    pub label: String, // Either symbol or exchange name
}

// ─── Shared data-type helpers (used by summary, chart, hist) ─────────────────

/// Maps a CLI `--data` + `--notional` combination to the tickers column name.
///
/// | data   | notional | column                   |
/// |--------|----------|--------------------------|
/// | oi     | false    | open_interest            |
/// | oi     | true     | open_interest_notional   |
/// | volume | false    | volume_24h               |
/// | volume | true     | turnover_24h             |
fn data_type_column(data_type: &str, notional: bool) -> Result<&'static str> {
    match (data_type, notional) {
        ("oi", false) => Ok("open_interest"),
        ("oi", true) => Ok("open_interest_notional"),
        ("volume", false) => Ok("volume_24h"),
        ("volume", true) => Ok("turnover_24h"),
        (other, _) => anyhow::bail!(
            "Unknown data type '{}'. Supported values: oi, volume",
            other
        ),
    }
}

/// Maps a CLI `--data` + `--notional` combination to a human-readable label.
fn data_type_label(data_type: &str, notional: bool) -> &'static str {
    match (data_type, notional) {
        ("oi", false) => "Open Interest",
        ("oi", true) => "Open Interest Notional (USD)",
        ("volume", false) => "24h Volume",
        ("volume", true) => "24h Turnover (USD)",
        _ => "Value",
    }
}

#[derive(Args)]
pub struct SummaryArgs {
    /// Comma-separated list of symbols (e.g., BTC,ETH). Defaults to all symbols if not specified.
    #[arg(short, long)]
    pub symbols: Option<String>,

    /// Comma-separated list of exchanges (e.g., binance,bybit). Defaults to all exchanges if not specified.
    #[arg(short, long)]
    pub exchanges: Option<String>,

    /// Data to summarise (oi, volume)
    #[arg(short, long, default_value = "oi")]
    pub data: String,

    /// Use notional value: for oi → open_interest_notional; for volume → turnover_24h
    #[arg(long, default_value_t = false)]
    pub notional: bool,

    /// Output format (table, json)
    #[arg(short, long, default_value = "table")]
    pub format: String,

    /// Database URL (required)
    #[arg(long, env = "DATABASE_URL")]
    pub database_url: Option<String>,
}

/// Summary stats with symbol/exchange context
#[derive(Debug, sqlx::FromRow)]
pub struct SummaryStatsWithContext {
    pub symbol: Option<String>,
    pub exchange: Option<String>,
    pub min_oi: Option<f64>,
    pub mean_oi: Option<f64>,
    pub median_oi: Option<f64>,
    pub p5_oi: Option<f64>,
    pub p95_oi: Option<f64>,
    pub p99_oi: Option<f64>,
    pub max_oi: Option<f64>,
    pub sample_count: Option<i64>,
}

pub async fn execute_summary(args: SummaryArgs) -> Result<()> {
    let column = data_type_column(&args.data, args.notional)?;

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
        None => {
            tracing::info!("No symbols specified, fetching all symbols");
            get_all_symbol_names(&pool).await?
        }
    };

    let exchanges: Vec<String> = match args.exchanges {
        Some(ref e) => e
            .split(',')
            .map(|v| v.trim().to_lowercase())
            .filter(|v| !v.is_empty())
            .collect(),
        None => {
            tracing::info!("No exchanges specified, fetching all exchanges");
            get_all_exchange_names(&pool).await?
        }
    };

    if symbols.len() > 1 && exchanges.len() > 1 {
        anyhow::bail!(
            "Invalid combination: multiple symbols ({}) with multiple exchanges ({}) is not supported.\n\
            Use either:\n\
            - Multiple symbols with a single exchange\n\
            - Single symbol with multiple exchanges",
            symbols.len(),
            exchanges.len()
        );
    }
    if symbols.is_empty() {
        anyhow::bail!("No symbols found in database");
    }
    if exchanges.is_empty() {
        anyhow::bail!("No exchanges found in database");
    }

    let exchange_ids = get_exchange_ids(&pool, &exchanges).await?;
    if exchange_ids.is_empty() {
        anyhow::bail!("No valid exchanges found in database");
    }

    if symbols.len() > 1 {
        // Multiple symbols, single exchange — group by symbol
        let stats = fetch_summary_by_symbol(&pool, &symbols, &exchange_ids[0], column).await?;
        display_summary_by_symbol(&stats, &args.format, &exchanges[0], &args.data, args.notional)?;
    } else if exchanges.len() > 1 {
        // Single symbol, multiple exchanges — group by exchange
        let stats = fetch_summary_by_exchange(&pool, &symbols[0], &exchange_ids, column).await?;
        display_summary_by_exchange(&stats, &args.format, &symbols[0], &args.data, args.notional)?;
    } else {
        // Single symbol, single exchange — show stats for this pair
        let stats =
            fetch_summary_by_exchange(&pool, &symbols[0], &[exchange_ids[0]], column).await?;
        display_summary_by_exchange(&stats, &args.format, &symbols[0], &args.data, args.notional)?;
    }

    Ok(())
}

/// Fetch OI notional stats grouped by symbol (multiple symbols, single exchange)
async fn fetch_summary_by_symbol(
    pool: &PgPool,
    symbols: &[String],
    exchange_id: &i32,
    column: &str,
) -> Result<Vec<SummaryStatsWithContext>> {
    let placeholders: Vec<String> = (2..=symbols.len() + 1)
        .map(|i| format!("${}", i))
        .collect();

    let query = format!(
        r#"
        SELECT
            t.symbol AS symbol,
            NULL::TEXT AS exchange,
            MIN(t.{col})::DOUBLE PRECISION AS min_oi,
            AVG(t.{col})::DOUBLE PRECISION AS mean_oi,
            PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY t.{col})::DOUBLE PRECISION AS median_oi,
            PERCENTILE_CONT(0.05) WITHIN GROUP (ORDER BY t.{col})::DOUBLE PRECISION AS p5_oi,
            PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY t.{col})::DOUBLE PRECISION AS p95_oi,
            PERCENTILE_CONT(0.99) WITHIN GROUP (ORDER BY t.{col})::DOUBLE PRECISION AS p99_oi,
            MAX(t.{col})::DOUBLE PRECISION AS max_oi,
            COUNT(*)::BIGINT AS sample_count
        FROM tickers t
        WHERE t.exchange_id = $1
            AND t.symbol IN ({placeholders})
            AND t.{col} IS NOT NULL
            AND t.{col} > 0
        GROUP BY t.symbol
        ORDER BY t.symbol
        "#,
        col = column,
        placeholders = placeholders.join(", ")
    );

    let mut qb = sqlx::query_as::<_, SummaryStatsWithContext>(&query);
    qb = qb.bind(exchange_id);
    for s in symbols {
        qb = qb.bind(s);
    }

    qb.fetch_all(pool)
        .await
        .context("Failed to fetch summary stats by symbol")
}

/// Fetch summary stats grouped by exchange (single symbol, one or more exchanges)
async fn fetch_summary_by_exchange(
    pool: &PgPool,
    symbol: &str,
    exchange_ids: &[i32],
    column: &str,
) -> Result<Vec<SummaryStatsWithContext>> {
    let placeholders: Vec<String> = (2..=exchange_ids.len() + 1)
        .map(|i| format!("${}", i))
        .collect();

    let query = format!(
        r#"
        SELECT
            NULL::TEXT AS symbol,
            e.name AS exchange,
            MIN(t.{col})::DOUBLE PRECISION AS min_oi,
            AVG(t.{col})::DOUBLE PRECISION AS mean_oi,
            PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY t.{col})::DOUBLE PRECISION AS median_oi,
            PERCENTILE_CONT(0.05) WITHIN GROUP (ORDER BY t.{col})::DOUBLE PRECISION AS p5_oi,
            PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY t.{col})::DOUBLE PRECISION AS p95_oi,
            PERCENTILE_CONT(0.99) WITHIN GROUP (ORDER BY t.{col})::DOUBLE PRECISION AS p99_oi,
            MAX(t.{col})::DOUBLE PRECISION AS max_oi,
            COUNT(*)::BIGINT AS sample_count
        FROM tickers t
        JOIN exchanges e ON t.exchange_id = e.id
        WHERE t.symbol = $1
            AND t.exchange_id IN ({placeholders})
            AND t.{col} IS NOT NULL
            AND t.{col} > 0
        GROUP BY e.id, e.name
        ORDER BY e.name
        "#,
        col = column,
        placeholders = placeholders.join(", ")
    );

    let mut qb = sqlx::query_as::<_, SummaryStatsWithContext>(&query);
    qb = qb.bind(symbol);
    for id in exchange_ids {
        qb = qb.bind(id);
    }

    qb.fetch_all(pool)
        .await
        .context("Failed to fetch OI notional stats by exchange")
}

fn display_summary_by_symbol(
    stats: &[SummaryStatsWithContext],
    fmt: &str,
    exchange: &str,
    data_type: &str,
    notional: bool,
) -> Result<()> {
    let label = data_type_label(data_type, notional);
    match fmt.to_lowercase().as_str() {
        "json" => {
            let out = json!({
                "exchange": exchange,
                "data_type": label,
                "data": stats.iter().map(|s| json!({
                    "symbol": s.symbol,
                    "min": s.min_oi,
                    "mean": s.mean_oi,
                    "median": s.median_oi,
                    "p5": s.p5_oi,
                    "p95": s.p95_oi,
                    "p99": s.p99_oi,
                    "max": s.max_oi,
                    "sample_count": s.sample_count,
                })).collect::<Vec<_>>()
            });
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        _ => {
            println!("\n{} — {} Stats", exchange.to_uppercase(), label);
            let mut table = Table::new();
            table.set_format(*format::consts::FORMAT_NO_LINESEP_WITH_TITLE);
            table.set_titles(Row::new(vec![
                Cell::new("Symbol"),
                Cell::new("Min"),
                Cell::new("Mean"),
                Cell::new("Median"),
                Cell::new("P5"),
                Cell::new("P95"),
                Cell::new("P99"),
                Cell::new("Max"),
                Cell::new("Samples"),
            ]));
            for s in stats {
                table.add_row(Row::new(vec![
                    Cell::new(s.symbol.as_deref().unwrap_or("N/A")),
                    Cell::new(&format_oi_stat(s.min_oi)),
                    Cell::new(&format_oi_stat(s.mean_oi)),
                    Cell::new(&format_oi_stat(s.median_oi)),
                    Cell::new(&format_oi_stat(s.p5_oi)),
                    Cell::new(&format_oi_stat(s.p95_oi)),
                    Cell::new(&format_oi_stat(s.p99_oi)),
                    Cell::new(&format_oi_stat(s.max_oi)),
                    Cell::new(&s.sample_count.unwrap_or(0).to_string()),
                ]));
            }
            table.printstd();
        }
    }
    Ok(())
}

fn display_summary_by_exchange(
    stats: &[SummaryStatsWithContext],
    fmt: &str,
    symbol: &str,
    data_type: &str,
    notional: bool,
) -> Result<()> {
    let label = data_type_label(data_type, notional);
    match fmt.to_lowercase().as_str() {
        "json" => {
            let out = json!({
                "symbol": symbol,
                "data_type": label,
                "data": stats.iter().map(|s| json!({
                    "exchange": s.exchange,
                    "min": s.min_oi,
                    "mean": s.mean_oi,
                    "median": s.median_oi,
                    "p5": s.p5_oi,
                    "p95": s.p95_oi,
                    "p99": s.p99_oi,
                    "max": s.max_oi,
                    "sample_count": s.sample_count,
                })).collect::<Vec<_>>()
            });
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        _ => {
            println!("\n{} — {} Stats", symbol, label);
            let mut table = Table::new();
            table.set_format(*format::consts::FORMAT_NO_LINESEP_WITH_TITLE);
            table.set_titles(Row::new(vec![
                Cell::new("Exchange"),
                Cell::new("Min"),
                Cell::new("Mean"),
                Cell::new("Median"),
                Cell::new("P5"),
                Cell::new("P95"),
                Cell::new("P99"),
                Cell::new("Max"),
                Cell::new("Samples"),
            ]));
            for s in stats {
                table.add_row(Row::new(vec![
                    Cell::new(s.exchange.as_deref().unwrap_or("N/A")),
                    Cell::new(&format_oi_stat(s.min_oi)),
                    Cell::new(&format_oi_stat(s.mean_oi)),
                    Cell::new(&format_oi_stat(s.median_oi)),
                    Cell::new(&format_oi_stat(s.p5_oi)),
                    Cell::new(&format_oi_stat(s.p95_oi)),
                    Cell::new(&format_oi_stat(s.p99_oi)),
                    Cell::new(&format_oi_stat(s.max_oi)),
                    Cell::new(&s.sample_count.unwrap_or(0).to_string()),
                ]));
            }
            table.printstd();
        }
    }
    Ok(())
}

/// Format an optional OI notional value with K/M/B suffix
fn format_oi_stat(value: Option<f64>) -> String {
    match value {
        Some(v) => format_oi_value(v),
        None => "N/A".to_string(),
    }
}

#[derive(Args)]
pub struct ChartArgs {
    /// Comma-separated list of symbols (e.g., BTC,ETH). Defaults to all symbols if not specified.
    #[arg(short, long)]
    pub symbols: Option<String>,

    /// Comma-separated list of exchanges (e.g., binance,bybit). Defaults to all exchanges if not specified.
    #[arg(short, long)]
    pub exchanges: Option<String>,

    /// Data to chart (oi, volume)
    #[arg(short, long, default_value = "oi")]
    pub data: String,

    /// Use notional value: for oi → open_interest_notional; for volume → turnover_24h
    #[arg(long, default_value_t = false)]
    pub notional: bool,

    /// Use log scale for the Y-axis
    #[arg(long, default_value_t = false)]
    pub log_scale: bool,

    /// Database URL (required)
    #[arg(long, env = "DATABASE_URL")]
    pub database_url: Option<String>,
}

/// Time-series data point for a charted value
#[derive(Debug, sqlx::FromRow)]
pub struct ValuePoint {
    pub ts: DateTime<Utc>,
    pub oi_value: f64,
    pub label: String,
}

pub async fn execute_chart(args: ChartArgs) -> Result<()> {
    let column = data_type_column(&args.data, args.notional)?;

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
        None => {
            tracing::info!("No symbols specified, fetching all symbols");
            get_all_symbol_names(&pool).await?
        }
    };

    let exchanges: Vec<String> = match args.exchanges {
        Some(ref e) => e
            .split(',')
            .map(|v| v.trim().to_lowercase())
            .filter(|v| !v.is_empty())
            .collect(),
        None => {
            tracing::info!("No exchanges specified, fetching all exchanges");
            get_all_exchange_names(&pool).await?
        }
    };

    if symbols.len() > 1 && exchanges.len() > 1 {
        anyhow::bail!(
            "Invalid combination: multiple symbols ({}) with multiple exchanges ({}) is not supported.\n\
            Use either:\n\
            - Multiple symbols with a single exchange\n\
            - Single symbol with multiple exchanges\n\
            - Single symbol with single exchange",
            symbols.len(),
            exchanges.len()
        );
    }
    if symbols.is_empty() {
        anyhow::bail!("No symbols found in database");
    }
    if exchanges.is_empty() {
        anyhow::bail!("No exchanges found in database");
    }

    let exchange_ids = get_exchange_ids(&pool, &exchanges).await?;
    if exchange_ids.is_empty() {
        anyhow::bail!("No valid exchanges found in database");
    }

    if symbols.len() == 1 && exchanges.len() > 1 {
        // Single symbol, multiple exchanges — plot by exchange
        let data =
            fetch_value_timeseries_by_exchange(&pool, &symbols[0], &exchange_ids, column).await?;
        plot_value_timeseries(&data, &symbols[0], "exchange", &args.data, args.notional, args.log_scale)?;
    } else if symbols.len() > 1 && exchanges.len() == 1 {
        // Multiple symbols, single exchange — plot by symbol
        let data =
            fetch_value_timeseries_by_symbol(&pool, &symbols, &exchange_ids[0], column).await?;
        plot_value_timeseries(&data, &exchanges[0], "symbol", &args.data, args.notional, args.log_scale)?;
    } else {
        // Single symbol, single exchange — time-series for this pair
        let data =
            fetch_value_timeseries_by_exchange(&pool, &symbols[0], &[exchange_ids[0]], column)
                .await?;
        let ctx = format!("{}_{}", symbols[0], exchanges[0]);
        plot_value_timeseries(&data, &ctx, "timeseries", &args.data, args.notional, args.log_scale)?;
    }

    Ok(())
}

pub async fn execute_oi_rate(args: OiRateArgs) -> Result<()> {
    // Validate database URL
    let db_url = args.database_url.ok_or_else(|| {
        anyhow::anyhow!(
            "DATABASE_URL is required. Set via --database-url flag or DATABASE_URL environment variable"
        )
    })?;

    // Connect to database first
    tracing::info!("Connecting to database");
    let pool = PgPool::connect(&db_url)
        .await
        .context("Failed to connect to database")?;

    // Parse symbols or get all symbols if not specified
    let symbols: Vec<String> = match args.symbols {
        Some(ref symbols_str) => symbols_str
            .split(',')
            .map(|s| s.trim().to_uppercase())
            .filter(|s| !s.is_empty())
            .collect(),
        None => {
            tracing::info!("No symbols specified, fetching all symbols");
            get_all_symbol_names(&pool).await?
        }
    };

    // Parse exchanges or get all exchanges if not specified
    let exchanges: Vec<String> = match args.exchanges {
        Some(ref exchanges_str) => exchanges_str
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect(),
        None => {
            tracing::info!("No exchanges specified, fetching all exchanges");
            get_all_exchange_names(&pool).await?
        }
    };

    // Validate constraints
    if symbols.len() > 1 && exchanges.len() > 1 {
        anyhow::bail!(
            "Invalid combination: Multiple symbols ({}) with multiple exchanges ({}) is not allowed.\n\
            Use either:\n\
            - Multiple symbols with a single exchange\n\
            - Single symbol with multiple exchanges",
            symbols.len(),
            exchanges.len()
        );
    }

    if symbols.is_empty() {
        anyhow::bail!("No symbols found in database");
    }

    if exchanges.is_empty() {
        anyhow::bail!("No exchanges found in database");
    }

    // Fetch exchange IDs
    let exchange_ids = get_exchange_ids(&pool, &exchanges).await?;

    if exchange_ids.is_empty() {
        anyhow::bail!("No valid exchanges found in database");
    }

    // Execute appropriate query based on constraints
    if args.plot {
        // Generate time-series plot
        if symbols.len() == 1 && exchanges.len() > 1 {
            // Single symbol, multiple exchanges - plot by exchange
            let data = fetch_oi_timeseries_by_exchange(&pool, &symbols[0], &exchange_ids).await?;
            plot_oi_timeseries(&data, &symbols[0], "exchange")?;
        } else if symbols.len() > 1 && exchanges.len() == 1 {
            // Multiple symbols, single exchange - plot by symbol
            let data = fetch_oi_timeseries_by_symbol(&pool, &symbols, &exchange_ids[0]).await?;
            plot_oi_timeseries(&data, &exchanges[0], "symbol")?;
        } else if symbols.len() == 1 && exchanges.len() == 1 {
            // Single symbol, single exchange - plot time-series for this pair
            let data = fetch_oi_timeseries_by_exchange(&pool, &symbols[0], &[exchange_ids[0]]).await?;
            let filename_context = format!("{}_{}", symbols[0], exchanges[0]);
            plot_oi_timeseries(&data, &filename_context, "timeseries")?;
        } else {
            // Multiple symbols, multiple exchanges - not allowed
            anyhow::bail!(
                "Invalid combination: Multiple symbols ({}) with multiple exchanges ({}) is not allowed.\n\
                Use either:\n\
                - Multiple symbols with a single exchange\n\
                - Single symbol with multiple exchanges\n\
                - Single symbol with single exchange",
                symbols.len(),
                exchanges.len()
            );
        }
    } else {
        // Display statistics
        if symbols.len() == 1 && exchanges.len() > 1 {
            // Single symbol, multiple exchanges - group by exchange
            let stats = fetch_oi_rate_by_exchange(&pool, &symbols[0], &exchange_ids).await?;
            display_stats_by_exchange(&stats, &args.format, &symbols[0])?;
        } else if symbols.len() > 1 && exchanges.len() == 1 {
            // Multiple symbols, single exchange - group by symbol
            let stats = fetch_oi_rate_by_symbol(&pool, &symbols, &exchange_ids[0]).await?;
            display_stats_by_symbol(&stats, &args.format, &exchanges[0])?;
        } else {
            // Single symbol, single exchange - overall stats
            let stats = fetch_oi_rate_overall(&pool, &symbols[0], &exchange_ids[0]).await?;
            display_stats_overall(&stats, &args.format, &symbols[0], &exchanges[0])?;
        }
    }

    Ok(())
}

/// Get all unique symbol names from database
async fn get_all_symbol_names(pool: &PgPool) -> Result<Vec<String>> {
    let names = sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT symbol FROM tickers ORDER BY symbol"
    )
    .fetch_all(pool)
    .await
    .context("Failed to fetch symbol names")?;

    Ok(names)
}

/// Get all exchange names from database
async fn get_all_exchange_names(pool: &PgPool) -> Result<Vec<String>> {
    let names = sqlx::query_scalar::<_, String>("SELECT name FROM exchanges ORDER BY name")
        .fetch_all(pool)
        .await
        .context("Failed to fetch exchange names")?;

    Ok(names)
}

/// Get exchange IDs from names
async fn get_exchange_ids(pool: &PgPool, exchanges: &[String]) -> Result<Vec<i32>> {
    let placeholders: Vec<String> = (1..=exchanges.len())
        .map(|i| format!("${}", i))
        .collect();
    let query = format!(
        "SELECT id FROM exchanges WHERE LOWER(name) IN ({})",
        placeholders.join(", ")
    );

    let mut query_builder = sqlx::query_scalar::<_, i32>(&query);
    for exchange in exchanges {
        query_builder = query_builder.bind(exchange.to_lowercase());
    }

    let ids = query_builder
        .fetch_all(pool)
        .await
        .context("Failed to fetch exchange IDs")?;

    Ok(ids)
}

/// Fetch OI/Volume ratio stats for a single symbol across multiple exchanges
async fn fetch_oi_rate_by_exchange(
    pool: &PgPool,
    symbol: &str,
    exchange_ids: &[i32],
) -> Result<Vec<OiVolumeStatsWithContext>> {
    let placeholders: Vec<String> = (2..=exchange_ids.len() + 1)
        .map(|i| format!("${}", i))
        .collect();

    let query = format!(
        r#"
        SELECT
            e.name AS exchange,
            NULL::TEXT AS symbol,
            ROUND((MIN(oi_vol_ratio) * 100)::NUMERIC, 2)::DOUBLE PRECISION AS min_oi_vol_pct,
            ROUND((AVG(oi_vol_ratio) * 100)::NUMERIC, 2)::DOUBLE PRECISION AS mean_oi_vol_pct,
            ROUND((PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY oi_vol_ratio) * 100)::NUMERIC, 2)::DOUBLE PRECISION AS median_oi_vol_pct,
            ROUND((PERCENTILE_CONT(0.05) WITHIN GROUP (ORDER BY oi_vol_ratio) * 100)::NUMERIC, 2)::DOUBLE PRECISION AS p5_oi_vol_pct,
            ROUND((PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY oi_vol_ratio) * 100)::NUMERIC, 2)::DOUBLE PRECISION AS p95_oi_vol_pct,
            ROUND((PERCENTILE_CONT(0.99) WITHIN GROUP (ORDER BY oi_vol_ratio) * 100)::NUMERIC, 2)::DOUBLE PRECISION AS p99_oi_vol_pct,
            ROUND((MAX(oi_vol_ratio) * 100)::NUMERIC, 2)::DOUBLE PRECISION AS max_oi_vol_pct,
            COUNT(*)::BIGINT AS sample_count
        FROM (
            SELECT
                t.exchange_id,
                open_interest / NULLIF(volume_24h, 0) AS oi_vol_ratio
            FROM tickers t
            WHERE t.symbol = $1
                AND t.exchange_id IN ({})
                AND t.volume_24h > 0
                AND t.open_interest IS NOT NULL
                AND t.volume_24h IS NOT NULL
                AND t.open_interest > 0
        ) AS ratios
        JOIN exchanges e ON ratios.exchange_id = e.id
        WHERE oi_vol_ratio IS NOT NULL
        GROUP BY e.name, e.id
        ORDER BY e.name
        "#,
        placeholders.join(", ")
    );

    let mut query_builder = sqlx::query_as::<_, OiVolumeStatsWithContext>(&query);
    query_builder = query_builder.bind(symbol);
    for exchange_id in exchange_ids {
        query_builder = query_builder.bind(exchange_id);
    }

    let stats = query_builder
        .fetch_all(pool)
        .await
        .context("Failed to fetch OI/Volume ratio stats by exchange")?;

    Ok(stats)
}

/// Fetch OI/Volume ratio stats for multiple symbols on a single exchange
async fn fetch_oi_rate_by_symbol(
    pool: &PgPool,
    symbols: &[String],
    exchange_id: &i32,
) -> Result<Vec<OiVolumeStatsWithContext>> {
    let placeholders: Vec<String> = (2..=symbols.len() + 1)
        .map(|i| format!("${}", i))
        .collect();

    let query = format!(
        r#"
        SELECT
            NULL::TEXT AS exchange,
            ratios.symbol AS symbol,
            ROUND((MIN(oi_vol_ratio) * 100)::NUMERIC, 2)::DOUBLE PRECISION AS min_oi_vol_pct,
            ROUND((AVG(oi_vol_ratio) * 100)::NUMERIC, 2)::DOUBLE PRECISION AS mean_oi_vol_pct,
            ROUND((PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY oi_vol_ratio) * 100)::NUMERIC, 2)::DOUBLE PRECISION AS median_oi_vol_pct,
            ROUND((PERCENTILE_CONT(0.05) WITHIN GROUP (ORDER BY oi_vol_ratio) * 100)::NUMERIC, 2)::DOUBLE PRECISION AS p5_oi_vol_pct,
            ROUND((PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY oi_vol_ratio) * 100)::NUMERIC, 2)::DOUBLE PRECISION AS p95_oi_vol_pct,
            ROUND((PERCENTILE_CONT(0.99) WITHIN GROUP (ORDER BY oi_vol_ratio) * 100)::NUMERIC, 2)::DOUBLE PRECISION AS p99_oi_vol_pct,
            ROUND((MAX(oi_vol_ratio) * 100)::NUMERIC, 2)::DOUBLE PRECISION AS max_oi_vol_pct,
            COUNT(*)::BIGINT AS sample_count
        FROM (
            SELECT
                t.symbol,
                open_interest / NULLIF(volume_24h, 0) AS oi_vol_ratio
            FROM tickers t
            WHERE t.exchange_id = $1
                AND t.symbol IN ({})
                AND t.volume_24h > 0
                AND t.open_interest IS NOT NULL
                AND t.volume_24h IS NOT NULL
                AND t.open_interest > 0
        ) AS ratios
        WHERE oi_vol_ratio IS NOT NULL
        GROUP BY ratios.symbol
        ORDER BY ratios.symbol
        "#,
        placeholders.join(", ")
    );

    let mut query_builder = sqlx::query_as::<_, OiVolumeStatsWithContext>(&query);
    query_builder = query_builder.bind(exchange_id);
    for symbol in symbols {
        query_builder = query_builder.bind(symbol);
    }

    let stats = query_builder
        .fetch_all(pool)
        .await
        .context("Failed to fetch OI/Volume ratio stats by symbol")?;

    Ok(stats)
}

/// Fetch overall OI/Volume ratio stats for a single symbol on a single exchange
async fn fetch_oi_rate_overall(
    pool: &PgPool,
    symbol: &str,
    exchange_id: &i32,
) -> Result<OiVolumeStats> {
    let stats = sqlx::query_as::<_, OiVolumeStats>(
        r#"
        SELECT
            ROUND((MIN(oi_vol_ratio) * 100)::NUMERIC, 2)::DOUBLE PRECISION AS min_oi_vol_pct,
            ROUND((AVG(oi_vol_ratio) * 100)::NUMERIC, 2)::DOUBLE PRECISION AS mean_oi_vol_pct,
            ROUND((PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY oi_vol_ratio) * 100)::NUMERIC, 2)::DOUBLE PRECISION AS median_oi_vol_pct,
            ROUND((PERCENTILE_CONT(0.05) WITHIN GROUP (ORDER BY oi_vol_ratio) * 100)::NUMERIC, 2)::DOUBLE PRECISION AS p5_oi_vol_pct,
            ROUND((PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY oi_vol_ratio) * 100)::NUMERIC, 2)::DOUBLE PRECISION AS p95_oi_vol_pct,
            ROUND((PERCENTILE_CONT(0.99) WITHIN GROUP (ORDER BY oi_vol_ratio) * 100)::NUMERIC, 2)::DOUBLE PRECISION AS p99_oi_vol_pct,
            ROUND((MAX(oi_vol_ratio) * 100)::NUMERIC, 2)::DOUBLE PRECISION AS max_oi_vol_pct,
            COUNT(*)::BIGINT AS sample_count
        FROM (
            SELECT
                open_interest / NULLIF(volume_24h, 0) AS oi_vol_ratio
            FROM tickers
            WHERE exchange_id = $1
                AND symbol = $2
                AND volume_24h > 0
                AND open_interest IS NOT NULL
                AND volume_24h IS NOT NULL
                AND open_interest > 0
        ) AS ratios
        WHERE oi_vol_ratio IS NOT NULL
        "#,
    )
    .bind(exchange_id)
    .bind(symbol)
    .fetch_one(pool)
    .await
    .context("Failed to fetch overall OI/Volume ratio stats")?;

    Ok(stats)
}

/// Display stats grouped by exchange (single symbol, multiple exchanges)
fn display_stats_by_exchange(
    stats: &[OiVolumeStatsWithContext],
    format: &str,
    symbol: &str,
) -> Result<()> {
    match format.to_lowercase().as_str() {
        "json" => {
            let json_output = json!({
                "symbol": symbol,
                "title": format!("{} OI-to-Volume Ratio Summary", symbol),
                "data": stats
                    .iter()
                    .map(|s| {
                        json!({
                            "exchange": s.exchange,
                            "min": s.min_oi_vol_pct,
                            "mean": s.mean_oi_vol_pct,
                            "median": s.median_oi_vol_pct,
                            "p5": s.p5_oi_vol_pct,
                            "p95": s.p95_oi_vol_pct,
                            "p99": s.p99_oi_vol_pct,
                            "max": s.max_oi_vol_pct,
                            "sample_count": s.sample_count,
                        })
                    })
                    .collect::<Vec<_>>()
            });
            println!("{}", serde_json::to_string_pretty(&json_output)?);
        }
        _ => {
            let mut table = Table::new();
            table.set_format(*format::consts::FORMAT_NO_LINESEP_WITH_TITLE);
            table.set_titles(Row::new(vec![
                Cell::new("Exchange"),
                Cell::new("Min %"),
                Cell::new("Mean %"),
                Cell::new("Median %"),
                Cell::new("P5 %"),
                Cell::new("P95 %"),
                Cell::new("P99 %"),
                Cell::new("Max %"),
                Cell::new("Samples"),
            ]));

            for stat in stats {
                table.add_row(Row::new(vec![
                    Cell::new(&stat.exchange.as_deref().unwrap_or("N/A")),
                    Cell::new(&format_number(stat.min_oi_vol_pct)),
                    Cell::new(&format_number(stat.mean_oi_vol_pct)),
                    Cell::new(&format_number(stat.median_oi_vol_pct)),
                    Cell::new(&format_number(stat.p5_oi_vol_pct)),
                    Cell::new(&format_number(stat.p95_oi_vol_pct)),
                    Cell::new(&format_number(stat.p99_oi_vol_pct)),
                    Cell::new(&format_number(stat.max_oi_vol_pct)),
                    Cell::new(&stat.sample_count.unwrap_or(0).to_string()),
                ]));
            }

            println!("\n{} OI-to-Volume Ratio Summary", symbol);
            table.printstd();
        }
    }

    Ok(())
}

/// Display stats grouped by symbol (multiple symbols, single exchange)
fn display_stats_by_symbol(
    stats: &[OiVolumeStatsWithContext],
    format: &str,
    exchange: &str,
) -> Result<()> {
    match format.to_lowercase().as_str() {
        "json" => {
            let json_output = json!({
                "exchange": exchange,
                "title": format!("{} OI-to-Volume Ratio Summary", exchange.to_uppercase()),
                "data": stats
                    .iter()
                    .map(|s| {
                        json!({
                            "symbol": s.symbol,
                            "min": s.min_oi_vol_pct,
                            "mean": s.mean_oi_vol_pct,
                            "median": s.median_oi_vol_pct,
                            "p5": s.p5_oi_vol_pct,
                            "p95": s.p95_oi_vol_pct,
                            "p99": s.p99_oi_vol_pct,
                            "max": s.max_oi_vol_pct,
                            "sample_count": s.sample_count,
                        })
                    })
                    .collect::<Vec<_>>()
            });
            println!("{}", serde_json::to_string_pretty(&json_output)?);
        }
        _ => {
            // Print caption
            let mut table = Table::new();
            table.set_format(*format::consts::FORMAT_NO_LINESEP_WITH_TITLE);
            table.set_titles(Row::new(vec![
                Cell::new("Symbol"),
                Cell::new("Min %"),
                Cell::new("Mean %"),
                Cell::new("Median %"),
                Cell::new("P5 %"),
                Cell::new("P95 %"),
                Cell::new("P99 %"),
                Cell::new("Max %"),
                Cell::new("Samples"),
            ]));

            for stat in stats {
                table.add_row(Row::new(vec![
                    Cell::new(&stat.symbol.as_deref().unwrap_or("N/A")),
                    Cell::new(&format_number(stat.min_oi_vol_pct)),
                    Cell::new(&format_number(stat.mean_oi_vol_pct)),
                    Cell::new(&format_number(stat.median_oi_vol_pct)),
                    Cell::new(&format_number(stat.p5_oi_vol_pct)),
                    Cell::new(&format_number(stat.p95_oi_vol_pct)),
                    Cell::new(&format_number(stat.p99_oi_vol_pct)),
                    Cell::new(&format_number(stat.max_oi_vol_pct)),
                    Cell::new(&stat.sample_count.unwrap_or(0).to_string()),
                ]));
            }

            println!("\n{} OI-to-Volume Ratio Summary", exchange.to_uppercase());
            table.printstd();
        }
    }

    Ok(())
}

/// Display overall stats (single symbol, single exchange)
fn display_stats_overall(
    stats: &OiVolumeStats,
    format: &str,
    symbol: &str,
    exchange: &str,
) -> Result<()> {
    match format.to_lowercase().as_str() {
        "json" => {
            let json_output = json!({
                "title": format!("{} on {} - OI-to-Volume Ratio Summary", symbol, exchange.to_uppercase()),
                "symbol": symbol,
                "exchange": exchange,
                "min": stats.min_oi_vol_pct,
                "mean": stats.mean_oi_vol_pct,
                "median": stats.median_oi_vol_pct,
                "p5": stats.p5_oi_vol_pct,
                "p95": stats.p95_oi_vol_pct,
                "p99": stats.p99_oi_vol_pct,
                "max": stats.max_oi_vol_pct,
                "sample_count": stats.sample_count,
            });
            println!("{}", serde_json::to_string_pretty(&json_output)?);
        }
        _ => {
            // Print caption
            println!("\n{} on {} - OI-to-Volume Ratio Summary", symbol, exchange.to_uppercase());
            println!("{}", "=".repeat(60));

            let mut table = Table::new();
            table.set_format(*format::consts::FORMAT_NO_LINESEP_WITH_TITLE);
            table.set_titles(Row::new(vec![
                Cell::new("Metric"),
                Cell::new("Value (%)"),
            ]));

            table.add_row(Row::new(vec![
                Cell::new("Symbol"),
                Cell::new(symbol),
            ]));
            table.add_row(Row::new(vec![
                Cell::new("Exchange"),
                Cell::new(exchange),
            ]));
            table.add_row(Row::new(vec![
                Cell::new("Min"),
                Cell::new(&format_number(stats.min_oi_vol_pct)),
            ]));
            table.add_row(Row::new(vec![
                Cell::new("Mean"),
                Cell::new(&format_number(stats.mean_oi_vol_pct)),
            ]));
            table.add_row(Row::new(vec![
                Cell::new("Median"),
                Cell::new(&format_number(stats.median_oi_vol_pct)),
            ]));
            table.add_row(Row::new(vec![
                Cell::new("P5"),
                Cell::new(&format_number(stats.p5_oi_vol_pct)),
            ]));
            table.add_row(Row::new(vec![
                Cell::new("P95"),
                Cell::new(&format_number(stats.p95_oi_vol_pct)),
            ]));
            table.add_row(Row::new(vec![
                Cell::new("P99"),
                Cell::new(&format_number(stats.p99_oi_vol_pct)),
            ]));
            table.add_row(Row::new(vec![
                Cell::new("Max"),
                Cell::new(&format_number(stats.max_oi_vol_pct)),
            ]));
            table.add_row(Row::new(vec![
                Cell::new("Samples"),
                Cell::new(&stats.sample_count.unwrap_or(0).to_string()),
            ]));

            table.printstd();
            println!(); // Add newline after table
        }
    }

    Ok(())
}

/// Fetch time-series OI/Volume ratio data for a single symbol across multiple exchanges
async fn fetch_oi_timeseries_by_exchange(
    pool: &PgPool,
    symbol: &str,
    exchange_ids: &[i32],
) -> Result<Vec<OiTimeSeriesPoint>> {
    let placeholders: Vec<String> = (2..=exchange_ids.len() + 1)
        .map(|i| format!("${}", i))
        .collect();

    let query = format!(
        r#"
        SELECT
            t.ts AS ts,
            (t.open_interest * 100 / NULLIF(t.volume_24h, 0))::DOUBLE PRECISION AS oi_vol_ratio,
            e.name AS label
        FROM tickers t
        JOIN exchanges e ON t.exchange_id = e.id
        WHERE t.symbol = $1
            AND t.exchange_id IN ({})
            AND t.open_interest IS NOT NULL
            AND t.volume_24h IS NOT NULL
            AND t.open_interest > 0
            AND t.volume_24h > 0
        ORDER BY t.ts, e.name
        "#,
        placeholders.join(", ")
    );

    let mut query_builder = sqlx::query_as::<_, OiTimeSeriesPoint>(&query);
    query_builder = query_builder.bind(symbol);
    for exchange_id in exchange_ids {
        query_builder = query_builder.bind(exchange_id);
    }

    let data = query_builder
        .fetch_all(pool)
        .await
        .context("Failed to fetch OI time-series data by exchange")?;

    Ok(data)
}

/// Fetch time-series OI/Volume ratio data for multiple symbols on a single exchange
async fn fetch_oi_timeseries_by_symbol(
    pool: &PgPool,
    symbols: &[String],
    exchange_id: &i32,
) -> Result<Vec<OiTimeSeriesPoint>> {
    let placeholders: Vec<String> = (2..=symbols.len() + 1)
        .map(|i| format!("${}", i))
        .collect();

    let query = format!(
        r#"
        SELECT
            t.ts AS ts,
            (t.open_interest * 100 / NULLIF(t.volume_24h, 0))::DOUBLE PRECISION AS oi_vol_ratio,
            t.symbol AS label
        FROM tickers t
        WHERE t.exchange_id = $1
            AND t.symbol IN ({})
            AND t.open_interest IS NOT NULL
            AND t.volume_24h IS NOT NULL
            AND t.open_interest > 0
            AND t.volume_24h > 0
        ORDER BY t.ts, t.symbol
        "#,
        placeholders.join(", ")
    );

    let mut query_builder = sqlx::query_as::<_, OiTimeSeriesPoint>(&query);
    query_builder = query_builder.bind(exchange_id);
    for symbol in symbols {
        query_builder = query_builder.bind(symbol);
    }

    let data = query_builder
        .fetch_all(pool)
        .await
        .context("Failed to fetch OI time-series data by symbol")?;

    Ok(data)
}

/// Get a consistent color for a given label (exchange or symbol)
/// Uses a high-contrast palette optimized for data visualization
fn get_color_for_label(label: &str) -> RGBColor {
    // Deterministic color mapping with maximum contrast
    match label.to_lowercase().as_str() {
        "binance" => RGBColor(255, 127, 14),    // Vivid Orange
        "extended" => RGBColor(214, 39, 40),    // Strong Red
        "hyperliquid" => RGBColor(44, 160, 44), // Deep Green
        "nadex" => RGBColor(31, 119, 180),      // Deep Blue
        "pacifica" => RGBColor(23, 190, 207),   // Bright Cyan
        "lighter" => RGBColor(148, 103, 189),   // Medium Purple
        "bybit" => RGBColor(227, 119, 194),     // Pink
        "okx" => RGBColor(188, 189, 34),        // Yellow-Green
        "deribit" => RGBColor(140, 86, 75),     // Brown
        "btc" => RGBColor(247, 147, 26),        // Bitcoin Orange
        "eth" => RGBColor(98, 126, 234),        // Ethereum Blue
        "sol" => RGBColor(220, 31, 255),        // Solana Purple
        "arb" => RGBColor(40, 160, 240),        // Arbitrum Blue
        "avax" => RGBColor(232, 65, 66),        // Avalanche Red
        "op" => RGBColor(255, 4, 32),           // Optimism Red
        _ => {
            // For unknown labels, use tableau color palette rotation
            let hash = label.bytes().fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
            let colors = [
                RGBColor(31, 119, 180),   // Blue
                RGBColor(255, 127, 14),   // Orange
                RGBColor(44, 160, 44),    // Green
                RGBColor(214, 39, 40),    // Red
                RGBColor(148, 103, 189),  // Purple
                RGBColor(140, 86, 75),    // Brown
                RGBColor(227, 119, 194),  // Pink
                RGBColor(127, 127, 127),  // Gray
                RGBColor(188, 189, 34),   // Yellow-Green
                RGBColor(23, 190, 207),   // Cyan
            ];
            colors[(hash as usize) % colors.len()]
        }
    }
}

/// Plot OI/Volume ratio time-series data with log-scale
fn plot_oi_timeseries(
    data: &[OiTimeSeriesPoint],
    context: &str,
    grouping: &str,
) -> Result<()> {
    if data.is_empty() {
        anyhow::bail!("No data to plot");
    }

    // Group data by label
    use std::collections::HashMap;
    let mut series: HashMap<String, Vec<(DateTime<Utc>, f64)>> = HashMap::new();

    for point in data {
        series
            .entry(point.label.clone())
            .or_insert_with(Vec::new)
            .push((point.ts, point.oi_vol_ratio));
    }

    // Find min and max values for axes
    let min_ts = data.iter().map(|p| p.ts).min().unwrap();
    let max_ts = data.iter().map(|p| p.ts).max().unwrap();

    let min_ratio = data
        .iter()
        .map(|p| p.oi_vol_ratio)
        .filter(|&v| v > 0.0)
        .min_by(|a, b| a.partial_cmp(b).unwrap())
        .unwrap_or(0.01);
    let max_ratio = data
        .iter()
        .map(|p| p.oi_vol_ratio)
        .max_by(|a, b| a.partial_cmp(b).unwrap())
        .unwrap_or(1.0);

    // Create output directory if it doesn't exist
    std::fs::create_dir_all("out").context("Failed to create output directory")?;

    // Create output filename
    let filename = format!("out/oi_ratio_plot_{}_{}.png", context, grouping);

    // Create the plot
    let root = BitMapBackend::new(&filename, (1200, 800)).into_drawing_area();
    root.fill(&WHITE)?;

    let title = format!(
        "OI-to-Volume Ratio Over Time - {} (by {})",
        context.to_uppercase(),
        grouping
    );

    let mut chart = ChartBuilder::on(&root)
        .caption(&title, ("sans-serif", 30).into_font())
        .margin(10)
        .x_label_area_size(40)
        .y_label_area_size(80)
        .build_cartesian_2d(min_ts..max_ts, ((min_ratio * 0.5)..(max_ratio * 1.5)).log_scale())?;

    chart
        .configure_mesh()
        .x_desc("Date")
        .y_desc("OI/Volume Ratio (Log Scale)")
        .x_label_formatter(&|x| x.format("%Y-%m-%d").to_string())
        .draw()?;

    // Sort series by label for consistent ordering
    let mut sorted_series: Vec<_> = series.iter().collect();
    sorted_series.sort_by(|a, b| a.0.cmp(b.0));

    // Plot each series with deterministic colors
    for (label, points) in sorted_series {
        let color = get_color_for_label(label);

        chart
            .draw_series(LineSeries::new(
                points.iter().map(|(ts, oi)| (*ts, *oi)),
                &color,
            ))?
            .label(label)
            .legend(move |(x, y)| PathElement::new(vec![(x, y), (x + 20, y)], &color));
    }

    chart
        .configure_series_labels()
        .position(SeriesLabelPosition::UpperRight)
        .background_style(&WHITE.mix(0.8))
        .border_style(&BLACK)
        .draw()?;

    root.present()?;

    println!("\n✓ Plot saved to: {}", filename);

    Ok(())
}

/// Format optional number for display
fn format_number(value: Option<f64>) -> String {
    match value {
        Some(v) => format!("{:.2}", v),
        None => "N/A".to_string(),
    }
}

/// Fetch a value time-series for a single symbol across multiple exchanges.
async fn fetch_value_timeseries_by_exchange(
    pool: &PgPool,
    symbol: &str,
    exchange_ids: &[i32],
    column: &str,
) -> Result<Vec<ValuePoint>> {
    let placeholders: Vec<String> = (2..=exchange_ids.len() + 1)
        .map(|i| format!("${}", i))
        .collect();

    let query = format!(
        r#"
        SELECT
            t.ts AS ts,
            t.{col}::DOUBLE PRECISION AS oi_value,
            e.name AS label
        FROM tickers t
        JOIN exchanges e ON t.exchange_id = e.id
        WHERE t.symbol = $1
            AND t.exchange_id IN ({placeholders})
            AND t.{col} IS NOT NULL
            AND t.{col} > 0
        ORDER BY t.ts, e.name
        "#,
        col = column,
        placeholders = placeholders.join(", ")
    );

    let mut qb = sqlx::query_as::<_, ValuePoint>(&query);
    qb = qb.bind(symbol);
    for id in exchange_ids {
        qb = qb.bind(id);
    }

    qb.fetch_all(pool)
        .await
        .context("Failed to fetch value time-series by exchange")
}

/// Fetch a value time-series for multiple symbols on a single exchange.
async fn fetch_value_timeseries_by_symbol(
    pool: &PgPool,
    symbols: &[String],
    exchange_id: &i32,
    column: &str,
) -> Result<Vec<ValuePoint>> {
    let placeholders: Vec<String> = (2..=symbols.len() + 1)
        .map(|i| format!("${}", i))
        .collect();

    let query = format!(
        r#"
        SELECT
            t.ts AS ts,
            t.{col}::DOUBLE PRECISION AS oi_value,
            t.symbol AS label
        FROM tickers t
        WHERE t.exchange_id = $1
            AND t.symbol IN ({placeholders})
            AND t.{col} IS NOT NULL
            AND t.{col} > 0
        ORDER BY t.ts, t.symbol
        "#,
        col = column,
        placeholders = placeholders.join(", ")
    );

    let mut qb = sqlx::query_as::<_, ValuePoint>(&query);
    qb = qb.bind(exchange_id);
    for s in symbols {
        qb = qb.bind(s);
    }

    qb.fetch_all(pool)
        .await
        .context("Failed to fetch value time-series by symbol")
}

/// Format a raw OI USD value with K/M/B suffix for axis labels
fn format_oi_value(value: f64) -> String {
    if value >= 1_000_000_000.0 {
        format!("{:.1}B", value / 1_000_000_000.0)
    } else if value >= 1_000_000.0 {
        format!("{:.1}M", value / 1_000_000.0)
    } else if value >= 1_000.0 {
        format!("{:.1}K", value / 1_000.0)
    } else {
        format!("{:.0}", value)
    }
}

/// Plot a data value as a time-series line chart.
fn plot_value_timeseries(
    data: &[ValuePoint],
    context: &str,
    grouping: &str,
    data_type: &str,
    notional: bool,
    log_scale: bool,
) -> Result<()> {
    if data.is_empty() {
        anyhow::bail!("No data to plot");
    }

    use std::collections::HashMap;
    let mut series: HashMap<String, Vec<(DateTime<Utc>, f64)>> = HashMap::new();
    for point in data {
        series
            .entry(point.label.clone())
            .or_default()
            .push((point.ts, point.oi_value));
    }

    let min_ts = data.iter().map(|p| p.ts).min().unwrap();
    let max_ts = data.iter().map(|p| p.ts).max().unwrap();
    let min_oi = data
        .iter()
        .map(|p| p.oi_value)
        .filter(|&v| v > 0.0)
        .min_by(|a, b| a.partial_cmp(b).unwrap())
        .unwrap_or(1.0);
    let max_oi = data
        .iter()
        .map(|p| p.oi_value)
        .max_by(|a, b| a.partial_cmp(b).unwrap())
        .unwrap_or(1.0);

    std::fs::create_dir_all("out").context("Failed to create output directory")?;
    let filename = format!("out/chart_{}_{}_{}.png", data_type, context, grouping);

    let root = BitMapBackend::new(&filename, (1200, 800)).into_drawing_area();
    root.fill(&WHITE)?;

    let value_label = data_type_label(data_type, notional);
    let title = format!(
        "{} Over Time — {} (by {})",
        value_label,
        context.to_uppercase(),
        grouping
    );

    let mut sorted_series: Vec<_> = series.iter().collect();
    sorted_series.sort_by(|a, b| a.0.cmp(b.0));

    if log_scale {
        let mut chart = ChartBuilder::on(&root)
            .caption(&title, ("sans-serif", 30).into_font())
            .margin(10)
            .x_label_area_size(40)
            .y_label_area_size(90)
            .build_cartesian_2d(
                min_ts..max_ts,
                ((min_oi * 0.5)..(max_oi * 2.0)).log_scale(),
            )?;

        chart
            .configure_mesh()
            .x_desc("Date")
            .y_desc(format!("{} [Log Scale]", value_label).as_str())
            .x_label_formatter(&|x| x.format("%Y-%m-%d").to_string())
            .y_label_formatter(&|y| format_oi_value(*y))
            .draw()?;

        for (label, points) in &sorted_series {
            let color = get_color_for_label(label);
            chart
                .draw_series(LineSeries::new(
                    points.iter().map(|(ts, v)| (*ts, *v)),
                    &color,
                ))?
                .label(label.as_str())
                .legend(move |(x, y)| PathElement::new(vec![(x, y), (x + 20, y)], &color));
        }

        chart
            .configure_series_labels()
            .position(SeriesLabelPosition::UpperRight)
            .background_style(&WHITE.mix(0.8))
            .border_style(&BLACK)
            .draw()?;
    } else {
        let mut chart = ChartBuilder::on(&root)
            .caption(&title, ("sans-serif", 30).into_font())
            .margin(10)
            .x_label_area_size(40)
            .y_label_area_size(90)
            .build_cartesian_2d(min_ts..max_ts, (min_oi * 0.9)..(max_oi * 1.1))?;

        chart
            .configure_mesh()
            .x_desc("Date")
            .y_desc(value_label)
            .x_label_formatter(&|x| x.format("%Y-%m-%d").to_string())
            .y_label_formatter(&|y| format_oi_value(*y))
            .draw()?;

        for (label, points) in &sorted_series {
            let color = get_color_for_label(label);
            chart
                .draw_series(LineSeries::new(
                    points.iter().map(|(ts, v)| (*ts, *v)),
                    &color,
                ))?
                .label(label.as_str())
                .legend(move |(x, y)| PathElement::new(vec![(x, y), (x + 20, y)], &color));
        }

        chart
            .configure_series_labels()
            .position(SeriesLabelPosition::UpperRight)
            .background_style(&WHITE.mix(0.8))
            .border_style(&BLACK)
            .draw()?;
    }

    root.present()?;
    println!("\n✓ Plot saved to: {}", filename);

    Ok(())
}

// ─── stats hist ──────────────────────────────────────────────────────────────

#[derive(Args)]
pub struct HistArgs {
    /// Comma-separated list of symbols (e.g., BTC,ETH). Defaults to all symbols if not specified.
    #[arg(short, long)]
    pub symbols: Option<String>,

    /// Comma-separated list of exchanges (e.g., binance,bybit). Defaults to all exchanges if not specified.
    #[arg(short, long)]
    pub exchanges: Option<String>,

    /// Data to histogram (oi, volume)
    #[arg(short, long, default_value = "oi")]
    pub data: String,

    /// Use notional value: for oi → open_interest_notional; for volume → turnover_24h
    #[arg(long, default_value_t = false)]
    pub notional: bool,

    /// Number of histogram bins
    #[arg(short, long, default_value = "10")]
    pub bins: usize,

    /// Output format (table, json)
    #[arg(short, long, default_value = "table")]
    pub format: String,

    /// Export histogram as a PNG chart (saved to out/)
    #[arg(long, default_value_t = false)]
    pub plot: bool,

    /// Database URL (required)
    #[arg(long, env = "DATABASE_URL")]
    pub database_url: Option<String>,
}

/// A single bin in the histogram
#[derive(Debug)]
struct HistBin {
    lower: f64,
    upper: f64,
    count: usize,
    pct: f64,
}

/// A complete histogram for one symbol+exchange group
#[derive(Debug)]
struct Histogram {
    /// Human-readable label, e.g. "BTC on BINANCE"
    label: String,
    data_type_label: String,
    bins: Vec<HistBin>,
    total: usize,
    min: f64,
    max: f64,
    mean: f64,
}

/// Fetch raw f64 values for one symbol on one exchange.
async fn fetch_raw_values(
    pool: &PgPool,
    symbol: &str,
    exchange_id: i32,
    column: &str,
) -> Result<Vec<f64>> {
    // Column name is validated by data_type_column() before this call — not user input.
    let query = format!(
        r#"
        SELECT {col}::DOUBLE PRECISION
        FROM tickers
        WHERE exchange_id = $1
            AND symbol = $2
            AND {col} IS NOT NULL
            AND {col} > 0
        "#,
        col = column
    );

    sqlx::query_scalar::<_, f64>(&query)
        .bind(exchange_id)
        .bind(symbol)
        .fetch_all(pool)
        .await
        .context("Failed to fetch raw values for histogram")
}

/// Bin a slice of values into `num_bins` equal-width bins.
fn compute_histogram(label: String, data_type: &str, notional: bool, values: &[f64], num_bins: usize) -> Histogram {
    let total = values.len();
    let min = values.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let mean = if total > 0 {
        values.iter().sum::<f64>() / total as f64
    } else {
        0.0
    };

    let bins = if total == 0 || num_bins == 0 {
        vec![]
    } else if (max - min).abs() < f64::EPSILON {
        // All values identical — one bin
        vec![HistBin {
            lower: min,
            upper: max,
            count: total,
            pct: 100.0,
        }]
    } else {
        let bin_width = (max - min) / num_bins as f64;
        let mut counts = vec![0usize; num_bins];
        for &v in values {
            let idx = ((v - min) / bin_width) as usize;
            counts[idx.min(num_bins - 1)] += 1;
        }
        counts
            .into_iter()
            .enumerate()
            .map(|(i, count)| HistBin {
                lower: min + i as f64 * bin_width,
                upper: min + (i + 1) as f64 * bin_width,
                count,
                pct: count as f64 / total as f64 * 100.0,
            })
            .collect()
    };

    Histogram {
        label,
        data_type_label: data_type_label(data_type, notional).to_string(),
        bins,
        total,
        min,
        max,
        mean,
    }
}

/// Render one histogram as a terminal bar chart.
fn render_histogram_table(hist: &Histogram) {
    println!(
        "\n{} — {}",
        hist.label, hist.data_type_label
    );
    println!(
        "  Min: {}   Max: {}   Mean: {}   Samples: {}",
        format_oi_value(hist.min),
        format_oi_value(hist.max),
        format_oi_value(hist.mean),
        hist.total
    );
    println!();

    if hist.bins.is_empty() {
        println!("  (no data)");
        return;
    }

    let max_count = hist.bins.iter().map(|b| b.count).max().unwrap_or(1);
    const BAR_WIDTH: usize = 40;

    for bin in &hist.bins {
        let bar_len = if max_count > 0 {
            bin.count * BAR_WIDTH / max_count
        } else {
            0
        };
        let bar = "█".repeat(bar_len);
        println!(
            "  [{:>8}, {:>8})  {:<40}  {:>6}  {:>5.1}%",
            format_oi_value(bin.lower),
            format_oi_value(bin.upper),
            bar,
            bin.count,
            bin.pct,
        );
    }
}

/// Export one histogram as a PNG bar chart.
///
/// Returns the path to the saved file.
fn plot_histogram(hist: &Histogram, data_type: &str, color: RGBColor) -> Result<String> {
    if hist.bins.is_empty() {
        anyhow::bail!("No bins to plot for '{}'", hist.label);
    }

    std::fs::create_dir_all("out").context("Failed to create output directory")?;

    // "BTC on BINANCE" → "BTC_BINANCE"
    let label_slug = hist
        .label
        .replace(" on ", "_")
        .replace(' ', "_")
        .to_uppercase();
    let filename = format!("out/hist_{}_{}.png", label_slug, data_type);

    // Scope root so it drops (flushing the PNG) before we return the filename.
    {
        let root = BitMapBackend::new(&filename, (1200, 800)).into_drawing_area();
        root.fill(&WHITE)?;

        // ── Header (top 110 px): title + stats line ──────────────────────────
        let (header, chart_area) = root.split_vertically(110);

        let title = format!(
            "{} — {} Histogram ({} bins)",
            hist.label,
            hist.data_type_label,
            hist.bins.len()
        );
        header.draw(&Text::new(
            title,
            (30, 22),
            ("sans-serif", 22).into_font().color(&BLACK),
        ))?;

        let stats_text = format!(
            "Min: {}   Mean: {}   Max: {}   Samples: {}",
            format_oi_value(hist.min),
            format_oi_value(hist.mean),
            format_oi_value(hist.max),
            hist.total,
        );
        header.draw(&Text::new(
            stats_text,
            (30, 68),
            ("sans-serif", 15).into_font().color(&RGBColor(90, 90, 90)),
        ))?;

        // ── Chart area ───────────────────────────────────────────────────────
        let num_bins = hist.bins.len();
        let max_count = hist.bins.iter().map(|b| b.count).max().unwrap_or(1);
        // 20 % headroom so percentage labels above bars don't clip
        let y_max = (max_count as f64 * 1.22) as u32 + 1;

        let mut chart = ChartBuilder::on(&chart_area)
            .margin(20)
            .x_label_area_size(60)
            .y_label_area_size(80)
            .build_cartesian_2d(0u32..num_bins as u32, 0u32..y_max)?;

        chart
            .configure_mesh()
            .x_labels(num_bins + 1)
            .x_label_formatter(&|x| {
                let idx = *x as usize;
                if idx < hist.bins.len() {
                    format_oi_value(hist.bins[idx].lower)
                } else {
                    hist.bins
                        .last()
                        .map(|b| format_oi_value(b.upper))
                        .unwrap_or_default()
                }
            })
            .x_desc(hist.data_type_label.as_str())
            .y_desc("Count")
            .light_line_style(RGBColor(230, 230, 230))
            .bold_line_style(RGBColor(200, 200, 200))
            .draw()?;

        // ── Bars: filled + darker outline ────────────────────────────────────
        let fill_style = ShapeStyle {
            color: RGBAColor(color.0, color.1, color.2, 0.82),
            filled: true,
            stroke_width: 0,
        };
        let outline_style = ShapeStyle {
            color: RGBAColor(
                (color.0 as f64 * 0.65) as u8,
                (color.1 as f64 * 0.65) as u8,
                (color.2 as f64 * 0.65) as u8,
                1.0,
            ),
            filled: false,
            stroke_width: 1,
        };

        chart.draw_series(hist.bins.iter().enumerate().map(|(i, bin)| {
            Rectangle::new(
                [(i as u32, 0u32), (i as u32 + 1, bin.count as u32)],
                fill_style,
            )
        }))?;
        chart.draw_series(hist.bins.iter().enumerate().map(|(i, bin)| {
            Rectangle::new(
                [(i as u32, 0u32), (i as u32 + 1, bin.count as u32)],
                outline_style,
            )
        }))?;

        // ── Percentage labels above each bar ──────────────────────────────────
        let label_offset = (y_max as f64 * 0.025) as u32 + 1;
        for (i, bin) in hist.bins.iter().enumerate() {
            if bin.count > 0 {
                chart.draw_series(std::iter::once(Text::new(
                    format!("{:.1}%", bin.pct),
                    (i as u32, bin.count as u32 + label_offset),
                    ("sans-serif", 11).into_font().color(&BLACK),
                )))?;
            }
        }

        root.present()?;
    } // root drops here → PNG flushed, borrow on filename released

    Ok(filename)
}

/// Render one histogram as JSON.
fn render_histogram_json(hist: &Histogram) -> serde_json::Value {
    json!({
        "label": hist.label,
        "data_type": hist.data_type_label,
        "total": hist.total,
        "min": hist.min,
        "max": hist.max,
        "mean": hist.mean,
        "bins": hist.bins.iter().map(|b| json!({
            "lower": b.lower,
            "upper": b.upper,
            "count": b.count,
            "pct": b.pct,
        })).collect::<Vec<_>>(),
    })
}

pub async fn execute_hist(args: HistArgs) -> Result<()> {
    // Validate data type early so we fail fast before DB connection
    let column = data_type_column(&args.data, args.notional)?;

    if args.bins == 0 {
        anyhow::bail!("--bins must be at least 1");
    }

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
        None => {
            tracing::info!("No symbols specified, fetching all symbols");
            get_all_symbol_names(&pool).await?
        }
    };

    let exchanges: Vec<String> = match args.exchanges {
        Some(ref e) => e
            .split(',')
            .map(|v| v.trim().to_lowercase())
            .filter(|v| !v.is_empty())
            .collect(),
        None => {
            tracing::info!("No exchanges specified, fetching all exchanges");
            get_all_exchange_names(&pool).await?
        }
    };

    if symbols.len() > 1 && exchanges.len() > 1 {
        anyhow::bail!(
            "Invalid combination: multiple symbols ({}) with multiple exchanges ({}) is not supported.\n\
            Use either:\n\
            - Multiple symbols with a single exchange\n\
            - Single symbol with multiple exchanges",
            symbols.len(),
            exchanges.len()
        );
    }
    if symbols.is_empty() {
        anyhow::bail!("No symbols found in database");
    }
    if exchanges.is_empty() {
        anyhow::bail!("No exchanges found in database");
    }

    let exchange_ids = get_exchange_ids(&pool, &exchanges).await?;
    if exchange_ids.is_empty() {
        anyhow::bail!("No valid exchanges found in database");
    }

    // Build one (label, symbol, exchange_id) group per histogram
    let groups: Vec<(String, String, i32)> = if symbols.len() > 1 {
        // Multiple symbols, single exchange
        symbols
            .iter()
            .map(|sym| {
                let lbl = format!("{} on {}", sym, exchanges[0].to_uppercase());
                (lbl, sym.clone(), exchange_ids[0])
            })
            .collect()
    } else {
        // Single symbol, one or more exchanges
        exchanges
            .iter()
            .zip(exchange_ids.iter())
            .map(|(exch, &eid)| {
                let lbl = format!("{} on {}", symbols[0], exch.to_uppercase());
                (lbl, symbols[0].clone(), eid)
            })
            .collect()
    };

    // Tableau-10 palette — same order used elsewhere in this file
    let palette = [
        RGBColor(31, 119, 180),
        RGBColor(255, 127, 14),
        RGBColor(44, 160, 44),
        RGBColor(214, 39, 40),
        RGBColor(148, 103, 189),
        RGBColor(140, 86, 75),
        RGBColor(227, 119, 194),
        RGBColor(127, 127, 127),
        RGBColor(188, 189, 34),
        RGBColor(23, 190, 207),
    ];

    // Compute and render one histogram per group
    let mut json_out: Vec<serde_json::Value> = Vec::new();

    for (idx, (label, symbol, exchange_id)) in groups.iter().enumerate() {
        let values = fetch_raw_values(&pool, symbol, *exchange_id, column).await?;
        if values.is_empty() {
            tracing::warn!(label = %label, "No data found, skipping");
            continue;
        }

        let hist = compute_histogram(label.clone(), &args.data, args.notional, &values, args.bins);

        if args.plot {
            let color = get_color_for_label(label);
            // Fall back to palette index if label not in the lookup map
            let color = if color == RGBColor(127, 127, 127) {
                palette[idx % palette.len()]
            } else {
                color
            };
            let path = plot_histogram(&hist, &args.data, color)?;
            println!("\n✓ Plot saved to: {}", path);
        } else {
            match args.format.to_lowercase().as_str() {
                "json" => json_out.push(render_histogram_json(&hist)),
                _ => render_histogram_table(&hist),
            }
        }
    }

    if !args.plot && args.format.to_lowercase() == "json" {
        let out = if json_out.len() == 1 {
            json_out.into_iter().next().unwrap()
        } else {
            serde_json::Value::Array(json_out)
        };
        println!("{}", serde_json::to_string_pretty(&out)?);
    }

    Ok(())
}
