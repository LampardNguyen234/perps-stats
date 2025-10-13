use anyhow::Result;
use chrono::{DateTime, NaiveDate, Utc};
use prettytable::{format, Cell, Row, Table};
use serde_json::json;
use sqlx::{PgPool, Row as SqlxRow};
use std::path::Path;

/// Initialize database schema, run migrations, and create partitions
pub async fn init(database_url: Option<String>, create_partitions_days: i32) -> Result<()> {
    let db_url = database_url.ok_or_else(|| {
        anyhow::anyhow!(
            "DATABASE_URL is required. Set via --database-url flag or DATABASE_URL environment variable"
        )
    })?;

    tracing::info!("Initializing database schema");
    tracing::info!("Database URL: {}", mask_password(&db_url));

    // Connect to database
    let pool = PgPool::connect(&db_url).await?;

    // Run migrations
    tracing::info!("Running migrations from migrations/ directory");
    let migrations_path = Path::new("migrations");
    if !migrations_path.exists() {
        anyhow::bail!("Migrations directory not found: migrations/");
    }

    sqlx::migrate!("./migrations").run(&pool).await?;
    tracing::info!("✓ Migrations completed successfully");

    // Create partitions for past 7 days (for backfilling) and next N days
    tracing::info!(
        "Creating partitions for past 7 days and next {} days",
        create_partitions_days
    );
    create_partitions(&pool, create_partitions_days).await?;
    tracing::info!("✓ Partitions created successfully");

    tracing::info!("✓ Database initialization completed");
    Ok(())
}

/// Run database migrations only
pub async fn migrate(database_url: Option<String>) -> Result<()> {
    let db_url = database_url.ok_or_else(|| {
        anyhow::anyhow!(
            "DATABASE_URL is required. Set via --database-url flag or DATABASE_URL environment variable"
        )
    })?;

    tracing::info!("Running database migrations");
    tracing::info!("Database URL: {}", mask_password(&db_url));

    let pool = PgPool::connect(&db_url).await?;

    let migrations_path = Path::new("migrations");
    if !migrations_path.exists() {
        anyhow::bail!("Migrations directory not found: migrations/");
    }

    sqlx::migrate!("./migrations").run(&pool).await?;
    tracing::info!("✓ Migrations completed successfully");

    Ok(())
}

/// Clean old data or truncate tables
pub async fn clean(
    database_url: Option<String>,
    older_than: Option<i32>,
    drop_partitions_older_than: Option<i32>,
    truncate: bool,
) -> Result<()> {
    let db_url = database_url.ok_or_else(|| {
        anyhow::anyhow!(
            "DATABASE_URL is required. Set via --database-url flag or DATABASE_URL environment variable"
        )
    })?;

    if !truncate && older_than.is_none() && drop_partitions_older_than.is_none() {
        anyhow::bail!("Must specify at least one cleaning option: --older-than, --drop-partitions-older-than, or --truncate");
    }

    if truncate {
        tracing::warn!("⚠️  WARNING: This will DELETE ALL DATA from the database!");
        tracing::warn!("Press Ctrl+C within 5 seconds to cancel...");
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
    }

    tracing::info!("Connecting to database");
    let pool = PgPool::connect(&db_url).await?;

    if truncate {
        tracing::info!("Truncating all tables...");
        truncate_all_tables(&pool).await?;
        tracing::info!("✓ All tables truncated");
        return Ok(());
    }

    if let Some(days) = older_than {
        tracing::info!("Deleting data older than {} days", days);
        delete_old_data(&pool, days).await?;
        tracing::info!("✓ Old data deleted");
    }

    if let Some(days) = drop_partitions_older_than {
        tracing::info!("Dropping partitions older than {} days", days);
        drop_old_partitions(&pool, days).await?;
        tracing::info!("✓ Old partitions dropped");
    }

    Ok(())
}

/// Show database statistics
pub async fn stats(database_url: Option<String>, format: &str) -> Result<()> {
    let db_url = database_url.ok_or_else(|| {
        anyhow::anyhow!(
            "DATABASE_URL is required. Set via --database-url flag or DATABASE_URL environment variable"
        )
    })?;

    tracing::info!("Fetching database statistics");
    let pool = PgPool::connect(&db_url).await?;

    // Fetch table statistics
    let stats = fetch_table_stats(&pool).await?;

    match format.to_lowercase().as_str() {
        "json" => {
            println!("{}", serde_json::to_string_pretty(&stats)?);
        }
        _ => {
            display_stats_table(&stats)?;
        }
    }

    Ok(())
}

/// Create partitions for partitioned tables
/// Creates partitions for past days (for backfilling) and future days
async fn create_partitions(pool: &PgPool, days: i32) -> Result<()> {
    let tables = vec!["tickers", "orderbooks", "trades", "funding_rates", "liquidity_depth", "klines"];

    // Create partitions for past 7 days (for backfilling/historical data) and future N days
    let past_days = 7;
    for day_offset in -past_days..days {
        let date = Utc::now().date_naive() + chrono::Duration::days(day_offset as i64);
        let next_date = date + chrono::Duration::days(1);

        for table in &tables {
            let partition_name = format!("{}_{}", table, date.format("%Y_%m_%d"));

            // Check if partition exists
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_class WHERE relname = $1)",
            )
            .bind(&partition_name)
            .fetch_one(pool)
            .await?;

            if !exists {
                let query = format!(
                    "CREATE TABLE {} PARTITION OF {} FOR VALUES FROM ('{}') TO ('{}')",
                    partition_name,
                    table,
                    date.format("%Y-%m-%d"),
                    next_date.format("%Y-%m-%d")
                );

                sqlx::query(&query).execute(pool).await?;
                tracing::debug!("✓ Created partition: {}", partition_name);
            } else {
                tracing::debug!("Partition already exists: {}", partition_name);
            }
        }
    }

    Ok(())
}

/// Truncate all tables
async fn truncate_all_tables(pool: &PgPool) -> Result<()> {
    // Order matters due to foreign key constraints
    let tables = vec![
        "tickers",
        "orderbooks",
        "trades",
        "funding_rates",
        "liquidity_depth",
        "markets",
        "klines",
        // Don't truncate exchanges as it's a reference table
    ];

    for table in tables {
        tracing::info!("Truncating table: {}", table);
        let query = format!("TRUNCATE TABLE {} CASCADE", table);
        sqlx::query(&query).execute(pool).await?;
    }

    Ok(())
}

/// Delete data older than N days
async fn delete_old_data(pool: &PgPool, days: i32) -> Result<()> {
    let cutoff_date = Utc::now() - chrono::Duration::days(days as i64);
    let tables = vec!["tickers", "orderbooks", "trades", "funding_rates", "liquidity_depth", "klines"];

    for table in tables {
        tracing::info!("Deleting old data from table: {}", table);
        let query = format!("DELETE FROM {} WHERE ts < $1", table);
        let result = sqlx::query(&query)
            .bind(cutoff_date)
            .execute(pool)
            .await?;

        tracing::info!("✓ Deleted {} rows from {}", result.rows_affected(), table);
    }

    Ok(())
}

/// Drop partitions older than N days
async fn drop_old_partitions(pool: &PgPool, days: i32) -> Result<()> {
    let cutoff_date = Utc::now().date_naive() - chrono::Duration::days(days as i64);

    // Query to find all partitions
    let partitions: Vec<(String,)> = sqlx::query_as(
        "SELECT tablename FROM pg_tables WHERE schemaname = 'public' AND tablename ~ '^(tickers|orderbooks|trades|funding_rates|liquidity_depth|klines)_[0-9]{4}_[0-9]{2}_[0-9]{2}$'"
    )
    .fetch_all(pool)
    .await?;

    for (partition_name,) in partitions {
        // Extract date from partition name (e.g., "tickers_2024_01_15")
        let parts: Vec<&str> = partition_name.split('_').collect();
        if parts.len() >= 4 {
            let year: i32 = parts[parts.len() - 3].parse()?;
            let month: u32 = parts[parts.len() - 2].parse()?;
            let day: u32 = parts[parts.len() - 1].parse()?;

            if let Some(partition_date) = NaiveDate::from_ymd_opt(year, month, day) {
                if partition_date < cutoff_date {
                    tracing::info!("Dropping partition: {}", partition_name);
                    let query = format!("DROP TABLE IF EXISTS {}", partition_name);
                    sqlx::query(&query).execute(pool).await?;
                }
            }
        }
    }

    Ok(())
}

/// Fetch table statistics
async fn fetch_table_stats(pool: &PgPool) -> Result<serde_json::Value> {
    let mut stats = json!({});

    // Get row counts and sizes for each table
    let tables = vec![
        "exchanges",
        "markets",
        "tickers",
        "orderbooks",
        "trades",
        "funding_rates",
        "liquidity_depth",
        "klines",
        "open_interest",
    ];

    for table in tables {
        // Get row count
        let count_query = format!("SELECT COUNT(*) as count FROM {}", table);
        let row_count: i64 = sqlx::query_scalar(&count_query).fetch_one(pool).await?;

        // Get table size
        let size_query = "SELECT pg_size_pretty(pg_total_relation_size($1))";
        let size: String = sqlx::query_scalar(size_query)
            .bind(table)
            .fetch_one(pool)
            .await?;

        // Get date range for time-series tables
        let mut min_ts: Option<DateTime<Utc>> = None;
        let mut max_ts: Option<DateTime<Utc>> = None;
        let mut data_age_minutes: Option<i64> = None;

        if vec!["tickers", "orderbooks", "trades", "funding_rates", "liquidity_depth", "klines", "open_interest"]
            .contains(&table)
        {
            let ts_query = format!("SELECT MIN(ts), MAX(ts) FROM {}", table);
            if let Ok(row) = sqlx::query(&ts_query).fetch_one(pool).await {
                min_ts = row.try_get(0).ok();
                max_ts = row.try_get(1).ok();

                // Calculate data freshness (minutes since last update)
                if let Some(max_timestamp) = max_ts {
                    let now = Utc::now();
                    data_age_minutes = Some((now - max_timestamp).num_minutes());
                }
            }
        }

        // Get per-exchange breakdown for time-series tables
        let mut exchange_breakdown = json!({});
        if vec!["tickers", "orderbooks", "trades", "funding_rates", "liquidity_depth", "klines", "open_interest"]
            .contains(&table)
        {
            let breakdown_query = format!(
                "SELECT e.name, COUNT(*) as count, MIN(t.ts) as min_ts, MAX(t.ts) as max_ts
                 FROM {} t
                 JOIN exchanges e ON t.exchange_id = e.id
                 GROUP BY e.name
                 ORDER BY count DESC",
                table
            );

            if let Ok(rows) = sqlx::query(&breakdown_query).fetch_all(pool).await {
                for row in rows {
                    let exchange_name: String = row.get("name");
                    let exchange_count: i64 = row.get("count");
                    let exchange_min_ts: Option<DateTime<Utc>> = row.try_get("min_ts").ok();
                    let exchange_max_ts: Option<DateTime<Utc>> = row.try_get("max_ts").ok();

                    let mut exchange_age_minutes: Option<i64> = None;
                    if let Some(exchange_max) = exchange_max_ts {
                        exchange_age_minutes = Some((Utc::now() - exchange_max).num_minutes());
                    }

                    exchange_breakdown[&exchange_name] = json!({
                        "count": exchange_count,
                        "min_timestamp": exchange_min_ts,
                        "max_timestamp": exchange_max_ts,
                        "data_age_minutes": exchange_age_minutes,
                    });
                }
            }
        }

        stats[table] = json!({
            "row_count": row_count,
            "size": size,
            "min_timestamp": min_ts,
            "max_timestamp": max_ts,
            "data_age_minutes": data_age_minutes,
            "exchange_breakdown": exchange_breakdown,
        });
    }

    // Get partition count
    let partition_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pg_tables WHERE schemaname = 'public' AND tablename ~ '^(tickers|orderbooks|trades|funding_rates|liquidity_depth|klines|open_interest)_[0-9]{4}_[0-9]{2}_[0-9]{2}$'"
    )
    .fetch_one(pool)
    .await?;

    // Get top symbols by activity (across all exchanges)
    let mut top_symbols = json!([]);
    if let Ok(rows) = sqlx::query(
        "SELECT symbol, COUNT(*) as total_count
         FROM (
             SELECT symbol FROM tickers
             UNION ALL SELECT symbol FROM trades
             UNION ALL SELECT symbol FROM orderbooks
         ) combined
         GROUP BY symbol
         ORDER BY total_count DESC
         LIMIT 10"
    ).fetch_all(pool).await {
        for row in rows {
            let symbol: String = row.get("symbol");
            let total_count: i64 = row.get("total_count");
            top_symbols.as_array_mut().unwrap().push(json!({
                "symbol": symbol,
                "total_records": total_count,
            }));
        }
    }

    stats["metadata"] = json!({
        "partition_count": partition_count,
        "top_symbols": top_symbols,
        "fetched_at": Utc::now(),
    });

    Ok(stats)
}

/// Display statistics in a table format
fn display_stats_table(stats: &serde_json::Value) -> Result<()> {
    let mut table = Table::new();
    table.set_format(*format::consts::FORMAT_NO_LINESEP_WITH_TITLE);

    table.set_titles(Row::new(vec![
        Cell::new("Table").with_style(prettytable::Attr::Bold),
        Cell::new("Row Count").with_style(prettytable::Attr::Bold),
        Cell::new("Size").with_style(prettytable::Attr::Bold),
        Cell::new("Data Age").with_style(prettytable::Attr::Bold),
        Cell::new("Min Timestamp").with_style(prettytable::Attr::Bold),
        Cell::new("Max Timestamp").with_style(prettytable::Attr::Bold),
    ]));

    let tables = vec![
        "exchanges",
        "markets",
        "tickers",
        "orderbooks",
        "trades",
        "funding_rates",
        "liquidity_depth",
        "klines",
        "open_interest",
        "ingest_events",
    ];

    for table_name in tables {
        if let Some(table_stats) = stats.get(table_name) {
            let row_count = table_stats["row_count"].as_i64().unwrap_or(0);
            let size = table_stats["size"].as_str().unwrap_or("N/A");

            let data_age = if let Some(age_minutes) = table_stats["data_age_minutes"].as_i64() {
                if age_minutes < 60 {
                    format!("{}m", age_minutes)
                } else if age_minutes < 1440 {
                    format!("{}h", age_minutes / 60)
                } else {
                    format!("{}d", age_minutes / 1440)
                }
            } else {
                "-".to_string()
            };

            let min_ts = table_stats["min_timestamp"]
                .as_str()
                .map(|s| s.split('.').next().unwrap_or(s).replace('T', " "))
                .unwrap_or_else(|| "-".to_string());
            let max_ts = table_stats["max_timestamp"]
                .as_str()
                .map(|s| s.split('.').next().unwrap_or(s).replace('T', " "))
                .unwrap_or_else(|| "-".to_string());

            table.add_row(Row::new(vec![
                Cell::new(table_name),
                Cell::new_align(&row_count.to_string(), format::Alignment::RIGHT),
                Cell::new_align(size, format::Alignment::RIGHT),
                Cell::new_align(&data_age, format::Alignment::RIGHT),
                Cell::new(&min_ts),
                Cell::new(&max_ts),
            ]));
        }
    }

    println!();
    println!("Database Statistics");
    println!("==================");
    table.printstd();
    println!();

    // Print per-exchange breakdown for key tables
    let key_tables = vec!["tickers", "trades", "orderbooks", "liquidity_depth"];
    for table_name in key_tables {
        if let Some(table_stats) = stats.get(table_name) {
            if let Some(breakdown) = table_stats.get("exchange_breakdown") {
                if let Some(breakdown_obj) = breakdown.as_object() {
                    if !breakdown_obj.is_empty() {
                        println!("{} - Per Exchange Breakdown:", table_name.to_uppercase());
                        println!("{}", "─".repeat(80));

                        let mut exchange_table = Table::new();
                        exchange_table.set_format(*format::consts::FORMAT_NO_LINESEP_WITH_TITLE);
                        exchange_table.set_titles(Row::new(vec![
                            Cell::new("Exchange").with_style(prettytable::Attr::Bold),
                            Cell::new("Count").with_style(prettytable::Attr::Bold),
                            Cell::new("Data Age").with_style(prettytable::Attr::Bold),
                            Cell::new("Latest Update").with_style(prettytable::Attr::Bold),
                        ]));

                        for (exchange, exchange_stats) in breakdown_obj {
                            let count = exchange_stats["count"].as_i64().unwrap_or(0);
                            let age = if let Some(age_minutes) = exchange_stats["data_age_minutes"].as_i64() {
                                if age_minutes < 60 {
                                    format!("{}m", age_minutes)
                                } else if age_minutes < 1440 {
                                    format!("{}h", age_minutes / 60)
                                } else {
                                    format!("{}d", age_minutes / 1440)
                                }
                            } else {
                                "-".to_string()
                            };
                            let max_ts = exchange_stats["max_timestamp"]
                                .as_str()
                                .map(|s| s.split('.').next().unwrap_or(s).replace('T', " "))
                                .unwrap_or_else(|| "-".to_string());

                            exchange_table.add_row(Row::new(vec![
                                Cell::new(exchange),
                                Cell::new_align(&count.to_string(), format::Alignment::RIGHT),
                                Cell::new_align(&age, format::Alignment::RIGHT),
                                Cell::new(&max_ts),
                            ]));
                        }

                        exchange_table.printstd();
                        println!();
                    }
                }
            }
        }
    }

    // Print metadata
    if let Some(metadata) = stats.get("metadata") {
        println!("Summary");
        println!("=======");
        let partition_count = metadata["partition_count"].as_i64().unwrap_or(0);
        println!("Total partitions: {}", partition_count);

        if let Some(top_symbols) = metadata.get("top_symbols") {
            if let Some(symbols_array) = top_symbols.as_array() {
                if !symbols_array.is_empty() {
                    println!("\nTop 10 Symbols by Activity:");
                    for (i, symbol_data) in symbols_array.iter().enumerate() {
                        let symbol = symbol_data["symbol"].as_str().unwrap_or("N/A");
                        let count = symbol_data["total_records"].as_i64().unwrap_or(0);
                        println!("  {}. {} ({} records)", i + 1, symbol, count);
                    }
                }
            }
        }

        println!(
            "\nFetched at: {}",
            metadata["fetched_at"].as_str().unwrap_or("N/A")
        );
        println!();
    }

    Ok(())
}

/// Mask password in database URL for logging
fn mask_password(url: &str) -> String {
    if let Ok(parsed) = url::Url::parse(url) {
        let mut masked = parsed.clone();
        if parsed.password().is_some() {
            let _ = masked.set_password(Some("***"));
        }
        masked.to_string()
    } else {
        url.to_string()
    }
}
