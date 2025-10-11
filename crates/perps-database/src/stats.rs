use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

/// Database statistics
#[derive(Debug, Serialize, Deserialize)]
pub struct DatabaseStats {
    pub tables: Vec<TableStats>,
    pub recent_ingests: Vec<RecentIngest>,
    pub total_size: String,
}

/// Statistics for a single table
#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct TableStats {
    pub table_name: String,
    pub row_count: i64,
    pub table_size: String,
    pub index_size: String,
    pub total_size: String,
}

/// Recent ingest information
#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct RecentIngest {
    pub table_name: String,
    pub symbol: Option<String>,
    pub latest_ts: Option<DateTime<Utc>>,
    pub count_last_hour: i64,
}

/// Get comprehensive database statistics
pub async fn get_database_stats(pool: &PgPool) -> Result<DatabaseStats> {
    let tables = get_table_stats(pool).await?;
    let recent_ingests = get_recent_ingests(pool).await?;
    let total_size = get_total_database_size(pool).await?;

    Ok(DatabaseStats {
        tables,
        recent_ingests,
        total_size,
    })
}

/// Get statistics for all tables
async fn get_table_stats(pool: &PgPool) -> Result<Vec<TableStats>> {
    let stats = sqlx::query_as::<_, TableStats>(
        r#"
        SELECT
            schemaname || '.' || tablename AS table_name,
            n_live_tup AS row_count,
            pg_size_pretty(pg_table_size(schemaname || '.' || tablename)) AS table_size,
            pg_size_pretty(pg_indexes_size(schemaname || '.' || tablename)) AS index_size,
            pg_size_pretty(pg_total_relation_size(schemaname || '.' || tablename)) AS total_size
        FROM pg_stat_user_tables
        WHERE schemaname = 'public'
        ORDER BY pg_total_relation_size(schemaname || '.' || tablename) DESC
        "#,
    )
    .fetch_all(pool)
    .await
    .context("Failed to fetch table statistics")?;

    Ok(stats)
}

/// Get recent ingest statistics
async fn get_recent_ingests(pool: &PgPool) -> Result<Vec<RecentIngest>> {
    let mut ingests = Vec::new();

    // Get stats for tickers
    let ticker_stats = sqlx::query_as::<_, RecentIngest>(
        r#"
        SELECT
            'tickers' AS table_name,
            symbol,
            MAX(ts) AS latest_ts,
            COUNT(*) FILTER (WHERE ts > now() - INTERVAL '1 hour') AS count_last_hour
        FROM tickers
        GROUP BY symbol
        ORDER BY latest_ts DESC NULLS LAST
        LIMIT 10
        "#,
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    ingests.extend(ticker_stats);

    // Get stats for orderbooks
    let orderbook_stats = sqlx::query_as::<_, RecentIngest>(
        r#"
        SELECT
            'orderbooks' AS table_name,
            symbol,
            MAX(ts) AS latest_ts,
            COUNT(*) FILTER (WHERE ts > now() - INTERVAL '1 hour') AS count_last_hour
        FROM orderbooks
        GROUP BY symbol
        ORDER BY latest_ts DESC NULLS LAST
        LIMIT 10
        "#,
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    ingests.extend(orderbook_stats);

    // Get stats for trades
    let trade_stats = sqlx::query_as::<_, RecentIngest>(
        r#"
        SELECT
            'trades' AS table_name,
            symbol,
            MAX(ts) AS latest_ts,
            COUNT(*) FILTER (WHERE ts > now() - INTERVAL '1 hour') AS count_last_hour
        FROM trades
        GROUP BY symbol
        ORDER BY latest_ts DESC NULLS LAST
        LIMIT 10
        "#,
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    ingests.extend(trade_stats);

    // Get stats for funding_rates
    let funding_stats = sqlx::query_as::<_, RecentIngest>(
        r#"
        SELECT
            'funding_rates' AS table_name,
            symbol,
            MAX(ts) AS latest_ts,
            COUNT(*) FILTER (WHERE ts > now() - INTERVAL '1 hour') AS count_last_hour
        FROM funding_rates
        GROUP BY symbol
        ORDER BY latest_ts DESC NULLS LAST
        LIMIT 10
        "#,
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    ingests.extend(funding_stats);

    Ok(ingests)
}

/// Get total database size
async fn get_total_database_size(pool: &PgPool) -> Result<String> {
    let size: String = sqlx::query_scalar(
        r#"
        SELECT pg_size_pretty(pg_database_size(current_database()))
        "#,
    )
    .fetch_one(pool)
    .await
    .context("Failed to fetch database size")?;

    Ok(size)
}

/// Clean old data from tables
pub async fn clean_old_data(pool: &PgPool, days: i32) -> Result<CleanResult> {
    tracing::info!("Cleaning data older than {} days", days);

    let cutoff = Utc::now() - chrono::Duration::days(days as i64);
    let mut result = CleanResult::default();

    // Clean tickers
    result.tickers_deleted = clean_table(pool, "tickers", cutoff).await?;

    // Clean orderbooks
    result.orderbooks_deleted = clean_table(pool, "orderbooks", cutoff).await?;

    // Clean trades
    result.trades_deleted = clean_table(pool, "trades", cutoff).await?;

    // Clean funding_rates
    result.funding_rates_deleted = clean_table(pool, "funding_rates", cutoff).await?;

    // Clean ingest_events
    result.ingest_events_deleted = clean_table(pool, "ingest_events", cutoff).await?;

    tracing::info!("Cleanup completed: {:?}", result);
    Ok(result)
}

async fn clean_table(pool: &PgPool, table: &str, cutoff: DateTime<Utc>) -> Result<i64> {
    let query = format!("DELETE FROM {} WHERE ts < $1", table);
    let result = sqlx::query(&query)
        .bind(cutoff)
        .execute(pool)
        .await
        .with_context(|| format!("Failed to clean table {}", table))?;

    Ok(result.rows_affected() as i64)
}

/// Result of a cleanup operation
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct CleanResult {
    pub tickers_deleted: i64,
    pub orderbooks_deleted: i64,
    pub trades_deleted: i64,
    pub funding_rates_deleted: i64,
    pub ingest_events_deleted: i64,
}

impl CleanResult {
    pub fn total(&self) -> i64 {
        self.tickers_deleted
            + self.orderbooks_deleted
            + self.trades_deleted
            + self.funding_rates_deleted
            + self.ingest_events_deleted
    }
}

/// Truncate all data tables (WARNING: destructive)
pub async fn truncate_all_tables(pool: &PgPool) -> Result<()> {
    tracing::warn!("TRUNCATING ALL TABLES - This will delete all data!");

    sqlx::query("TRUNCATE TABLE tickers, orderbooks, trades, funding_rates, ingest_events RESTART IDENTITY CASCADE")
        .execute(pool)
        .await
        .context("Failed to truncate tables")?;

    tracing::info!("All tables truncated successfully");
    Ok(())
}
