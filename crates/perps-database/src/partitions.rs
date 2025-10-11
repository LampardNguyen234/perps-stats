use anyhow::{Context, Result};
use chrono::{NaiveDate, Utc};
use sqlx::PgPool;

/// Tables that support partitioning
const PARTITIONED_TABLES: &[&str] = &["tickers", "orderbooks", "trades", "funding_rates"];

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

/// Create a partition for a specific table and date
async fn create_partition_for_date(pool: &PgPool, table: &str, date: NaiveDate) -> Result<()> {
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

    // Create the partition
    let create_sql = format!(
        "CREATE TABLE {} PARTITION OF {} FOR VALUES FROM ('{}') TO ('{}')",
        partition_name, table, start_date, end_date
    );

    sqlx::query(&create_sql)
        .execute(pool)
        .await
        .with_context(|| format!("Failed to create partition {}", partition_name))?;

    tracing::debug!("Created partition: {}", partition_name);
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
    // Query to find partitions for this table
    let partitions: Vec<String> = sqlx::query_scalar(
        r#"
        SELECT c.relname
        FROM pg_class c
        JOIN pg_inherits i ON i.inhrelid = c.oid
        JOIN pg_class p ON p.oid = i.inhparent
        WHERE p.relname = $1
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
            p.relname AS parent_table,
            c.relname AS partition_name,
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
