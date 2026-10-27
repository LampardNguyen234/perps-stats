use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use clap::Args;
use perps_core::{FundingRate, IPerps};
use perps_exchanges::factory;
use prettytable::{format, Cell, Row, Table};
use rust_decimal::prelude::ToPrimitive;
use serde::Serialize;
use std::collections::BTreeMap;

use crate::commands::volatility::resolve_date_range;

const PAGE_LIMIT: u32 = 1000;
const DEFAULT_LOOKBACK_DAYS: i64 = 7;

#[derive(Args)]
pub struct FundingHistoryArgs {
    /// Exchange name (e.g., binance)
    #[arg(short, long)]
    pub exchange: String,

    /// Symbol (e.g., BTC)
    #[arg(short, long)]
    pub symbol: String,

    /// Inclusive start date (YYYY-MM-DD)
    #[arg(long, conflicts_with = "range")]
    pub start_date: Option<String>,

    /// Inclusive end date (YYYY-MM-DD). Defaults to now.
    #[arg(long)]
    pub end_date: Option<String>,

    /// Lookback range ending at --end-date or now (e.g., 24h, 7d, 4w). Defaults to 7d.
    #[arg(long, conflicts_with = "start_date")]
    pub range: Option<String>,

    /// Output format (table, json)
    #[arg(short, long, default_value = "table")]
    pub format: String,
}

#[derive(Debug, Serialize)]
struct FundingRow {
    symbol: String,
    exchange: String,
    funding_time: DateTime<Utc>,
    funding_rate_pct: f64,
    predicted_rate_pct: f64,
    funding_interval_hours: i32,
}

pub async fn execute(args: FundingHistoryArgs) -> Result<()> {
    validate_format(&args.format)?;

    let symbol = args.symbol.trim().to_ascii_uppercase();
    if symbol.is_empty() {
        anyhow::bail!("--symbol is required");
    }
    let exchange = args.exchange.trim().to_ascii_lowercase();
    if exchange.is_empty() {
        anyhow::bail!("--exchange is required");
    }

    let (start_date, end_date) = resolve_date_range(
        args.start_date.as_deref(),
        args.end_date.as_deref(),
        args.range.as_deref(),
        Utc::now(),
    )?;

    let client = factory::get_exchange(&exchange)
        .await
        .with_context(|| format!("Failed to initialize exchange client for {}", exchange))?;

    let rates = fetch_funding_history(client.as_ref(), &symbol, start_date, end_date).await?;

    if rates.is_empty() {
        anyhow::bail!(
            "No funding rate history returned for {} on {} in the requested range",
            symbol,
            exchange
        );
    }

    let rows: Vec<FundingRow> = rates
        .into_iter()
        .map(|rate| FundingRow {
            symbol: symbol.clone(),
            exchange: exchange.clone(),
            funding_time: rate.funding_time,
            funding_rate_pct: rate.funding_rate.to_f64().unwrap_or(0.0) * 100.0,
            predicted_rate_pct: rate.predicted_rate.to_f64().unwrap_or(0.0) * 100.0,
            funding_interval_hours: rate.funding_interval,
        })
        .collect();

    render(&rows, &args.format)
}

/// Pages through `get_funding_rate_history` in fixed-size time windows, deduping
/// by exact funding_time (an exchange may re-return the window's last candle on
/// the next page). The 1-hour-per-entry page width is a conservative upper bound
/// on funding frequency (shortest known interval across supported exchanges);
/// it just controls page count, not correctness.
async fn fetch_funding_history(
    client: &(dyn IPerps + Send + Sync),
    symbol: &str,
    start_date: Option<DateTime<Utc>>,
    end_date: Option<DateTime<Utc>>,
) -> Result<Vec<FundingRate>> {
    let end = end_date.unwrap_or_else(Utc::now);
    let start = match start_date {
        Some(start) => start,
        None => end - Duration::days(DEFAULT_LOOKBACK_DAYS),
    };

    if start >= end {
        anyhow::bail!("Funding history start time must be before end time");
    }

    let page_duration = Duration::hours(1) * PAGE_LIMIT as i32;
    let mut rates = BTreeMap::new();
    let mut cursor = start;
    while cursor < end {
        let request_end = (cursor + page_duration).min(end);
        let batch = client
            .get_funding_rate_history(symbol, Some(cursor), Some(request_end), Some(PAGE_LIMIT))
            .await
            .with_context(|| {
                format!(
                    "Funding rate history request failed for {} on {}",
                    symbol,
                    client.get_name()
                )
            })?;

        let latest_time = batch.iter().map(|rate| rate.funding_time).max();
        for rate in batch {
            if rate.funding_time < start || rate.funding_time >= end {
                continue;
            }
            rates.insert(rate.funding_time, rate);
        }

        cursor = match latest_time {
            Some(latest) if latest >= cursor && latest < request_end => {
                (latest + Duration::seconds(1)).min(request_end)
            }
            _ => request_end,
        };
    }

    Ok(rates.into_values().collect())
}

fn validate_format(value: &str) -> Result<()> {
    match value.to_ascii_lowercase().as_str() {
        "table" | "json" => Ok(()),
        other => anyhow::bail!(
            "Unknown output format '{}'. Supported values: table, json",
            other
        ),
    }
}

fn render(rows: &[FundingRow], format: &str) -> Result<()> {
    match format.to_ascii_lowercase().as_str() {
        "json" => {
            println!("{}", serde_json::to_string_pretty(rows)?);
        }
        _ => {
            let mut table = Table::new();
            table.set_format(*format::consts::FORMAT_BOX_CHARS);
            table.set_titles(Row::new(vec![
                Cell::new("SYMBOL"),
                Cell::new("EXCHANGE"),
                Cell::new("FUNDING TIME"),
                Cell::new("FUNDING RATE"),
                Cell::new("PREDICTED RATE"),
                Cell::new("INTERVAL (H)"),
            ]));
            for row in rows {
                table.add_row(Row::new(vec![
                    Cell::new(&row.symbol),
                    Cell::new(&row.exchange),
                    Cell::new(&row.funding_time.format("%Y-%m-%d %H:%M:%S UTC").to_string()),
                    Cell::new(&format!("{:.6}%", row.funding_rate_pct)),
                    Cell::new(&format!("{:.6}%", row.predicted_rate_pct)),
                    Cell::new(&row.funding_interval_hours.to_string()),
                ]));
            }
            table.printstd();
        }
    }
    Ok(())
}
