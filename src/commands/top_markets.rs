use anyhow::{bail, Result};
use chrono::Utc;
use futures::{stream, StreamExt};
use perps_core::{IPerps, MarketStats};
use perps_exchanges::get_exchange;
use prettytable::{format, Cell, Row, Table};
use rust_decimal::Decimal;
use serde_json;
use std::sync::Arc;

use super::volatility::{self, VolatilityMethod};

const MAX_CONCURRENT_VOL_REQUESTS: usize = 8;
const MAX_CONCURRENT_OI_REQUESTS: usize = 20;

/// Arguments for the top-markets command
pub struct TopMarketsArgs {
    pub exchange: String,
    pub limit: usize,
    pub by: String,
    pub vol_method: String,
    pub interval: String,
    pub range: String,
    pub format: String,
}

/// Realized/Parkinson/Garman-Klass annualized volatility (%), one Option per requested method.
struct Volatility {
    realized_pct: Option<f64>,
    parkinson_pct: Option<f64>,
    garman_klass_pct: Option<f64>,
}

struct RankedMarket {
    stats: MarketStats,
    vol: Volatility,
}

pub async fn execute(args: TopMarketsArgs) -> Result<()> {
    let TopMarketsArgs {
        exchange,
        limit,
        by,
        vol_method,
        interval,
        range,
        format,
    } = args;

    let exchange = exchange.trim();
    if exchange.is_empty() {
        bail!("--exchange must be a single, non-empty exchange name");
    }
    if exchange.contains(',') {
        bail!("--exchange accepts a single exchange name, not a list");
    }

    let by = by.to_lowercase();
    if by != "volume" && by != "oi" {
        bail!("--by must be either \"volume\" or \"oi\", got \"{}\"", by);
    }

    // Reuse the exact same volatility formulas as `stats volatility`.
    let methods = volatility::parse_methods(&vol_method)?;
    let periods_per_year = volatility::periods_per_year(&interval)?;
    let lookback = volatility::parse_range(&range)?;
    let end = Utc::now();
    let start = end
        .checked_sub_signed(lookback)
        .ok_or_else(|| anyhow::anyhow!("--range is too large"))?;

    let client = get_exchange(exchange).await?;
    let client: Arc<dyn IPerps + Send + Sync> = Arc::from(client);
    let mut stats = client.get_all_market_stats().await?;

    // Binance (and possibly others) leaves open_interest at zero in
    // get_all_market_stats (no bulk OI endpoint). Backfill it per symbol via
    // get_open_interest: for the full catalog when ranking by OI (needed for
    // correct sorting), or just the already-truncated top N when ranking by
    // volume (OI is then only for display, so no need to fetch it for every
    // symbol on the exchange).
    if by == "oi" {
        backfill_open_interest(&client, &mut stats).await;
    }

    stats.sort_by(|a, b| {
        let (lhs, rhs) = if by == "volume" {
            (b.turnover_24h, a.turnover_24h)
        } else {
            (b.open_interest, a.open_interest)
        };
        lhs.cmp(&rhs)
    });
    stats.truncate(limit);

    if by == "volume" {
        backfill_open_interest(&client, &mut stats).await;
    }

    let vols = stream::iter(stats.iter().map(|s| {
        let client = client.clone();
        let symbol = s.symbol.clone();
        let interval = interval.clone();
        let methods = methods.clone();
        async move {
            let result =
                volatility::fetch_remote_klines(client.as_ref(), &symbol, &interval, Some(start), Some(end))
                    .await;
            let prices = match result {
                Ok(prices) if !prices.is_empty() => prices,
                Ok(_) => return Volatility::empty(),
                Err(error) => {
                    tracing::warn!(symbol, %error, "Failed to fetch klines for volatility");
                    return Volatility::empty();
                }
            };

            let mut vol = Volatility::empty();
            for method in &methods {
                let (value, _) = volatility::calculate_volatility(&prices, *method, periods_per_year);
                let value_pct = value.map(|v| v * 100.0);
                match method {
                    VolatilityMethod::Realized => vol.realized_pct = value_pct,
                    VolatilityMethod::Parkinson => vol.parkinson_pct = value_pct,
                    VolatilityMethod::GarmanKlass => vol.garman_klass_pct = value_pct,
                }
            }
            vol
        }
    }))
    .buffer_unordered(MAX_CONCURRENT_VOL_REQUESTS)
    .collect::<Vec<_>>()
    .await;

    let ranked: Vec<RankedMarket> = stats
        .into_iter()
        .zip(vols)
        .map(|(stats, vol)| RankedMarket { stats, vol })
        .collect();

    match format.to_lowercase().as_str() {
        "json" => display_json(&ranked, &methods),
        "csv" => display_csv(&ranked, &methods)?,
        _ => display_table(&ranked, &methods, exchange, &by, &interval, &range),
    }

    Ok(())
}

/// Fetches open interest per symbol (bounded concurrency) and writes it back
/// into `stats` in place. Only call this on the set of symbols actually
/// needed (full catalog for --by oi, or the already-truncated top N for
/// --by volume) to avoid hammering exchanges with no bulk OI endpoint.
async fn backfill_open_interest(client: &Arc<dyn IPerps + Send + Sync>, stats: &mut [MarketStats]) {
    let targets: Vec<(usize, String)> = stats
        .iter()
        .enumerate()
        .map(|(i, s)| (i, s.symbol.clone()))
        .collect();

    let results: Vec<(usize, Decimal)> = stream::iter(targets.into_iter().map(|(i, symbol)| {
        let client = client.clone();
        async move {
            let oi = client
                .get_open_interest(&symbol)
                .await
                .map(|oi| oi.open_interest)
                .unwrap_or(Decimal::ZERO);
            (i, oi)
        }
    }))
    .buffer_unordered(MAX_CONCURRENT_OI_REQUESTS)
    .collect()
    .await;

    for (i, oi) in results {
        stats[i].open_interest = oi;
    }
}

impl Volatility {
    fn empty() -> Self {
        Self {
            realized_pct: None,
            parkinson_pct: None,
            garman_klass_pct: None,
        }
    }

    fn value(&self, method: VolatilityMethod) -> Option<f64> {
        match method {
            VolatilityMethod::Realized => self.realized_pct,
            VolatilityMethod::Parkinson => self.parkinson_pct,
            VolatilityMethod::GarmanKlass => self.garman_klass_pct,
        }
    }
}

fn vol_json_key(method: VolatilityMethod) -> &'static str {
    match method {
        VolatilityMethod::Realized => "realized_vol_pct",
        VolatilityMethod::Parkinson => "parkinson_vol_pct",
        VolatilityMethod::GarmanKlass => "garman_klass_vol_pct",
    }
}

fn display_table(
    ranked: &[RankedMarket],
    methods: &[VolatilityMethod],
    exchange: &str,
    by: &str,
    interval: &str,
    range: &str,
) {
    let mut table = Table::new();
    table.set_format(*format::consts::FORMAT_NO_LINESEP_WITH_TITLE);
    let mut headers = vec![
        Cell::new("RANK"),
        Cell::new("SYM"),
        Cell::new("VOLUME"),
        Cell::new("OI"),
        Cell::new("PRICE"),
        Cell::new("CHG%"),
    ];
    headers.extend(methods.iter().map(|method| Cell::new(method.display_name())));
    table.set_titles(Row::new(headers));

    for (i, r) in ranked.iter().enumerate() {
        let s = &r.stats;
        let mut cells = vec![
            Cell::new(&(i + 1).to_string()),
            Cell::new(&s.symbol),
            Cell::new(&format_usd(s.turnover_24h)),
            Cell::new(&s.open_interest.to_string()),
            Cell::new(&s.last_price.to_string()),
            Cell::new(&format!(
                "{:.2}%",
                s.price_change_pct * rust_decimal::Decimal::new(100, 0)
            )),
        ];
        cells.extend(
            methods
                .iter()
                .map(|method| Cell::new(&format_volatility(r.vol.value(*method)))),
        );
        table.add_row(Row::new(cells));
    }

    println!(
        "Top {} markets on {} by {} (volatility: {} interval, {} lookback)",
        ranked.len(),
        exchange,
        by,
        interval,
        range
    );
    table.printstd();
}

fn format_usd(value: rust_decimal::Decimal) -> String {
    format!("${:.2}", value)
}

fn format_volatility(value: Option<f64>) -> String {
    value
        .map(|value| format!("{:.4}%", value))
        .unwrap_or_else(|| "N/A".to_string())
}

fn display_json(ranked: &[RankedMarket], methods: &[VolatilityMethod]) {
    let values: Vec<serde_json::Value> = ranked
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let s = &r.stats;
            let mut obj = serde_json::json!({
                "rank": i + 1,
                "symbol": s.symbol,
                "volume_24h": s.turnover_24h.to_string(),
                "open_interest": s.open_interest.to_string(),
                "last_price": s.last_price.to_string(),
                "price_change_pct": s.price_change_pct.to_string(),
            });
            for method in methods {
                obj[vol_json_key(*method)] = serde_json::json!(r.vol.value(*method));
            }
            obj
        })
        .collect();

    println!(
        "{}",
        serde_json::to_string_pretty(&values).unwrap_or_default()
    );
}

fn display_csv(ranked: &[RankedMarket], methods: &[VolatilityMethod]) -> Result<()> {
    let mut wtr = csv::Writer::from_writer(std::io::stdout());
    let mut header = vec![
        "rank".to_string(),
        "symbol".to_string(),
        "volume_24h".to_string(),
        "open_interest".to_string(),
        "last_price".to_string(),
        "price_change_pct".to_string(),
    ];
    header.extend(methods.iter().map(|m| vol_json_key(*m).to_string()));
    wtr.write_record(&header)?;

    for (i, r) in ranked.iter().enumerate() {
        let s = &r.stats;
        let mut row = vec![
            (i + 1).to_string(),
            s.symbol.clone(),
            s.turnover_24h.to_string(),
            s.open_interest.to_string(),
            s.last_price.to_string(),
            s.price_change_pct.to_string(),
        ];
        row.extend(
            methods
                .iter()
                .map(|m| r.vol.value(*m).map(|v| v.to_string()).unwrap_or_default()),
        );
        wtr.write_record(&row)?;
    }
    wtr.flush()?;
    Ok(())
}
