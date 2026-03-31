use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use perps_core::{
    FundingRate, Kline, Market, MarketStats, MultiResolutionOrderbook, OpenInterest, Ticker,
};
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;

use super::types::*;

// ── helpers ───────────────────────────────────────────────────────────────────

fn f64_to_decimal_or_zero(opt: Option<f64>, field: &str) -> Decimal {
    opt.and_then(|v| {
        Decimal::from_f64(v).or_else(|| {
            tracing::warn!("QFEX: failed to convert f64 {} for field {}", v, field);
            None
        })
    })
    .unwrap_or(Decimal::ZERO)
}

fn str_to_decimal_or_zero(opt: Option<&str>, field: &str) -> Decimal {
    opt.and_then(|s| {
        s.parse::<f64>()
            .ok()
            .and_then(Decimal::from_f64)
            .or_else(|| Decimal::from_str_exact(s).ok())
    })
    .unwrap_or_else(|| {
        if opt.is_some() {
            tracing::warn!(
                "QFEX: failed to parse decimal for field {}: {:?}",
                field,
                opt
            );
        }
        Decimal::ZERO
    })
}

fn parse_iso8601(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

// ── RefDataItem → Market ──────────────────────────────────────────────────────

/// Convert a QFEX `RefDataItem` to a core `Market`.
///
/// `symbol` in `Market` is the global form (e.g. `"NVDA"`), stripped of `-USD` suffix.
pub fn refdata_item_to_market(item: &RefDataItem) -> Result<Market> {
    let global_symbol = item
        .base_asset
        .as_deref()
        .unwrap_or_else(|| item.symbol.trim_end_matches("-USD"))
        .to_string();

    let tick_size = str_to_decimal_or_zero(item.tick_size.as_deref(), "tick_size");
    let lot_size = str_to_decimal_or_zero(item.lot_size.as_deref(), "lot_size");
    let max_leverage = item
        .default_max_leverage
        .map(Decimal::from)
        .unwrap_or_else(|| Decimal::from(10));

    let _ = tick_size; // stored for future use (price_scale derivation)

    let min_order_qty = if lot_size > Decimal::ZERO {
        lot_size
    } else {
        Decimal::ONE
    };

    Ok(Market {
        symbol: global_symbol,
        contract: item.symbol.clone(),
        contract_size: Decimal::ONE,
        price_scale: 2,
        quantity_scale: 4,
        min_order_qty,
        max_order_qty: Decimal::from(1_000_000),
        min_order_value: Decimal::ZERO,
        max_leverage,
    })
}

// ── SymbolMetrics → Ticker ────────────────────────────────────────────────────

/// Build a core `Ticker` from a `SymbolMetrics` entry.
///
/// Field semantics:
/// - `mark_price_change_24h_pct` is a **percentage** (0.3563 = +0.3563%) — divide by 100.
/// - `volume_24h_usd_notional` is USD turnover → base volume = turnover / mark_price.
/// - `open_interest` is in **contracts** → notional = oi × mark_price.
/// - `funding_rate_bps` is a decimal ratio — use directly.
pub fn metrics_to_ticker(m: &SymbolMetrics, ob: &MultiResolutionOrderbook) -> Result<Ticker> {
    let global_symbol = m.symbol.trim_end_matches("-USD").to_string();

    let mark_price = f64_to_decimal_or_zero(m.current_mark_price, "current_mark_price");
    let turnover_24h = f64_to_decimal_or_zero(m.volume_24h_usd_notional, "volume_24h_usd_notional");

    let volume_24h = if mark_price > Decimal::ZERO {
        (turnover_24h / mark_price).round_dp(6)
    } else {
        Decimal::ZERO
    };

    // pct field is a percentage (0.3563 means +0.3563%) → divide by 100
    let price_change_pct = m
        .mark_price_change_24h_pct
        .and_then(Decimal::from_f64)
        .unwrap_or(Decimal::ZERO)
        / Decimal::from(100);

    let open_interest = f64_to_decimal_or_zero(m.open_interest, "open_interest");
    let open_interest_notional = if open_interest > Decimal::ZERO && mark_price > Decimal::ZERO {
        (open_interest * mark_price).round_dp(2)
    } else {
        Decimal::ZERO
    };

    let (best_bid_price, best_bid_qty) = ob.orderbooks[0]
        .bids
        .first()
        .and_then(|l| Option::from((l.price, l.quantity)))
        .unwrap_or((Decimal::ZERO, Decimal::ZERO));

    let (best_ask_price, best_ask_qty) = ob.orderbooks[0]
        .asks
        .first()
        .and_then(|l| Option::from((l.price, l.quantity)))
        .unwrap_or((Decimal::ZERO, Decimal::ZERO));

    Ok(Ticker {
        symbol: global_symbol,
        last_price: mark_price,
        mark_price,
        index_price: mark_price,
        best_bid_price,
        best_bid_qty,
        best_ask_price,
        best_ask_qty,
        volume_24h,
        turnover_24h,
        open_interest,
        open_interest_notional,
        price_change_24h: mark_price * price_change_pct,
        price_change_pct,
        high_price_24h: Decimal::ZERO,
        low_price_24h: Decimal::ZERO,
        timestamp: Utc::now(),
    })
}

// ── SymbolMetrics + RefDataItem → MarketStats ─────────────────────────────────

/// Build a core `MarketStats` from `SymbolMetrics` and optionally `RefDataItem`.
pub fn metrics_to_market_stats(
    m: &SymbolMetrics,
    _refdata: Option<&RefDataItem>,
) -> Result<MarketStats> {
    let global_symbol = m.symbol.trim_end_matches("-USD").to_string();

    let mark_price = f64_to_decimal_or_zero(m.current_mark_price, "current_mark_price");
    let turnover_24h = f64_to_decimal_or_zero(m.volume_24h_usd_notional, "volume_24h_usd_notional");

    let volume_24h = if mark_price > Decimal::ZERO {
        (turnover_24h / mark_price).round_dp(6)
    } else {
        Decimal::ZERO
    };

    let price_change_pct = m
        .mark_price_change_24h_pct
        .and_then(Decimal::from_f64)
        .unwrap_or(Decimal::ZERO)
        / Decimal::from(100);

    let open_interest = f64_to_decimal_or_zero(m.open_interest, "open_interest");

    // funding_rate_bps is a decimal ratio — use directly
    let funding_rate = f64_to_decimal_or_zero(m.funding_rate_bps, "funding_rate_bps");

    Ok(MarketStats {
        symbol: global_symbol,
        last_price: mark_price,
        mark_price,
        index_price: Decimal::ZERO,
        volume_24h,
        turnover_24h,
        open_interest,
        funding_rate,
        price_change_24h: Decimal::ZERO,
        price_change_pct,
        high_price_24h: Decimal::ZERO,
        low_price_24h: Decimal::ZERO,
        timestamp: Utc::now(),
    })
}

// ── SymbolMetrics → OpenInterest ──────────────────────────────────────────────

/// Build a core `OpenInterest` from a `SymbolMetrics` entry.
pub fn metrics_to_open_interest(m: &SymbolMetrics) -> Result<OpenInterest> {
    let global_symbol = m.symbol.trim_end_matches("-USD").to_string();

    let mark_price = f64_to_decimal_or_zero(m.current_mark_price, "current_mark_price");
    let open_interest = f64_to_decimal_or_zero(m.open_interest, "open_interest");
    let open_value = if open_interest > Decimal::ZERO && mark_price > Decimal::ZERO {
        open_interest * mark_price
    } else {
        Decimal::ZERO
    };

    Ok(OpenInterest {
        symbol: global_symbol,
        open_interest,
        open_value,
        timestamp: Utc::now(),
    })
}

// ── SymbolMetrics → FundingRate ───────────────────────────────────────────────

/// Build a core `FundingRate` from a `SymbolMetrics` entry.
///
/// Note: `funding_rate_bps` is a decimal ratio despite its name.
pub fn metrics_to_funding_rate(m: &SymbolMetrics) -> Result<FundingRate> {
    let global_symbol = m.symbol.trim_end_matches("-USD").to_string();

    // funding_rate_bps is a decimal ratio — use directly
    let funding_rate = f64_to_decimal_or_zero(m.funding_rate_bps, "funding_rate_bps");

    Ok(FundingRate {
        symbol: global_symbol,
        funding_rate,
        predicted_rate: funding_rate,
        funding_time: Utc::now(),
        next_funding_time: Utc::now() + chrono::Duration::hours(8),
        funding_interval: 8,
        funding_rate_cap_floor: Decimal::ZERO,
    })
}

// ── FundingPoint → FundingRate ────────────────────────────────────────────────

/// Convert a historical funding point to a core `FundingRate`.
pub fn funding_point_to_funding_rate(
    p: &FundingPoint,
    default_symbol: &str,
) -> Result<FundingRate> {
    let symbol = p
        .symbol
        .as_deref()
        .unwrap_or(default_symbol)
        .trim_end_matches("-USD")
        .to_string();

    let funding_rate = f64_to_decimal_or_zero(p.rate, "rate");

    let funding_time = p
        .window_start
        .as_deref()
        .map(parse_iso8601)
        .unwrap_or_else(Utc::now);

    let interval_hours = p
        .interval_minutes
        .map(|m| (m / 60).max(1) as i32)
        .unwrap_or(8);

    Ok(FundingRate {
        symbol,
        funding_rate,
        predicted_rate: funding_rate,
        funding_time,
        next_funding_time: funding_time + chrono::Duration::hours(interval_hours as i64),
        funding_interval: interval_hours,
        funding_rate_cap_floor: Decimal::ZERO,
    })
}

// ── CandleEntry → Kline ───────────────────────────────────────────────────────

/// Convert a QFEX candle entry to a core `Kline`.
///
/// `usd_volume` is USD turnover → `turnover` in core.
/// Base volume (`volume`) = turnover / close price.
pub fn candle_to_kline(c: &CandleEntry, symbol: &str, interval: &str) -> Result<Kline> {
    let global_symbol = symbol.trim_end_matches("-USD").to_string();

    let open = str_to_decimal_or_zero(c.open.as_deref(), "open");
    let high = str_to_decimal_or_zero(c.high.as_deref(), "high");
    let low = str_to_decimal_or_zero(c.low.as_deref(), "low");
    let close = str_to_decimal_or_zero(c.close.as_deref(), "close");

    let turnover = f64_to_decimal_or_zero(c.usd_volume, "usd_volume");

    // Try base_token_volume first; fall back to turnover / close
    let volume = if let Some(v) = c.base_token_volume.as_deref() {
        str_to_decimal_or_zero(Some(v), "base_token_volume")
    } else if close > Decimal::ZERO {
        turnover / close
    } else {
        Decimal::ZERO
    };

    let open_time = c
        .started_at
        .as_deref()
        .map(parse_iso8601)
        .unwrap_or_else(Utc::now);

    Ok(Kline {
        symbol: global_symbol,
        interval: interval.to_string(),
        open_time,
        close_time: open_time,
        open,
        high,
        low,
        close,
        volume,
        turnover,
    })
}

// ── symbol helpers ────────────────────────────────────────────────────────────

/// Convert global symbol `"NVDA"` → QFEX symbol `"NVDA-USD"`.
/// Idempotent: already-suffixed symbols pass through unchanged.
pub fn to_qfex_symbol(symbol: &str) -> String {
    let upper = symbol.to_uppercase();
    if upper.ends_with("-USD") {
        upper
    } else {
        format!("{}-USD", upper)
    }
}

/// Convert QFEX symbol `"NVDA-USD"` → global symbol `"NVDA"`.
pub fn to_global_symbol(qfex_symbol: &str) -> String {
    qfex_symbol.trim_end_matches("-USD").to_string()
}

/// Map standard kline interval to QFEX resolution string.
///
/// Supported: `1m`, `5m`, `15m`, `1h`, `4h`, `1d`.
/// Returns `Err` for unsupported intervals (e.g. `30m`).
pub fn map_kline_interval(interval: &str) -> Result<&'static str> {
    match interval {
        "1m" => Ok("1MIN"),
        "5m" => Ok("5MINS"),
        "15m" => Ok("15MINS"),
        "1h" => Ok("1HOUR"),
        "4h" => Ok("4HOURS"),
        "1d" => Ok("1DAY"),
        other => Err(anyhow!(
            "QFEX does not support kline interval '{}'. Supported: 1m, 5m, 15m, 1h, 4h, 1d",
            other
        )),
    }
}

// ── unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_metrics() -> SymbolMetrics {
        SymbolMetrics {
            symbol: "AAPL-USD".to_string(),
            current_mark_price: Some(247.86),
            volume_24h_usd_notional: Some(568.19),
            mark_price_change_24h_pct: Some(0.3563),
            open_interest: Some(1936.145),
            funding_rate_bps: Some(-0.0005),
        }
    }

    #[test]
    fn test_parse_symbol_idempotent() {
        assert_eq!(to_qfex_symbol("NVDA"), "NVDA-USD");
        assert_eq!(to_qfex_symbol("NVDA-USD"), "NVDA-USD");
        assert_eq!(to_qfex_symbol("nvda"), "NVDA-USD");
        assert_eq!(to_qfex_symbol("GOOGL"), "GOOGL-USD");
    }

    #[test]
    fn test_to_global_symbol() {
        assert_eq!(to_global_symbol("NVDA-USD"), "NVDA");
        assert_eq!(to_global_symbol("AAPL-USD"), "AAPL");
    }

    #[test]
    fn test_metrics_to_ticker_volume_turnover() {
        let m = make_metrics();
        let ticker = metrics_to_ticker(&m).unwrap();

        // Turnover is direct from usd_notional
        assert_eq!(ticker.turnover_24h, Decimal::from_f64(568.19).unwrap());
        // volume = turnover / mark_price
        let expected_volume =
            Decimal::from_f64(568.19).unwrap() / Decimal::from_f64(247.86).unwrap();
        // Allow small rounding difference
        let diff = (ticker.volume_24h - expected_volume).abs();
        assert!(diff < Decimal::from_str_exact("0.0001").unwrap());
    }

    #[test]
    fn test_price_change_pct_is_ratio() {
        let m = make_metrics();
        let ticker = metrics_to_ticker(&m).unwrap();

        // 0.3563 pct → 0.003563 ratio
        let expected = Decimal::from_f64(0.3563 / 100.0).unwrap();
        let diff = (ticker.price_change_pct - expected).abs();
        assert!(diff < Decimal::from_str_exact("0.0000001").unwrap());
    }

    #[test]
    fn test_funding_rate_direct() {
        let m = make_metrics();
        let _ticker = metrics_to_ticker(&m).unwrap();
        let fr = metrics_to_funding_rate(&m).unwrap();

        // funding_rate_bps = -0.0005 → use directly
        let expected = Decimal::from_f64(-0.0005).unwrap();
        assert_eq!(fr.funding_rate, expected);
    }

    #[test]
    fn test_open_interest_notional() {
        let m = make_metrics();
        let oi = metrics_to_open_interest(&m).unwrap();

        // open_interest (contracts) × mark_price = notional
        let expected_notional = Decimal::from_f64(1936.145 * 247.86).unwrap();
        let diff = (oi.open_value - expected_notional).abs();
        // Allow up to 0.01 USD difference from float rounding
        assert!(diff < Decimal::from_str_exact("0.01").unwrap());
    }

    #[test]
    fn test_map_kline_interval_valid() {
        assert_eq!(map_kline_interval("1m").unwrap(), "1MIN");
        assert_eq!(map_kline_interval("5m").unwrap(), "5MINS");
        assert_eq!(map_kline_interval("15m").unwrap(), "15MINS");
        assert_eq!(map_kline_interval("1h").unwrap(), "1HOUR");
        assert_eq!(map_kline_interval("4h").unwrap(), "4HOURS");
        assert_eq!(map_kline_interval("1d").unwrap(), "1DAY");
    }

    #[test]
    fn test_map_kline_interval_unsupported() {
        assert!(map_kline_interval("30m").is_err());
        assert!(map_kline_interval("1w").is_err());
        assert!(map_kline_interval("2h").is_err());
    }

    #[test]
    fn test_candle_to_kline_usd_volume_to_turnover() {
        let candle = CandleEntry {
            started_at: Some("2024-01-01T00:00:00Z".to_string()),
            open: Some("100.0".to_string()),
            high: Some("105.0".to_string()),
            low: Some("99.0".to_string()),
            close: Some("103.0".to_string()),
            usd_volume: Some(10300.0), // 100 shares × $103
            base_token_volume: None,
            trades: Some(50),
        };
        let kline = candle_to_kline(&candle, "NVDA-USD", "1h").unwrap();

        assert_eq!(kline.symbol, "NVDA");
        assert_eq!(kline.interval, "1h");
        assert_eq!(kline.turnover, Decimal::from_f64(10300.0).unwrap());
        // volume = 10300 / 103 = 100
        let diff = (kline.volume - Decimal::from(100)).abs();
        assert!(diff < Decimal::from_str_exact("0.001").unwrap());
    }

    #[test]
    fn test_refdata_item_to_market() {
        let item = RefDataItem {
            symbol: "NVDA-USD".to_string(),
            base_asset: Some("NVDA".to_string()),
            underlier_price: Some(900.0),
            price_change_24h: None,
            tick_size: Some("0.01".to_string()),
            lot_size: Some("1".to_string()),
            default_max_leverage: Some(10),
            status: Some("ACTIVE".to_string()),
            product_category: Some("EQUITY".to_string()),
        };
        let market = refdata_item_to_market(&item).unwrap();

        assert_eq!(market.symbol, "NVDA");
        assert_eq!(market.contract, "NVDA-USD");
        assert_eq!(market.max_leverage, Decimal::from(10));
        assert_eq!(market.min_order_qty, Decimal::ONE);
    }

    #[test]
    fn test_funding_point_to_funding_rate() {
        let p = FundingPoint {
            symbol: Some("NVDA-USD".to_string()),
            interval_minutes: Some(480), // 8 hours
            window_start: Some("2024-01-01T00:00:00Z".to_string()),
            rate: Some(-0.0005),
        };
        let fr = funding_point_to_funding_rate(&p, "NVDA-USD").unwrap();

        assert_eq!(fr.symbol, "NVDA");
        assert_eq!(fr.funding_rate, Decimal::from_f64(-0.0005).unwrap());
        assert_eq!(fr.funding_interval, 8);
    }
}
