use anyhow::{Context, Result};
use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use clap::Args;
use futures::{stream, StreamExt};
use perps_core::{IPerps, Kline};
use perps_exchanges::factory;
use prettytable::{format, Cell, Row, Table};
use rust_decimal::prelude::ToPrimitive;
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::Arc;

const DAYS_PER_YEAR: f64 = 365.0;
const KLINE_PAGE_LIMIT: u32 = 1000;
const MAX_CONCURRENT_REQUESTS: usize = 8;

#[derive(Args)]
pub struct VolatilityArgs {
    /// Comma-separated list of symbols (e.g., BTC,ETH)
    #[arg(short, long)]
    pub symbols: String,

    /// Comma-separated list of exchanges (e.g., binance,bybit). Defaults to all supported exchanges.
    #[arg(short, long)]
    pub exchanges: Option<String>,

    /// Volatility estimator(s): realized, parkinson, gl, or all
    #[arg(long, default_value = "all")]
    pub method: String,

    /// Kline interval used for the calculation
    #[arg(short, long, default_value = "1h")]
    pub interval: String,

    /// Inclusive start date (YYYY-MM-DD)
    #[arg(long, conflicts_with = "range")]
    pub start_date: Option<String>,

    /// Inclusive end date (YYYY-MM-DD). Defaults to now.
    #[arg(long)]
    pub end_date: Option<String>,

    /// Lookback range ending at --end-date or now (e.g., 24h, 7d, 4w)
    #[arg(long, conflicts_with = "start_date")]
    pub range: Option<String>,

    /// Output format (table, json)
    #[arg(short, long, default_value = "table")]
    pub format: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VolatilityMethod {
    Realized,
    Parkinson,
    GarmanKlass,
}

impl VolatilityMethod {
    fn display_name(self) -> &'static str {
        match self {
            Self::Realized => "REALIZED",
            Self::Parkinson => "PARKINSON",
            Self::GarmanKlass => "GARMAN-KLASS",
        }
    }
}

#[derive(Debug)]
struct KlinePrice {
    open_time: DateTime<Utc>,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
}

#[derive(Debug, Serialize)]
struct VolatilityResult {
    symbol: String,
    exchange: String,
    interval: String,
    bars: usize,
    realized_pct: Option<f64>,
    parkinson_pct: Option<f64>,
    garman_klass_pct: Option<f64>,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
}

pub async fn execute(args: VolatilityArgs) -> Result<()> {
    let methods = parse_methods(&args.method)?;
    let periods_per_year = periods_per_year(&args.interval)?;
    let (start_date, end_date) = resolve_date_range(
        args.start_date.as_deref(),
        args.end_date.as_deref(),
        args.range.as_deref(),
        Utc::now(),
    )?;
    validate_format(&args.format)?;

    let symbols = parse_list(&args.symbols, true);
    let exchanges = match args.exchanges.as_deref() {
        Some(value) => parse_list(value, false),
        None => factory::exchange_names()
            .iter()
            .map(|name| (*name).to_string())
            .collect(),
    };

    if symbols.is_empty() {
        anyhow::bail!("At least one symbol is required");
    }
    if exchanges.is_empty() {
        anyhow::bail!("At least one exchange is required");
    }
    validate_selection(&symbols, &exchanges)?;

    let mut jobs = Vec::with_capacity(symbols.len() * exchanges.len());
    for exchange in exchanges {
        match factory::get_exchange(&exchange).await {
            Ok(client) => {
                let client: Arc<dyn IPerps + Send + Sync> = Arc::from(client);
                for symbol in &symbols {
                    jobs.push((exchange.clone(), symbol.clone(), client.clone()));
                }
            }
            Err(error) => {
                tracing::warn!(exchange, %error, "Failed to initialize exchange client");
            }
        }
    }

    if jobs.is_empty() {
        anyhow::bail!("Failed to initialize any requested exchange clients");
    }

    let interval = args.interval.clone();
    let fetched = stream::iter(jobs.into_iter().map(|(exchange, symbol, client)| {
        let interval = interval.clone();
        async move {
            let result =
                fetch_remote_klines(client.as_ref(), &symbol, &interval, start_date, end_date)
                    .await;
            (exchange, symbol, result)
        }
    }))
    .buffer_unordered(MAX_CONCURRENT_REQUESTS)
    .collect::<Vec<_>>()
    .await;

    let mut results = Vec::with_capacity(fetched.len());
    let mut successful_pairs = 0usize;
    for (exchange, symbol, fetched_prices) in fetched {
        let prices = match fetched_prices {
            Ok(prices) if !prices.is_empty() => prices,
            Ok(_) => {
                tracing::warn!(
                    exchange,
                    symbol,
                    interval = args.interval,
                    "Remote API returned no klines"
                );
                continue;
            }
            Err(error) => {
                tracing::warn!(
                    exchange,
                    symbol,
                    interval = args.interval,
                    %error,
                    "Failed to fetch remote klines"
                );
                continue;
            }
        };
        successful_pairs += 1;

        let start = prices
            .first()
            .expect("kline group cannot be empty")
            .open_time;
        let end = prices
            .last()
            .expect("kline group cannot be empty")
            .open_time;

        let mut result = VolatilityResult {
            symbol,
            exchange,
            interval: args.interval.clone(),
            bars: prices.len(),
            realized_pct: None,
            parkinson_pct: None,
            garman_klass_pct: None,
            start,
            end,
        };
        for &method in &methods {
            let (volatility, _) = calculate_volatility(&prices, method, periods_per_year);
            let volatility_pct = volatility.map(|value| value * 100.0);
            match method {
                VolatilityMethod::Realized => result.realized_pct = volatility_pct,
                VolatilityMethod::Parkinson => result.parkinson_pct = volatility_pct,
                VolatilityMethod::GarmanKlass => result.garman_klass_pct = volatility_pct,
            }
        }
        results.push(result);
    }

    if successful_pairs == 0 {
        anyhow::bail!(
            "No remote kline data returned for the requested symbols, exchanges, interval, and range"
        );
    }

    results.sort_by(|left, right| {
        left.symbol
            .cmp(&right.symbol)
            .then_with(|| left.exchange.cmp(&right.exchange))
    });
    render_results(&results, &methods, &args.format)
}

fn validate_selection(symbols: &[String], exchanges: &[String]) -> Result<()> {
    if symbols.len() > 1 && exchanges.len() > 1 {
        anyhow::bail!(
            "Invalid comparison: multiple symbols ({}) cannot be combined with multiple exchanges ({}).\n\
             Use either multiple symbols with one exchange, or one symbol with multiple exchanges.",
            symbols.len(),
            exchanges.len()
        );
    }
    Ok(())
}

fn parse_methods(value: &str) -> Result<Vec<VolatilityMethod>> {
    match value.trim().to_ascii_lowercase().as_str() {
        "realized" => Ok(vec![VolatilityMethod::Realized]),
        "parkinson" => Ok(vec![VolatilityMethod::Parkinson]),
        "gl" => Ok(vec![VolatilityMethod::GarmanKlass]),
        "all" => Ok(vec![
            VolatilityMethod::Realized,
            VolatilityMethod::Parkinson,
            VolatilityMethod::GarmanKlass,
        ]),
        other => anyhow::bail!(
            "Unknown volatility method '{}'. Supported values: realized, parkinson, gl, all",
            other
        ),
    }
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

fn parse_list(value: &str, uppercase: bool) -> Vec<String> {
    let mut values = Vec::new();
    for item in value.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let normalized = if uppercase {
            item.to_ascii_uppercase()
        } else {
            item.to_ascii_lowercase()
        };
        if !values.contains(&normalized) {
            values.push(normalized);
        }
    }
    values
}

/// Resolve CLI date inputs into an inclusive start and exclusive end.
fn resolve_date_range(
    start_date: Option<&str>,
    end_date: Option<&str>,
    range: Option<&str>,
    now: DateTime<Utc>,
) -> Result<(Option<DateTime<Utc>>, Option<DateTime<Utc>>)> {
    let start = start_date
        .map(|value| parse_date(value, "--start-date"))
        .transpose()?;
    let inclusive_end = end_date
        .map(|value| parse_date(value, "--end-date"))
        .transpose()?;
    let exclusive_end = inclusive_end
        .map(|value| {
            value
                .checked_add_signed(Duration::days(1))
                .context("--end-date is outside the supported date range")
        })
        .transpose()?;

    if let Some(value) = range {
        if start.is_some() {
            anyhow::bail!("--range cannot be combined with --start-date");
        }
        let duration = parse_range(value)?;
        let end = exclusive_end.unwrap_or(now);
        let calculated_start = end
            .checked_sub_signed(duration)
            .context("--range produces a start date outside the supported date range")?;
        return Ok((Some(calculated_start), Some(end)));
    }

    if matches!((start, exclusive_end), (Some(start), Some(end)) if start >= end) {
        anyhow::bail!("--start-date must be on or before --end-date");
    }

    Ok((start, exclusive_end))
}

fn parse_range(value: &str) -> Result<Duration> {
    if value.len() < 2 {
        anyhow::bail!(
            "Invalid --range '{}'. Expected a positive duration such as 24h, 7d, or 4w",
            value
        );
    }

    let (amount, unit) = value.split_at(value.len() - 1);
    let amount: i64 = amount.parse().with_context(|| {
        format!(
            "Invalid --range '{}'. Expected a positive duration such as 24h, 7d, or 4w",
            value
        )
    })?;
    if amount <= 0 {
        anyhow::bail!("--range must be greater than zero");
    }

    let seconds_per_unit = match unit.to_ascii_lowercase().as_str() {
        "m" => 60,
        "h" => 60 * 60,
        "d" => 24 * 60 * 60,
        "w" => 7 * 24 * 60 * 60,
        _ => anyhow::bail!(
            "Unsupported --range unit in '{}'. Supported units: m, h, d, w",
            value
        ),
    };
    let seconds = amount
        .checked_mul(seconds_per_unit)
        .context("--range is too large")?;
    Duration::try_seconds(seconds).context("--range is too large")
}

fn parse_date(value: &str, argument: &str) -> Result<DateTime<Utc>> {
    let date = NaiveDate::parse_from_str(value, "%Y-%m-%d").with_context(|| {
        format!(
            "Invalid {} '{}'. Expected format: YYYY-MM-DD",
            argument, value
        )
    })?;
    let midnight = date
        .and_hms_opt(0, 0, 0)
        .context("Failed to construct midnight for date")?;
    Ok(Utc.from_utc_datetime(&midnight))
}

fn periods_per_year(interval: &str) -> Result<f64> {
    let seconds = interval_duration(interval)?.num_seconds() as f64;
    Ok(DAYS_PER_YEAR * 24.0 * 60.0 * 60.0 / seconds)
}

fn interval_duration(interval: &str) -> Result<Duration> {
    if interval.len() < 2 {
        anyhow::bail!(
            "Invalid kline interval '{}'. Expected values such as 5m, 1h, or 1d",
            interval
        );
    }

    let (amount, unit) = interval.split_at(interval.len() - 1);
    let amount: i64 = amount.parse().with_context(|| {
        format!(
            "Invalid kline interval '{}'. Expected values such as 5m, 1h, or 1d",
            interval
        )
    })?;
    if amount <= 0 {
        anyhow::bail!("Kline interval must be greater than zero");
    }

    let seconds_per_unit = match unit {
        "m" => 60,
        "h" => 60 * 60,
        "d" => 24 * 60 * 60,
        "w" => 7 * 24 * 60 * 60,
        _ => anyhow::bail!(
            "Unsupported kline interval '{}'. Supported units: m, h, d, w",
            interval
        ),
    };
    let seconds = amount
        .checked_mul(seconds_per_unit)
        .context("Kline interval is too large")?;
    Duration::try_seconds(seconds).context("Kline interval is too large")
}

async fn fetch_remote_klines(
    client: &(dyn IPerps + Send + Sync),
    symbol: &str,
    interval: &str,
    start_date: Option<DateTime<Utc>>,
    end_date: Option<DateTime<Utc>>,
) -> Result<Vec<KlinePrice>> {
    let candle_duration = interval_duration(interval)?;
    let page_duration = candle_duration * KLINE_PAGE_LIMIT as i32;
    let end = end_date.unwrap_or_else(Utc::now);
    let start = match start_date {
        Some(start) => start,
        None => end
            .checked_sub_signed(page_duration)
            .context("Default kline lookback is outside the supported date range")?,
    };

    if start >= end {
        anyhow::bail!("Kline start time must be before end time");
    }

    let mut candles = BTreeMap::new();
    let mut cursor = start;
    while cursor < end {
        let request_end = (cursor + page_duration).min(end);
        let batch = client
            .get_klines(
                symbol,
                interval,
                Some(cursor),
                Some(request_end),
                Some(KLINE_PAGE_LIMIT),
            )
            .await
            .with_context(|| {
                format!(
                    "Remote kline request failed for {} on {}",
                    symbol,
                    client.get_name()
                )
            })?;

        let latest_open = batch.iter().map(|kline| kline.open_time).max();
        for kline in batch {
            if kline.open_time < start || kline.open_time >= end {
                continue;
            }
            if let Some(price) = convert_kline(kline) {
                candles.insert(price.open_time, price);
            }
        }

        cursor = match latest_open {
            Some(latest) if latest >= cursor && latest < request_end => {
                (latest + candle_duration).min(request_end)
            }
            _ => request_end,
        };
    }

    Ok(candles.into_values().collect())
}

fn convert_kline(kline: Kline) -> Option<KlinePrice> {
    let open = kline.open.to_f64()?;
    let high = kline.high.to_f64()?;
    let low = kline.low.to_f64()?;
    let close = kline.close.to_f64()?;
    let values = [open, high, low, close];
    if values
        .iter()
        .any(|value| !value.is_finite() || *value <= 0.0)
    {
        return None;
    }

    Some(KlinePrice {
        open_time: kline.open_time,
        open,
        high,
        low,
        close,
    })
}

fn calculate_volatility(
    prices: &[KlinePrice],
    method: VolatilityMethod,
    periods_per_year: f64,
) -> (Option<f64>, usize) {
    match method {
        VolatilityMethod::Realized => {
            let returns: Vec<f64> = prices
                .windows(2)
                .map(|window| (window[1].close / window[0].close).ln())
                .filter(|value| value.is_finite())
                .collect();
            if returns.is_empty() {
                return (None, 0);
            }

            let mean_square =
                returns.iter().map(|value| value * value).sum::<f64>() / returns.len() as f64;
            (
                Some((mean_square * periods_per_year).max(0.0).sqrt()),
                returns.len(),
            )
        }
        VolatilityMethod::Parkinson => {
            if prices.is_empty() {
                return (None, 0);
            }

            let variance = prices
                .iter()
                .map(|price| (price.high / price.low).ln().powi(2))
                .sum::<f64>()
                / (4.0 * std::f64::consts::LN_2 * prices.len() as f64);
            (
                Some((variance * periods_per_year).max(0.0).sqrt()),
                prices.len(),
            )
        }
        VolatilityMethod::GarmanKlass => {
            if prices.is_empty() {
                return (None, 0);
            }

            let variance = prices
                .iter()
                .map(|price| {
                    let high_low = (price.high / price.low).ln();
                    let close_open = (price.close / price.open).ln();
                    0.5 * high_low.powi(2)
                        - (2.0 * std::f64::consts::LN_2 - 1.0) * close_open.powi(2)
                })
                .sum::<f64>()
                / prices.len() as f64;
            (
                Some((variance * periods_per_year).max(0.0).sqrt()),
                prices.len(),
            )
        }
    }
}

fn render_results(
    results: &[VolatilityResult],
    methods: &[VolatilityMethod],
    output_format: &str,
) -> Result<()> {
    if output_format.eq_ignore_ascii_case("json") {
        println!("{}", serde_json::to_string_pretty(results)?);
        return Ok(());
    }

    let mut table = Table::new();
    table.set_format(*format::consts::FORMAT_NO_LINESEP_WITH_TITLE);
    let mut headers = vec![Cell::new("SYM"), Cell::new("EXCHANGE"), Cell::new("BARS")];
    headers.extend(
        methods
            .iter()
            .map(|method| Cell::new(method.display_name())),
    );
    table.set_titles(Row::new(headers));

    for result in results {
        let mut cells = vec![
            Cell::new(&result.symbol),
            Cell::new(&result.exchange),
            Cell::new(&result.bars.to_string()),
        ];
        for method in methods {
            let value = match method {
                VolatilityMethod::Realized => result.realized_pct,
                VolatilityMethod::Parkinson => result.parkinson_pct,
                VolatilityMethod::GarmanKlass => result.garman_klass_pct,
            };
            cells.push(Cell::new(&format_volatility(value)));
        }
        table.add_row(Row::new(cells));
    }

    let interval = results
        .first()
        .map(|result| result.interval.as_str())
        .unwrap_or("N/A");
    println!(
        "\nAnnualized Volatility (365-day basis, interval {})",
        interval
    );
    table.printstd();
    Ok(())
}

fn format_volatility(value: Option<f64>) -> String {
    value
        .map(|value| format!("{:.4}%", value))
        .unwrap_or_else(|| "N/A".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use clap::Parser;

    fn price(day: u32, open: f64, high: f64, low: f64, close: f64) -> KlinePrice {
        KlinePrice {
            open_time: Utc.with_ymd_and_hms(2025, 1, day, 0, 0, 0).unwrap(),
            open,
            high,
            low,
            close,
        }
    }

    fn assert_approx_eq(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-12,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn parses_all_methods_in_display_order() {
        assert_eq!(
            parse_methods("all").unwrap(),
            vec![
                VolatilityMethod::Realized,
                VolatilityMethod::Parkinson,
                VolatilityMethod::GarmanKlass
            ]
        );
        assert_eq!(
            parse_methods("GL").unwrap(),
            vec![VolatilityMethod::GarmanKlass]
        );
        assert!(parse_methods("gk").is_err());
    }

    #[test]
    fn parses_and_deduplicates_lists() {
        assert_eq!(
            parse_list(" btc, ETH,btc ", true),
            vec!["BTC".to_string(), "ETH".to_string()]
        );
        assert_eq!(
            parse_list(" Binance,bybit,BINANCE ", false),
            vec!["binance".to_string(), "bybit".to_string()]
        );
    }

    #[test]
    fn enforces_single_comparison_axis() {
        let one_symbol = vec!["BTC".to_string()];
        let many_symbols = vec!["BTC".to_string(), "ETH".to_string()];
        let one_exchange = vec!["binance".to_string()];
        let many_exchanges = vec!["binance".to_string(), "bybit".to_string()];

        assert!(validate_selection(&many_symbols, &one_exchange).is_ok());
        assert!(validate_selection(&one_symbol, &many_exchanges).is_ok());
        assert!(validate_selection(&one_symbol, &one_exchange).is_ok());
        assert!(validate_selection(&many_symbols, &many_exchanges).is_err());
    }

    #[test]
    fn formats_volatility_as_percentage() {
        assert_eq!(format_volatility(Some(25.88249)), "25.8825%");
        assert_eq!(format_volatility(None), "N/A");
    }

    #[test]
    fn calculates_periods_per_year() {
        assert_approx_eq(periods_per_year("1d").unwrap(), 365.0);
        assert_approx_eq(periods_per_year("1h").unwrap(), 365.0 * 24.0);
        assert_approx_eq(periods_per_year("5m").unwrap(), 365.0 * 24.0 * 12.0);
        assert!(periods_per_year("daily").is_err());
        assert!(periods_per_year("0h").is_err());
    }

    #[test]
    fn volatility_cli_defaults_to_hourly_klines() {
        let cli = crate::cli::Cli::try_parse_from([
            "perps-stats",
            "stats",
            "volatility",
            "--symbols",
            "BTC",
        ])
        .unwrap();
        let crate::cli::Commands::Stats {
            command: crate::cli::StatsCommands::Volatility(args),
        } = cli.command
        else {
            panic!("expected stats volatility command");
        };

        assert_eq!(args.interval, "1h");
        assert_eq!(args.method, "all");
        assert!(args.start_date.is_none());
        assert!(args.end_date.is_none());
        assert!(args.range.is_none());
    }

    #[test]
    fn date_range_is_inclusive_of_end_date() {
        let now = Utc.with_ymd_and_hms(2025, 2, 1, 12, 0, 0).unwrap();
        let (start, end) =
            resolve_date_range(Some("2025-01-10"), Some("2025-01-10"), None, now).unwrap();

        assert_eq!(
            start.unwrap(),
            Utc.with_ymd_and_hms(2025, 1, 10, 0, 0, 0).unwrap()
        );
        assert_eq!(
            end.unwrap(),
            Utc.with_ymd_and_hms(2025, 1, 11, 0, 0, 0).unwrap()
        );
    }

    #[test]
    fn validates_date_range() {
        let now = Utc.with_ymd_and_hms(2025, 2, 1, 12, 0, 0).unwrap();

        assert!(resolve_date_range(Some("2025-01-11"), Some("2025-01-10"), None, now).is_err());
        assert!(resolve_date_range(Some("01/10/2025"), None, None, now).is_err());
        assert_eq!(
            resolve_date_range(None, None, None, now).unwrap(),
            (None, None)
        );
    }

    #[test]
    fn range_uses_now_as_default_end() {
        let now = Utc.with_ymd_and_hms(2025, 2, 1, 12, 30, 0).unwrap();
        let (start, end) = resolve_date_range(None, None, Some("7d"), now).unwrap();

        assert_eq!(start.unwrap(), now - Duration::days(7));
        assert_eq!(end.unwrap(), now);
    }

    #[test]
    fn range_can_be_anchored_to_inclusive_end_date() {
        let now = Utc.with_ymd_and_hms(2025, 2, 10, 12, 0, 0).unwrap();
        let (start, end) = resolve_date_range(None, Some("2025-01-31"), Some("24h"), now).unwrap();

        assert_eq!(
            start.unwrap(),
            Utc.with_ymd_and_hms(2025, 1, 31, 0, 0, 0).unwrap()
        );
        assert_eq!(
            end.unwrap(),
            Utc.with_ymd_and_hms(2025, 2, 1, 0, 0, 0).unwrap()
        );
    }

    #[test]
    fn validates_range() {
        let now = Utc.with_ymd_and_hms(2025, 2, 1, 12, 0, 0).unwrap();

        assert_eq!(parse_range("90m").unwrap(), Duration::minutes(90));
        assert_eq!(parse_range("24h").unwrap(), Duration::hours(24));
        assert_eq!(parse_range("7d").unwrap(), Duration::days(7));
        assert_eq!(parse_range("4W").unwrap(), Duration::weeks(4));
        assert!(parse_range("0d").is_err());
        assert!(parse_range("1y").is_err());
        assert!(resolve_date_range(Some("2025-01-01"), None, Some("7d"), now).is_err());
    }

    #[test]
    fn range_conflicts_with_start_date_in_cli() {
        assert!(crate::cli::Cli::try_parse_from([
            "perps-stats",
            "stats",
            "volatility",
            "--symbols",
            "BTC",
            "--range",
            "7d",
            "--start-date",
            "2025-01-01"
        ])
        .is_err());
    }

    #[test]
    fn calculates_realized_volatility() {
        let prices = vec![
            price(1, 99.0, 101.0, 98.0, 100.0),
            price(2, 100.0, 111.0, 99.0, 110.0),
            price(3, 110.0, 122.0, 109.0, 121.0),
        ];
        let (actual, observations) =
            calculate_volatility(&prices, VolatilityMethod::Realized, 365.0);
        let expected = 1.1_f64.ln() * 365.0_f64.sqrt();

        assert_eq!(observations, 2);
        assert_approx_eq(actual.unwrap(), expected);
    }

    #[test]
    fn calculates_parkinson_volatility() {
        let prices = vec![
            price(1, 100.0, 110.0, 100.0, 105.0),
            price(2, 105.0, 110.0, 100.0, 106.0),
        ];
        let (actual, observations) =
            calculate_volatility(&prices, VolatilityMethod::Parkinson, 365.0);
        let expected = (1.1_f64.ln().powi(2) / (4.0 * std::f64::consts::LN_2) * 365.0).sqrt();

        assert_eq!(observations, 2);
        assert_approx_eq(actual.unwrap(), expected);
    }

    #[test]
    fn calculates_garman_klass_volatility() {
        let prices = vec![price(1, 100.0, 110.0, 90.0, 105.0)];
        let (actual, observations) =
            calculate_volatility(&prices, VolatilityMethod::GarmanKlass, 365.0);
        let expected_variance = 0.5 * (110.0_f64 / 90.0).ln().powi(2)
            - (2.0 * std::f64::consts::LN_2 - 1.0) * (105.0_f64 / 100.0).ln().powi(2);

        assert_eq!(observations, 1);
        assert_approx_eq(actual.unwrap(), (expected_variance * 365.0).sqrt());
    }

    #[test]
    fn realized_requires_two_closes() {
        let prices = vec![price(1, 100.0, 110.0, 90.0, 105.0)];
        assert_eq!(
            calculate_volatility(&prices, VolatilityMethod::Realized, 365.0),
            (None, 0)
        );
    }
}
