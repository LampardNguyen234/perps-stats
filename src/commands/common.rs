use anyhow::Result;
use perps_core::IPerps;
use perps_database::{PgPool, PostgresRepository};

/// Configuration for periodic fetcher output
pub enum OutputConfig {
    File {
        output_file: String,
        output_dir: Option<String>,
        format: String,
    },
    Database {
        repository: PostgresRepository,
    },
}

/// Validate symbols against the exchange - filter out unsupported symbols
pub async fn validate_symbols(client: &dyn IPerps, symbols: &[String]) -> Result<Vec<String>> {
    let mut valid_symbols = Vec::new();
    let mut invalid_symbols = Vec::new();

    for symbol in symbols {
        match client.is_supported(symbol).await {
            Ok(true) => {
                tracing::debug!("✓ Symbol {} is supported on {}", symbol, client.get_name());
                valid_symbols.push(symbol.clone());
            }
            Ok(false) => {
                tracing::warn!("✗ Symbol {} is not supported on {} - skipping", symbol, client.get_name());
                invalid_symbols.push(symbol.clone());
            }
            Err(e) => {
                tracing::error!("Failed to check if symbol {} is supported: {} - skipping", symbol, e);
                invalid_symbols.push(symbol.clone());
            }
        }
    }

    // Log summary
    if !invalid_symbols.is_empty() {
        eprintln!(
            "Warning: {} symbol(s) not supported on {}: {}",
            invalid_symbols.len(),
            client.get_name(),
            invalid_symbols.join(", ")
        );
    }

    if !valid_symbols.is_empty() {
        tracing::info!(
            "Validated {} symbol(s) on {}: {}",
            valid_symbols.len(),
            client.get_name(),
            valid_symbols.join(", ")
        );
    }

    Ok(valid_symbols)
}

/// Determine output configuration from command-line arguments
///
/// This function validates the output configuration for periodic fetching:
/// - If format is "table" (default), requires database URL
/// - If format is "csv" or "excel", requires output file
/// - Creates output directory if specified
pub async fn determine_output_config(
    format: &str,
    output: Option<String>,
    output_dir: &Option<String>,
    database_url: Option<String>,
) -> Result<OutputConfig> {
    let format_lower = format.to_lowercase();

    if format_lower == "table" {
        // When format is table (default) and interval is set, use database
        if let Some(db_url) = database_url {
            let pool = PgPool::connect(&db_url).await?;
            let repository = PostgresRepository::new(pool);
            Ok(OutputConfig::Database { repository })
        } else {
            anyhow::bail!("Periodic fetching (--interval) without explicit format requires --database-url or DATABASE_URL environment variable");
        }
    } else if format_lower == "csv" || format_lower == "excel" {
        let output_file = output.ok_or_else(|| anyhow::anyhow!("Periodic fetching (--interval) with --format csv/excel requires --output <file>"))?;
        if let Some(ref dir) = output_dir {
            std::fs::create_dir_all(dir)?;
        }
        Ok(OutputConfig::File {
            output_file,
            output_dir: output_dir.clone(),
            format: format_lower,
        })
    } else {
        anyhow::bail!("Periodic fetching (--interval) requires --format csv, --format excel, or database storage (no format specified)");
    }
}

/// Format output description for logging
pub fn format_output_description(config: &OutputConfig) -> String {
    match config {
        OutputConfig::File { output_file, output_dir, format } => {
            format!("format: {}, output: {}{}",
                format,
                output_dir.as_ref().map(|d| format!("{}/", d)).unwrap_or_default(),
                output_file
            )
        }
        OutputConfig::Database { .. } => "database".to_string(),
    }
}
