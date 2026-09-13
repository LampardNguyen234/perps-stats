use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use perps_core::{
    FundingRate, Kline, Market, MarketStats, OpenInterest, OrderSide, Orderbook, OrderbookLevel,
    Ticker, Trade,
};
use rust_decimal::Decimal;

use super::types::*;

// ---- symbol conversion ----

/// Convert a global symbol (e.g. `"BTC"`) to StandX format (e.g. `"BTC-USD"`).
/// Idempotent: `"BTC-USD"` -> `"BTC-USD"`.
pub fn standx_parse_symbol(symbol: &str) -> String {
    let upper = symbol.to_uppercase();
    let base = upper.strip_suffix("-USD").unwrap_or(&upper);
    let resolved = crate::symbol_aliases::resolve_alias("standx", base);
    format!("{}-USD", resolved)
}

/// Convert a StandX symbol (e.g. `"BTC-USD"`) back to global format (e.g. `"BTC"`).
pub fn standx_normalize_symbol(exchange_symbol: &str) -> String {
    let upper = exchange_symbol.to_uppercase();
    let base = upper.strip_suffix("-USD").unwrap_or(&upper);
    crate::symbol_aliases::unresolve_alias("standx", base).to_string()
}

// ---- helpers ----

fn parse_decimal(s: &str, field: &str) -> Result<Decimal> {
    Decimal::from_str_exact(s)
        .with_context(|| format!("Failed to parse {} as decimal: {:?}", field, s))
}

fn parse_rfc3339(s: &str, field: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .with_context(|| format!("Failed to parse {} as RFC3339 timestamp: {:?}", field, s))
}

/// StandX's kline resolution enum has no bucket for `30m`/`4h` (only 1/5/15/60/1D/1W/1M
/// plus sub-minute 1T/3S) — reject rather than silently approximate.
pub fn interval_to_resolution(interval: &str) -> Result<&'static str> {
    match interval {
        "1m" => Ok("1"),
        "5m" => Ok("5"),
        "15m" => Ok("15"),
        "1h" => Ok("60"),
        "1d" => Ok("1D"),
        "1w" => Ok("1W"),
        _ => anyhow::bail!("StandX does not support kline interval {interval}"),
    }
}

fn interval_duration(interval: &str) -> chrono::Duration {
    match interval {
        "1m" => chrono::Duration::minutes(1),
        "5m" => chrono::Duration::minutes(5),
        "15m" => chrono::Duration::minutes(15),
        "1h" => chrono::Duration::hours(1),
        "1d" => chrono::Duration::days(1),
        "1w" => chrono::Duration::weeks(1),
        _ => chrono::Duration::zero(),
    }
}

// ---- Market ----

/// `min_order_value` and `contract_size` have no direct StandX field.
/// `[ASSUMPTION]` `min_order_value = min_order_qty * last_price` (from `query_market_overview`);
/// `contract_size = 1` (no contract-multiplier field is documented or observed live).
pub fn symbol_info_to_market(
    info: &SymbolInfo,
    last_price: Decimal,
    symbol: String,
) -> Result<Market> {
    Ok(Market {
        symbol,
        contract: info.symbol.clone(),
        contract_size: Decimal::ONE,
        price_scale: info.price_tick_decimals,
        quantity_scale: info.qty_tick_decimals,
        min_order_qty: info.min_order_qty,
        max_order_qty: info.max_order_qty,
        min_order_value: info.min_order_qty * last_price,
        max_leverage: info.max_leverage,
    })
}

// ---- Ticker (query_symbol_market + query_depth_book) ----

/// Build a core `Ticker` from `query_symbol_market` (price/volume/OI/funding stats) and
/// `query_depth_book` (top-of-book quantity, which `query_symbol_market` doesn't provide).
pub fn symbol_market_depth_to_ticker(
    market: &SymbolMarket,
    depth: DepthBookResponse,
    symbol: String,
) -> Result<Ticker> {
    let best_bid_price = parse_decimal(&market.spread[0], "spread[0]")?;
    let best_ask_price = parse_decimal(&market.spread[1], "spread[1]")?;

    let orderbook = depth_book_to_orderbook_inner(depth, symbol.clone(), None)?;
    let best_bid_qty = orderbook.best_bid_qty().unwrap_or(Decimal::ZERO);
    let best_ask_qty = orderbook.best_ask_qty().unwrap_or(Decimal::ZERO);

    Ok(Ticker {
        symbol,
        last_price: market.last_price,
        mark_price: market.mark_price,
        index_price: market.index_price,
        best_bid_price,
        best_bid_qty,
        best_ask_price,
        best_ask_qty,
        volume_24h: market.volume_24h,
        turnover_24h: market.volume_quote_24h,
        open_interest: market.open_interest,
        open_interest_notional: market.open_interest_notional,
        price_change_24h: market.price_change,
        price_change_pct: market.price_change_pct / Decimal::from(100),
        high_price_24h: market.high_price_24h,
        low_price_24h: market.low_price_24h,
        timestamp: parse_rfc3339(&market.time, "time")?,
    })
}

/// Build a core `MarketStats` from `query_symbol_market` alone (no bid/ask qty needed).
pub fn symbol_market_to_market_stats(market: &SymbolMarket, symbol: String) -> Result<MarketStats> {
    Ok(MarketStats {
        symbol,
        volume_24h: market.volume_24h,
        turnover_24h: market.volume_quote_24h,
        open_interest: market.open_interest,
        funding_rate: market.funding_rate,
        last_price: market.last_price,
        mark_price: market.mark_price,
        index_price: market.index_price,
        price_change_24h: market.price_change,
        price_change_pct: market.price_change_pct / Decimal::from(100),
        high_price_24h: market.high_price_24h,
        low_price_24h: market.low_price_24h,
        timestamp: parse_rfc3339(&market.time, "time")?,
    })
}

// ---- Orderbook ----

/// StandX documents that level ordering is not guaranteed — always sort unconditionally.
pub fn depth_book_to_orderbook(
    depth: DepthBookResponse,
    symbol: String,
    max_depth: usize,
) -> Result<Orderbook> {
    depth_book_to_orderbook_inner(depth, symbol, Some(max_depth))
}

/// Same conversion, no truncation — used by the WS path, where `depth_book` frames arrive
/// as full snapshots with no caller-specified depth (unlike the REST `get_orderbook(depth)` call).
pub fn depth_book_full_to_orderbook(depth: DepthBookResponse, symbol: String) -> Result<Orderbook> {
    depth_book_to_orderbook_inner(depth, symbol, None)
}

fn depth_book_to_orderbook_inner(
    depth: DepthBookResponse,
    symbol: String,
    max_depth: Option<usize>,
) -> Result<Orderbook> {
    let mut bids = depth
        .bids
        .iter()
        .map(|[p, q]| {
            Ok(OrderbookLevel {
                price: parse_decimal(p, "bids.price")?,
                quantity: parse_decimal(q, "bids.qty")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut asks = depth
        .asks
        .iter()
        .map(|[p, q]| {
            Ok(OrderbookLevel {
                price: parse_decimal(p, "asks.price")?,
                quantity: parse_decimal(q, "asks.qty")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    bids.sort_by(|a, b| b.price.cmp(&a.price));
    asks.sort_by(|a, b| a.price.cmp(&b.price));

    if let Some(d) = max_depth {
        bids.truncate(d);
        asks.truncate(d);
    }

    Ok(Orderbook {
        symbol,
        bids,
        asks,
        timestamp: Utc::now(),
    })
}

// ---- FundingRate ----

/// Build the *current* `FundingRate` from `query_symbol_market` (already fetched for the
/// ticker in the common path) plus `query_symbol_info`'s `funding_rate_cap` for the
/// cap/floor bound. `funding_interval` is hardcoded to 1 hour — live-verified 2026-09-13
/// by observing `query_funding_rates` history entries land exactly on the hour
/// (`...T06:00:00Z`, `...T07:00:00Z`, ...) and `next_funding_time` always rounding to the
/// next top-of-hour.
pub fn symbol_market_info_to_funding_rate(
    market: &SymbolMarket,
    info: &SymbolInfo,
    symbol: String,
) -> Result<FundingRate> {
    let next_funding_time = parse_rfc3339(&market.next_funding_time, "next_funding_time")?;
    Ok(FundingRate {
        symbol,
        funding_rate: market.funding_rate,
        // [ASSUMPTION] no forward-looking predicted-rate field is documented or observed live.
        predicted_rate: market.funding_rate,
        funding_time: next_funding_time - chrono::Duration::hours(1),
        next_funding_time,
        funding_interval: 1,
        funding_rate_cap_floor: info.funding_rate_cap,
    })
}

/// Convert one `query_funding_rates` history entry. `funding_rate_cap_floor` is left at
/// zero here (the endpoint doesn't repeat the symbol-level cap per row); callers wanting
/// the cap should read it from `query_symbol_info` directly.
pub fn funding_rate_entry_to_funding_rate(
    entry: &FundingRateEntry,
    symbol: String,
) -> Result<FundingRate> {
    let funding_time = parse_rfc3339(&entry.time, "time")?;
    Ok(FundingRate {
        symbol,
        funding_rate: entry.funding_rate,
        predicted_rate: entry.funding_rate,
        funding_time,
        next_funding_time: funding_time + chrono::Duration::hours(1),
        funding_interval: 1,
        funding_rate_cap_floor: Decimal::ZERO,
    })
}

// ---- OpenInterest ----

pub fn symbol_market_to_open_interest(
    market: &SymbolMarket,
    symbol: String,
) -> Result<OpenInterest> {
    Ok(OpenInterest {
        symbol,
        open_interest: market.open_interest,
        open_value: market.open_interest_notional,
        timestamp: parse_rfc3339(&market.time, "time")?,
    })
}

// ---- Trade ----

/// StandX's REST trade entries have no `id` field — `[ASSUMPTION]` synthesize one from
/// symbol + nanosecond timestamp (no-panic fallback, matches the pattern used elsewhere
/// in this codebase for exchanges without a trade id).
pub fn recent_trade_to_trade(t: &RecentTrade, symbol: String) -> Result<Trade> {
    let timestamp = parse_rfc3339(&t.time, "time")?;
    let side = if t.is_buyer_taker {
        OrderSide::Buy
    } else {
        OrderSide::Sell
    };
    Ok(Trade {
        id: format!(
            "{}-{}",
            symbol,
            timestamp.timestamp_nanos_opt().unwrap_or(0)
        ),
        symbol,
        price: t.price,
        quantity: t.qty,
        side,
        timestamp,
    })
}

// ---- Kline ----

/// `turnover = v * c` (derived — no notional array in the response, documented approximation).
/// `close_time` is derived from `open_time + interval` since the endpoint doesn't provide it.
pub fn kline_history_to_klines(
    resp: KlineHistoryResponse,
    symbol: String,
    interval: String,
) -> Result<Vec<Kline>> {
    if resp.s != "ok" {
        anyhow::bail!("StandX kline/history returned status {:?}", resp.s);
    }
    let duration = interval_duration(&interval);
    resp.t
        .iter()
        .enumerate()
        .map(|(i, &t)| {
            let open_time = DateTime::<Utc>::from_timestamp(t, 0)
                .ok_or_else(|| anyhow!("invalid kline open_time seconds: {t}"))?;
            let open = Decimal::from_f64_retain_checked(resp.o[i], "open")?;
            let high = Decimal::from_f64_retain_checked(resp.h[i], "high")?;
            let low = Decimal::from_f64_retain_checked(resp.l[i], "low")?;
            let close = Decimal::from_f64_retain_checked(resp.c[i], "close")?;
            let volume = Decimal::from_f64_retain_checked(resp.v[i], "volume")?;
            Ok(Kline {
                symbol: symbol.clone(),
                interval: interval.clone(),
                open_time,
                close_time: open_time + duration,
                open,
                high,
                low,
                close,
                volume,
                turnover: volume * close,
            })
        })
        .collect()
}

/// `kline/history`'s OHLCV arrays are native JSON floats (not strings, unlike almost every
/// other StandX numeric field) — `Decimal::from_f64_retain` with documented precision loss,
/// per the project's sanctioned exception for this one endpoint.
trait FromF64RetainChecked {
    fn from_f64_retain_checked(v: f64, field: &str) -> Result<Decimal>;
}
impl FromF64RetainChecked for Decimal {
    fn from_f64_retain_checked(v: f64, field: &str) -> Result<Decimal> {
        Decimal::from_f64_retain(v)
            .ok_or_else(|| anyhow!("cannot represent {field}={v} as Decimal"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_symbol_idempotent() {
        assert_eq!(standx_parse_symbol("BTC"), "BTC-USD");
        assert_eq!(standx_parse_symbol("btc"), "BTC-USD");
        assert_eq!(standx_parse_symbol("BTC-USD"), "BTC-USD");
        assert_eq!(standx_parse_symbol("XAU"), "XAU-USD");
    }

    #[test]
    fn test_normalize_symbol() {
        assert_eq!(standx_normalize_symbol("BTC-USD"), "BTC");
        assert_eq!(standx_normalize_symbol("btc-usd"), "BTC");
    }

    #[test]
    fn test_interval_whitelist_rejects_unsupported() {
        assert!(interval_to_resolution("30m").is_err());
        assert!(interval_to_resolution("4h").is_err());
        assert_eq!(interval_to_resolution("1h").unwrap(), "60");
        assert_eq!(interval_to_resolution("1d").unwrap(), "1D");
    }

    #[test]
    fn test_orderbook_sorts_out_of_order_input() {
        // Deliberately out-of-order (StandX explicitly documents unordered levels).
        let depth = DepthBookResponse {
            bids: vec![
                ["100".to_string(), "1".to_string()],
                ["105".to_string(), "1".to_string()],
                ["102".to_string(), "1".to_string()],
            ],
            asks: vec![
                ["110".to_string(), "1".to_string()],
                ["106".to_string(), "1".to_string()],
                ["108".to_string(), "1".to_string()],
            ],
        };
        let ob = depth_book_to_orderbook(depth, "BTC".to_string(), 10).unwrap();
        assert_eq!(
            ob.bids.iter().map(|l| l.price).collect::<Vec<_>>(),
            vec![Decimal::from(105), Decimal::from(102), Decimal::from(100)]
        );
        assert_eq!(
            ob.asks.iter().map(|l| l.price).collect::<Vec<_>>(),
            vec![Decimal::from(106), Decimal::from(108), Decimal::from(110)]
        );
        // No crossed book.
        assert!(ob.best_bid().unwrap() < ob.best_ask().unwrap());
    }

    #[test]
    fn test_price_change_pct_is_percentage_converted_to_ratio() {
        let market = SymbolMarket {
            symbol: "BTC-USD".to_string(),
            last_price: Decimal::from_str_exact("77259.99").unwrap(),
            mark_price: Decimal::from_str_exact("77259.99").unwrap(),
            index_price: Decimal::from_str_exact("77300.19").unwrap(),
            spread: ["77248.58".to_string(), "77259.99".to_string()],
            high_price_24h: Decimal::from_str_exact("77476.98").unwrap(),
            low_price_24h: Decimal::from_str_exact("77018.45").unwrap(),
            volume_24h: Decimal::from_str_exact("3581.03").unwrap(),
            volume_quote_24h: Decimal::from_str_exact("276606682.7").unwrap(),
            open_interest: Decimal::from_str_exact("383.994").unwrap(),
            open_interest_notional: Decimal::from_str_exact("29667372.6").unwrap(),
            funding_rate: Decimal::from_str_exact("0.00000933").unwrap(),
            next_funding_time: "2026-09-13T06:00:00Z".to_string(),
            price_change: Decimal::from_str_exact("47.95").unwrap(),
            price_change_pct: Decimal::from_str_exact("0.06210171367057734").unwrap(),
            time: "2026-09-13T05:38:33.271523Z".to_string(),
        };
        let stats = symbol_market_to_market_stats(&market, "BTC".to_string()).unwrap();
        // Ratio must be small (< 1%), not the raw percentage value.
        assert!(stats.price_change_pct.abs() < Decimal::from_str_exact("0.01").unwrap());
    }

    #[test]
    fn test_volume_vs_turnover_not_swapped() {
        let market = SymbolMarket {
            symbol: "BTC-USD".to_string(),
            last_price: Decimal::from(77000),
            mark_price: Decimal::from(77000),
            index_price: Decimal::from(77000),
            spread: ["76990".to_string(), "77010".to_string()],
            high_price_24h: Decimal::from(78000),
            low_price_24h: Decimal::from(76000),
            volume_24h: Decimal::from_str_exact("3581.03").unwrap(),
            volume_quote_24h: Decimal::from_str_exact("276606682.7").unwrap(),
            open_interest: Decimal::from_str_exact("383.994").unwrap(),
            open_interest_notional: Decimal::from_str_exact("29667372.6").unwrap(),
            funding_rate: Decimal::ZERO,
            next_funding_time: "2026-09-13T06:00:00Z".to_string(),
            price_change: Decimal::ZERO,
            price_change_pct: Decimal::ZERO,
            time: "2026-09-13T05:38:33Z".to_string(),
        };
        let stats = symbol_market_to_market_stats(&market, "BTC".to_string()).unwrap();
        assert!(stats.volume_24h < Decimal::from(10000)); // base qty, small
        assert!(stats.turnover_24h > Decimal::from(1_000_000)); // USD notional, large
    }

    #[test]
    fn test_kline_turnover_derivation() {
        let resp = KlineHistoryResponse {
            s: "ok".to_string(),
            t: vec![1754897028],
            o: vec![121896.02],
            h: vec![121897.95],
            l: vec![121895.92],
            c: vec![121897.95],
            v: vec![10.0],
        };
        let klines = kline_history_to_klines(resp, "BTC".to_string(), "1m".to_string()).unwrap();
        assert_eq!(klines.len(), 1);
        assert_eq!(klines[0].turnover, klines[0].volume * klines[0].close);
    }
}
