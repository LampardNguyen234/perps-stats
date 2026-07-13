use anyhow::{Context, Result};
use chrono::{NaiveDate, Utc};
use sqlx::PgPool;

/// Tables that support daily partitioning
const PARTITIONED_TABLES: &[&str] = &[
    "tickers",
    "orderbooks",
    "trades",
    "funding_rates",
    "liquidity_depth",
    "klines",
    "slippage",
];

/// Create daily partitions for the next N days
pub async fn create_partitions(pool: &PgPool, days: i32) -> Result<()> {
    tracing::info!("Creating partitions for the next {} days", days);

    let today = Utc::now().date_naive();

    for day_offset in 0..days {
        let date = today + chrono::Duration::days(day_offset as i64);
        for table in PARTITIONED_TABLES {
            create_partition_for_date(pool, table, date).await?;
        }
    }

    tracing::info!("Partitions created successfully");
    Ok(())
}

/// Create daily partitions for a date range (inclusive).
/// Safe to call concurrently — uses IF NOT EXISTS and existence checks.
pub async fn create_partitions_for_range(
    pool: &PgPool,
    start_date: NaiveDate,
    end_date: NaiveDate,
) -> Result<()> {
    tracing::info!(
        "Creating partitions for date range {} to {}",
        start_date,
        end_date
    );

    let mut date = start_date;
    while date <= end_date {
        for table in PARTITIONED_TABLES {
            create_partition_for_date(pool, table, date).await?;
        }
        date += chrono::Duration::days(1);
    }

    tracing::info!(
        "Partitions created successfully for range {} to {}",
        start_date,
        end_date
    );
    Ok(())
}

/// Create a partition for a specific table and date.
///
/// If the default partition contains rows in this date range, they are evacuated
/// first. PostgreSQL rejects CREATE PARTITION when the default partition has rows
/// that would belong to the new partition — this happens when data was ingested
/// before the partition existed (e.g. service restart with a new exchange/symbol).
pub async fn create_partition_for_date(pool: &PgPool, table: &str, date: NaiveDate) -> Result<()> {
    let partition_name = format!("{}_{}", table, date.format("%Y_%m_%d"));
    let start_date = date.format("%Y-%m-%d").to_string();
    let end_date = (date + chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();

    // Check if partition already exists
    let exists: bool = sqlx::query_scalar(
        r#"
        SELECT EXISTS (
            SELECT 1
            FROM pg_class c
            JOIN pg_namespace n ON n.oid = c.relnamespace
            WHERE c.relname = $1
        )
        "#,
    )
    .bind(&partition_name)
    .fetch_one(pool)
    .await
    .context("Failed to check if partition exists")?;

    if exists {
        tracing::debug!("Partition {} already exists, skipping", partition_name);
        return Ok(());
    }

    // All three names are safe to interpolate: `table` comes from the PARTITIONED_TABLES
    // static constant, `partition_name` and `default_table` are derived from it and a
    // typed NaiveDate — no user-controlled input reaches these strings.
    let default_table = format!("{}_default", table);
    // Temp table name is unique per (table, date) so concurrent calls don't collide.
    let temp_table = format!("evac_{}_{}", table, date.format("%Y_%m_%d"));

    let mut tx = pool.begin().await.context("Failed to begin transaction")?;

    // Snapshot any conflicting rows from the default partition into a temp table that
    // is automatically dropped when the transaction commits (ON COMMIT DROP).
    sqlx::query(&format!(
        "CREATE TEMP TABLE {temp} ON COMMIT DROP AS \
         SELECT * FROM {default} WHERE ts >= '{start}' AND ts < '{end}'",
        temp = temp_table,
        default = default_table,
        start = start_date,
        end = end_date,
    ))
    .execute(&mut *tx)
    .await
    .context("Failed to snapshot default partition rows")?;

    sqlx::query(&format!(
        "DELETE FROM {default} WHERE ts >= '{start}' AND ts < '{end}'",
        default = default_table,
        start = start_date,
        end = end_date,
    ))
    .execute(&mut *tx)
    .await
    .context("Failed to evacuate default partition")?;

    sqlx::query(&format!(
        "CREATE TABLE IF NOT EXISTS {part} PARTITION OF {table} FOR VALUES FROM ('{start}') TO ('{end}')",
        part = partition_name,
        table = table,
        start = start_date,
        end = end_date,
    ))
    .execute(&mut *tx)
    .await
    .with_context(|| format!("Failed to create partition {}", partition_name))?;

    // Re-route evacuated rows into the parent table; they now land in the new partition.
    let reinserted: u64 = sqlx::query(&format!(
        "INSERT INTO {table} SELECT * FROM {temp}",
        table = table,
        temp = temp_table,
    ))
    .execute(&mut *tx)
    .await
    .context("Failed to reinsert evacuated rows")?
    .rows_affected();

    tx.commit()
        .await
        .context("Failed to commit partition creation")?;

    if reinserted > 0 {
        tracing::info!(
            "Created partition {} and migrated {} rows from default partition",
            partition_name,
            reinserted
        );
    } else {
        tracing::debug!("Created partition: {}", partition_name);
    }

    Ok(())
}

/// Drop partitions older than N days
pub async fn drop_old_partitions(pool: &PgPool, days: i32) -> Result<usize> {
    tracing::info!("Dropping partitions older than {} days", days);

    let cutoff_date = Utc::now().date_naive() - chrono::Duration::days(days as i64);
    let mut dropped_count = 0;

    for table in PARTITIONED_TABLES {
        dropped_count += drop_partitions_before_date(pool, table, cutoff_date).await?;
    }

    tracing::info!("Dropped {} old partitions", dropped_count);
    Ok(dropped_count)
}

/// Drop partitions for a table that are older than the specified date
async fn drop_partitions_before_date(
    pool: &PgPool,
    table: &str,
    cutoff_date: NaiveDate,
) -> Result<usize> {
    // Query to find partitions for this table (exclude default partitions)
    let partitions: Vec<String> = sqlx::query_scalar(
        r#"
        SELECT c.relname::text
        FROM pg_class c
        JOIN pg_inherits i ON i.inhrelid = c.oid
        JOIN pg_class p ON p.oid = i.inhparent
        WHERE p.relname = $1
          AND c.relname NOT LIKE '%_default'
        "#,
    )
    .bind(table)
    .fetch_all(pool)
    .await
    .context("Failed to query partitions")?;

    let mut dropped = 0;

    for partition_name in partitions {
        // Extract date from partition name (format: tablename_YYYY_MM_DD)
        if let Some(date_part) = partition_name.strip_prefix(&format!("{}_", table)) {
            // Parse date from YYYY_MM_DD format
            if let Ok(partition_date) = NaiveDate::parse_from_str(date_part, "%Y_%m_%d") {
                if partition_date < cutoff_date {
                    let drop_sql = format!("DROP TABLE IF EXISTS {}", partition_name);
                    sqlx::query(&drop_sql)
                        .execute(pool)
                        .await
                        .with_context(|| format!("Failed to drop partition {}", partition_name))?;

                    tracing::debug!("Dropped partition: {}", partition_name);
                    dropped += 1;
                }
            }
        }
    }

    Ok(dropped)
}

/// List all partitions for monitoring
pub async fn list_partitions(pool: &PgPool) -> Result<Vec<PartitionInfo>> {
    let partitions = sqlx::query_as::<_, PartitionInfo>(
        r#"
        SELECT
            p.relname::text AS parent_table,
            c.relname::text AS partition_name,
            pg_size_pretty(pg_total_relation_size(c.oid)) AS size
        FROM pg_class c
        JOIN pg_inherits i ON i.inhrelid = c.oid
        JOIN pg_class p ON p.oid = i.inhparent
        WHERE p.relname = ANY($1)
        ORDER BY p.relname, c.relname
        "#,
    )
    .bind(PARTITIONED_TABLES)
    .fetch_all(pool)
    .await
    .context("Failed to list partitions")?;

    Ok(partitions)
}

#[derive(Debug, sqlx::FromRow)]
pub struct PartitionInfo {
    pub parent_table: String,
    pub partition_name: String,
    pub size: String,
}
