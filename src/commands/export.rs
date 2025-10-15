use anyhow::Result;
use chrono::Utc;
use clap::Args;
use rust_xlsxwriter::{Format, Workbook};
use sqlx::{Column, PgPool, Row as SqlxRow};
use std::fs;
use std::path::PathBuf;

#[derive(Args)]
pub struct ExportArgs {
    /// Database URL for connecting to PostgreSQL
    #[arg(long, env = "DATABASE_URL")]
    pub database_url: String,

    /// Output directory for exported files
    #[arg(short, long, default_value = "./exports")]
    pub output_dir: String,

    /// Export format (csv, xlsx)
    #[arg(short, long, default_value = "xlsx")]
    pub format: String,

    /// Create separate files for each table (applies to xlsx format only)
    /// When enabled, creates individual Excel files (e.g., tickers.xlsx, trades.xlsx)
    /// When disabled (default), creates a single Excel file with multiple sheets
    #[arg(long, default_value = "false")]
    pub separate_files: bool,

    /// Comma-separated list of tables to export (default: all)
    /// Available: exchanges,markets,tickers,orderbooks,trades,funding_rates,liquidity_depth,klines,slippage
    #[arg(short, long)]
    pub tables: Option<String>,

    /// Maximum rows per table (0 = unlimited)
    #[arg(short = 'n', long, default_value = "0")]
    pub max_rows: usize,

    /// Time filter: export only data from last N hours (0 = all data)
    #[arg(long, default_value = "0")]
    pub last_hours: i64,
}

/// All exportable tables with their column definitions
const ALL_TABLES: &[&str] = &[
    "exchanges",
    "markets",
    "tickers",
    "orderbooks",
    "trades",
    "funding_rates",
    "liquidity_depth",
    "klines",
    "slippage",
    "kline_discovery_cache",
];

/// Tables that have timestamp column for filtering
const TIMESTAMPED_TABLES: &[&str] = &[
    "tickers",
    "orderbooks",
    "trades",
    "funding_rates",
    "liquidity_depth",
    "klines",
    "slippage",
    "open_interest",
];

pub async fn execute(args: ExportArgs) -> Result<()> {
    tracing::info!("Starting database export");
    tracing::info!("Output directory: {}", args.output_dir);
    tracing::info!("Format: {}", args.format);

    // Validate format
    let format = args.format.to_lowercase();
    if format != "csv" && format != "xlsx" {
        anyhow::bail!("Invalid format: {}. Must be 'csv' or 'xlsx'", args.format);
    }

    // Create output directory
    fs::create_dir_all(&args.output_dir)?;
    tracing::info!("✓ Created output directory");

    // Connect to database
    tracing::info!("Connecting to database...");
    let pool = PgPool::connect(&args.database_url).await?;
    tracing::info!("✓ Connected to database");

    // Determine which tables to export
    let tables_to_export = if let Some(ref table_list) = args.tables {
        let requested: Vec<&str> = table_list.split(',').map(|s| s.trim()).collect();
        // Validate requested tables
        for table in &requested {
            if !ALL_TABLES.contains(table) {
                anyhow::bail!(
                    "Invalid table: {}. Available tables: {}",
                    table,
                    ALL_TABLES.join(", ")
                );
            }
        }
        requested
    } else {
        ALL_TABLES.to_vec()
    };

    tracing::info!("Tables to export: {}", tables_to_export.join(", "));

    // Export based on format
    match format.as_str() {
        "csv" => {
            export_to_csv(&pool, &args.output_dir, &tables_to_export, args.max_rows, args.last_hours).await?;
        }
        "xlsx" => {
            if args.separate_files {
                export_to_xlsx_separate(&pool, &args.output_dir, &tables_to_export, args.max_rows, args.last_hours).await?;
            } else {
                export_to_xlsx_combined(&pool, &args.output_dir, &tables_to_export, args.max_rows, args.last_hours).await?;
            }
        }
        _ => unreachable!(),
    }

    tracing::info!("✓ Export completed successfully");
    Ok(())
}

/// Export all tables to separate CSV files
async fn export_to_csv(
    pool: &PgPool,
    output_dir: &str,
    tables: &[&str],
    max_rows: usize,
    last_hours: i64,
) -> Result<()> {
    for table in tables {
        let output_path = format!("{}/{}.csv", output_dir, table);
        tracing::info!("Exporting {} to {}...", table, output_path);

        // Build query
        let query = build_query(table, max_rows, last_hours);

        // Fetch data
        let rows = sqlx::query(&query).fetch_all(pool).await?;

        if rows.is_empty() {
            tracing::warn!("Table {} is empty, skipping", table);
            continue;
        }

        // Create CSV writer
        let mut wtr = csv::Writer::from_path(&output_path)?;

        // Write headers
        let columns: Vec<String> = rows[0]
            .columns()
            .iter()
            .map(|col| col.name().to_string())
            .collect();
        wtr.write_record(&columns)?;

        // Write data rows
        for row in &rows {
            let mut record = Vec::new();
            for col in row.columns() {
                let value = format_cell_value(&row, col.name());
                record.push(value);
            }
            wtr.write_record(&record)?;
        }

        wtr.flush()?;
        tracing::info!("✓ Exported {} rows to {}", rows.len(), output_path);
    }

    Ok(())
}

/// Export each table to a separate Excel file
async fn export_to_xlsx_separate(
    pool: &PgPool,
    output_dir: &str,
    tables: &[&str],
    max_rows: usize,
    last_hours: i64,
) -> Result<()> {
    for table in tables {
        let output_path = PathBuf::from(output_dir).join(format!("{}.xlsx", table));
        tracing::info!("Exporting table {} to {:?}...", table, output_path);

        // Build query
        let query = build_query(table, max_rows, last_hours);

        // Fetch data
        let rows = sqlx::query(&query).fetch_all(pool).await?;

        if rows.is_empty() {
            tracing::warn!("Table {} is empty, skipping", table);
            continue;
        }

        // Create workbook
        let mut workbook = Workbook::new();

        // Create formats
        let header_format = Format::new()
            .set_bold()
            .set_background_color(rust_xlsxwriter::Color::RGB(0xD3D3D3));

        // Create worksheet (use table name as sheet name)
        let worksheet = workbook.add_worksheet();
        worksheet.set_name(*table)?;

        // Write headers
        let columns: Vec<String> = rows[0]
            .columns()
            .iter()
            .map(|col| col.name().to_string())
            .collect();

        for (col_idx, col_name) in columns.iter().enumerate() {
            worksheet.write_string_with_format(0, col_idx as u16, col_name, &header_format)?;
        }

        // Write data rows
        for (row_idx, row) in rows.iter().enumerate() {
            for (col_idx, col) in row.columns().iter().enumerate() {
                let value = format_cell_value(row, col.name());
                worksheet.write_string((row_idx + 1) as u32, col_idx as u16, &value)?;
            }
        }

        // Auto-fit columns (approximate)
        for col_idx in 0..columns.len() {
            worksheet.set_column_width(col_idx as u16, 15)?;
        }

        // Save workbook
        workbook.save(&output_path)?;
        tracing::info!("✓ Exported {} rows to {:?}", rows.len(), output_path);
    }

    Ok(())
}

/// Export all tables to a single Excel workbook with multiple sheets
async fn export_to_xlsx_combined(
    pool: &PgPool,
    output_dir: &str,
    tables: &[&str],
    max_rows: usize,
    last_hours: i64,
) -> Result<()> {
    let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
    let output_path = PathBuf::from(output_dir).join(format!("perps_export_{}.xlsx", timestamp));
    tracing::info!("Creating Excel workbook: {:?}", output_path);

    let mut workbook = Workbook::new();

    // Create formats
    let header_format = Format::new()
        .set_bold()
        .set_background_color(rust_xlsxwriter::Color::RGB(0xD3D3D3));

    for table in tables {
        tracing::info!("Exporting table: {}", table);

        // Build query
        let query = build_query(table, max_rows, last_hours);

        // Fetch data
        let rows = sqlx::query(&query).fetch_all(pool).await?;

        if rows.is_empty() {
            tracing::warn!("Table {} is empty, skipping", table);
            continue;
        }

        // Create worksheet
        let worksheet = workbook.add_worksheet();
        worksheet.set_name(*table)?;

        // Write headers
        let columns: Vec<String> = rows[0]
            .columns()
            .iter()
            .map(|col| col.name().to_string())
            .collect();

        for (col_idx, col_name) in columns.iter().enumerate() {
            worksheet.write_string_with_format(0, col_idx as u16, col_name, &header_format)?;
        }

        // Write data rows
        for (row_idx, row) in rows.iter().enumerate() {
            for (col_idx, col) in row.columns().iter().enumerate() {
                let value = format_cell_value(row, col.name());
                worksheet.write_string((row_idx + 1) as u32, col_idx as u16, &value)?;
            }
        }

        // Auto-fit columns (approximate)
        for col_idx in 0..columns.len() {
            worksheet.set_column_width(col_idx as u16, 15)?;
        }

        tracing::info!("✓ Exported {} rows to sheet '{}'", rows.len(), table);
    }

    workbook.save(&output_path)?;
    tracing::info!("✓ Saved workbook to {:?}", output_path);

    Ok(())
}

/// Build SQL query for a table with optional filters
fn build_query(table: &str, max_rows: usize, last_hours: i64) -> String {
    let mut query = format!("SELECT * FROM {}", table);

    // Add time filter if specified and table has timestamp
    if last_hours > 0 && TIMESTAMPED_TABLES.contains(&table) {
        query.push_str(&format!(
            " WHERE ts > NOW() - INTERVAL '{} hours'",
            last_hours
        ));
    }

    // Add ORDER BY for timestamped tables
    if TIMESTAMPED_TABLES.contains(&table) {
        query.push_str(" ORDER BY ts DESC");
    } else if table == "exchanges" || table == "markets" {
        query.push_str(" ORDER BY id");
    }

    // Add row limit if specified
    if max_rows > 0 {
        query.push_str(&format!(" LIMIT {}", max_rows));
    }

    query
}

/// Format a cell value from a sqlx Row for CSV/Excel export
fn format_cell_value(row: &sqlx::postgres::PgRow, col_name: &str) -> String {
    use sqlx::TypeInfo;

    let col = row
        .columns()
        .iter()
        .find(|c| c.name() == col_name)
        .unwrap();

    let type_name = col.type_info().name();

    // Handle different PostgreSQL types
    match type_name {
        "INT4" | "INTEGER" => {
            if let Ok(val) = row.try_get::<i32, _>(col_name) {
                return val.to_string();
            }
        }
        "INT8" | "BIGINT" => {
            if let Ok(val) = row.try_get::<i64, _>(col_name) {
                return val.to_string();
            }
        }
        "BOOL" | "BOOLEAN" => {
            if let Ok(val) = row.try_get::<bool, _>(col_name) {
                return val.to_string();
            }
        }
        "NUMERIC" | "DECIMAL" => {
            if let Ok(val) = row.try_get::<rust_decimal::Decimal, _>(col_name) {
                return val.to_string();
            }
        }
        "TEXT" | "VARCHAR" => {
            if let Ok(val) = row.try_get::<String, _>(col_name) {
                return val;
            }
        }
        "TIMESTAMPTZ" | "TIMESTAMP" => {
            if let Ok(val) = row.try_get::<chrono::DateTime<Utc>, _>(col_name) {
                return val.to_rfc3339();
            }
        }
        "JSONB" | "JSON" => {
            if let Ok(val) = row.try_get::<serde_json::Value, _>(col_name) {
                return val.to_string();
            }
        }
        _ => {}
    }

    // Fallback: try to get as string
    if let Ok(val) = row.try_get::<String, _>(col_name) {
        return val;
    }

    // Return empty string if unable to parse
    String::new()
}
