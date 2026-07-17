use anyhow::{Context, Result};
use chrono::{DateTime, TimeZone, Utc};
use perps_core::{
    FundingRate, Kline, Market, MarketStats, OpenInterest, OrderSide, Orderbook, OrderbookLevel,
    Ticker, Trade,
};
use rust_decimal::Decimal;
use std::str::FromStr;

use super::types::*;

// ── helpers ───────────────────────────────────────────────────────────────────

pub fn parse_decimal(s: &Option<String>, _field: &str) -> Decimal {
    s.as_deref()
        .filter(|v| !v.is_empty())
        .and_then(|v| Decimal::from_str(v).ok())
        .unwrap_or(Decimal::ZERO)
}

pub fn parse_decimal_required(s: &str, field: &str) -> Result<Decimal> {
    Decimal::from_str(s)
        .with_context(|| format!("failed to parse decimal field '{}': {:?}", field, s))
}

/// Unix nanoseconds string → DateTime<Utc>. Falls back to now() on parse failure.
pub fn nanos_str_to_datetime(s: &str) -> DateTime<Utc> {
    s.trim()
        .parse::<u64>()
        .ok()
        .and_then(|nanos| {
            let secs = (nanos / 1_000_000_000) as i64;
            let sub = (nanos % 1_000_000_000) as u32;
            Utc.timestamp_opt(secs, sub).single()
        })
        .unwrap_or_else(Utc::now)
}

/// Standard interval string ("1m", "1h", etc.) → nanoseconds for the RISEx API.
pub fn interval_to_nanos(interval: &str) -> Result<i64> {
    match interval {
        "1m" => Ok(60_000_000_000),
        "5m" => Ok(300_000_000_000),
        "15m" => Ok(900_000_000_000),
        "30m" => Ok(1_800_000_000_000),
        "1h" => Ok(3_600_000_000_000),
        "4h" => Ok(14_400_000_000_000),
        "1d" => Ok(86_400_000_000_000),
        "1w" => Ok(604_800_000_000_000),
        other => anyhow::bail!("unsupported kline interval: {}", other),
    }
}

/// RISEx API symbol → global symbol. "BTC/USDC" → "BTC"; "BTC" → "BTC".
pub fn global_symbol_from_risex_symbol(symbol: &str) -> String {
    symbol.split('/').next().unwrap_or(symbol).to_uppercase()
}

/// User/global symbol → RISEx symbol. "BTC" → "BTC/USDC"; "BTC/USDC" → "BTC/USDC".
pub fn risex_symbol_from_input(symbol: &str) -> String {
    let upper = symbol.to_uppercase();
    if upper.contains('/') {
        upper
    } else {
        format!("{}/USDC", upper)
    }
}

// ── converters ────────────────────────────────────────────────────────────────

pub fn to_market(m: &ApiMarketInfo) -> Market {
    Market {
        symbol: global_symbol_from_risex_symbol(&m.base_asset_symbol),
        contract: m.base_asset_symbol.to_uppercase(),
        contract_size: Decimal::ONE,
        price_scale: 8,
        quantity_scale: 6,
        min_order_qty: parse_decimal(&m.config.min_order_size, "min_order_size"),
        max_order_qty: Decimal::MAX,
        min_order_value: Decimal::ZERO,
        max_leverage: parse_decimal(&m.config.max_leverage, "max_leverage"),
    }
}

pub fn to_ticker(m: &ApiMarketInfo, ob: &ApiGetOrderbookResponse) -> Ticker {
    let last_price = parse_decimal(&m.last_price, "last_price");
    let mark_price = parse_decimal(&m.mark_price, "mark_price");
    let index_price = parse_decimal(&m.index_price, "index_price");

    // quote_volume_24h is USD turnover
    let turnover_24h = parse_decimal(&m.quote_volume_24h, "quote_volume_24h").round_dp(2);
    let volume_24h = if last_price > Decimal::ZERO {
        (turnover_24h / last_price).round_dp(6)
    } else {
        Decimal::ZERO
    };

    let open_interest = parse_decimal(&m.open_interest, "open_interest").round_dp(6);
    let open_interest_notional = (open_interest * mark_price).round_dp(2);

    let price_change_24h = parse_decimal(&m.change_24h, "change_24h");
    let prev_price = last_price - price_change_24h;
    let price_change_pct = if prev_price > Decimal::ZERO {
        (price_change_24h / prev_price).round_dp(6)
    } else {
        Decimal::ZERO
    };

    let best_bid = ob.bids.first();
    let best_ask = ob.asks.first();

    Ticker {
        symbol: global_symbol_from_risex_symbol(&m.base_asset_symbol),
        last_price,
        mark_price,
        index_price,
        best_bid_price: best_bid
            .map(|l| parse_decimal_required(&l.price, "bid.price").unwrap_or(Decimal::ZERO))
            .unwrap_or(Decimal::ZERO),
        best_bid_qty: best_bid
            .map(|l| parse_decimal_required(&l.quantity, "bid.qty").unwrap_or(Decimal::ZERO))
            .unwrap_or(Decimal::ZERO),
        best_ask_price: best_ask
            .map(|l| parse_decimal_required(&l.price, "ask.price").unwrap_or(Decimal::ZERO))
            .unwrap_or(Decimal::ZERO),
        best_ask_qty: best_ask
            .map(|l| parse_decimal_required(&l.quantity, "ask.qty").unwrap_or(Decimal::ZERO))
            .unwrap_or(Decimal::ZERO),
        volume_24h,
        turnover_24h,
        open_interest,
        open_interest_notional,
        price_change_24h,
        price_change_pct,
        high_price_24h: parse_decimal(&m.high_24h, "high_24h"),
        low_price_24h: parse_decimal(&m.low_24h, "low_24h"),
        timestamp: Utc::now(),
    }
}

pub fn to_orderbook(resp: &ApiGetOrderbookResponse, symbol: &str, depth: usize) -> Orderbook {
    let mut bids: Vec<OrderbookLevel> = resp
        .bids
        .iter()
        .filter_map(|l| {
            let price = Decimal::from_str(&l.price).ok()?;
            let qty = Decimal::from_str(&l.quantity).ok()?;
            Some(OrderbookLevel {
                price,
                quantity: qty,
            })
        })
        .collect();
    let mut asks: Vec<OrderbookLevel> = resp
        .asks
        .iter()
        .filter_map(|l| {
            let price = Decimal::from_str(&l.price).ok()?;
            let qty = Decimal::from_str(&l.quantity).ok()?;
            Some(OrderbookLevel {
                price,
                quantity: qty,
            })
        })
        .collect();

    bids.sort_by(|a, b| b.price.cmp(&a.price)); // descending
    asks.sort_by(|a, b| a.price.cmp(&b.price)); // ascending

    if let (Some(bid), Some(ask)) = (bids.first(), asks.first()) {
        if bid.price >= ask.price {
            tracing::warn!(
                "RISEx crossed orderbook for {}: bid {} >= ask {}",
                symbol,
                bid.price,
                ask.price
            );
        }
    }

    bids.truncate(depth);
    asks.truncate(depth);

    Orderbook {
        symbol: symbol.to_string(),
        bids,
        asks,
        timestamp: Utc::now(),
    }
}

pub fn to_funding_rate(m: &ApiMarketInfo) -> FundingRate {
    let funding_interval_hours = m
        .funding_interval
        .as_deref()
        .and_then(|s| s.parse::<u64>().ok())
        .map(|ns| (ns / 3_600_000_000_000) as i32)
        .unwrap_or(1);
    let next_funding_time = m
        .next_funding_time
        .as_deref()
        .map(nanos_str_to_datetime)
        .unwrap_or_else(|| Utc::now() + chrono::Duration::hours(funding_interval_hours as i64));

    FundingRate {
        symbol: global_symbol_from_risex_symbol(&m.base_asset_symbol),
        funding_rate: parse_decimal(&m.current_funding_rate, "current_funding_rate"),
        predicted_rate: parse_decimal(&m.predicted_funding_rate, "predicted_funding_rate"),
        funding_time: Utc::now(),
        next_funding_time,
        funding_interval: funding_interval_hours,
        funding_rate_cap_floor: Decimal::ZERO,
    }
}

pub fn to_funding_rate_from_record(symbol: &str, r: &ApiFundingRecord) -> FundingRate {
    let funding_rate =
        parse_decimal_required(&r.funding_rate, "funding_rate").unwrap_or(Decimal::ZERO);
    let funding_time = nanos_str_to_datetime(&r.end_time);
    FundingRate {
        symbol: symbol.to_string(),
        funding_rate,
        predicted_rate: funding_rate,
        funding_time,
        next_funding_time: funding_time + chrono::Duration::hours(1),
        funding_interval: 1,
        funding_rate_cap_floor: Decimal::ZERO,
    }
}

pub fn to_open_interest(m: &ApiMarketInfo) -> OpenInterest {
    let mark_price = parse_decimal(&m.mark_price, "mark_price");
    let qty = parse_decimal(&m.open_interest, "open_interest").round_dp(6);
    OpenInterest {
        symbol: global_symbol_from_risex_symbol(&m.base_asset_symbol),
        open_interest: qty,
        open_value: (qty * mark_price).round_dp(2),
        timestamp: Utc::now(),
    }
}

pub fn to_kline(symbol: &str, bar: &ApiKlineBar, interval: &str) -> Result<Kline> {
    let open_time = nanos_str_to_datetime(&bar.time);
    let interval_dur = chrono::Duration::nanoseconds(interval_to_nanos(interval)?);
    let close_time = open_time + interval_dur - chrono::Duration::milliseconds(1);

    let close = parse_decimal_required(&bar.close, "close")?;
    let volume = parse_decimal_required(&bar.volume, "volume")
        .unwrap_or(Decimal::ZERO)
        .round_dp(6);
    // RISEx kline volume field is base quantity; turnover derived as volume × close price.
    let turnover = (volume * close).round_dp(2);

    Ok(Kline {
        symbol: symbol.to_string(),
        interval: interval.to_string(),
        open_time,
        close_time,
        open: parse_decimal_required(&bar.open, "open")?,
        high: parse_decimal_required(&bar.high, "high")?,
        low: parse_decimal_required(&bar.low, "low")?,
        close,
        volume,
        turnover,
    })
}

pub fn to_trade(symbol: &str, t: &ApiTrade) -> Result<Trade> {
    // maker_side is the passive side; taker (aggressor) is the opposite.
    // Conventionally Trade.side = aggressor side, so we invert maker_side.
    let side = match t.maker_side.as_str() {
        "BUY" => OrderSide::Sell, // maker bought → taker sold
        "SELL" => OrderSide::Buy, // maker sold → taker bought
        other => {
            tracing::warn!(
                "RISEx unknown trade maker_side {:?} for {}, defaulting to Buy",
                other,
                symbol
            );
            OrderSide::Buy
        }
    };

    Ok(Trade {
        id: t
            .id
            .clone()
            .unwrap_or_else(|| format!("{}-{}", symbol, t.time)),
        symbol: symbol.to_string(),
        price: parse_decimal_required(&t.price, "price")?,
        quantity: parse_decimal_required(&t.size, "size")?,
        side,
        timestamp: nanos_str_to_datetime(&t.time),
    })
}

pub fn to_market_stats(m: &ApiMarketInfo) -> MarketStats {
    let last_price = parse_decimal(&m.last_price, "last_price");
    let mark_price = parse_decimal(&m.mark_price, "mark_price");
    let turnover_24h = parse_decimal(&m.quote_volume_24h, "quote_volume_24h").round_dp(2);
    let volume_24h = if last_price > Decimal::ZERO {
        (turnover_24h / last_price).round_dp(6)
    } else {
        Decimal::ZERO
    };
    let open_interest = parse_decimal(&m.open_interest, "open_interest").round_dp(6);
    let price_change_24h = parse_decimal(&m.change_24h, "change_24h");
    let prev_price = last_price - price_change_24h;
    let price_change_pct = if prev_price > Decimal::ZERO {
        (price_change_24h / prev_price).round_dp(6)
    } else {
        Decimal::ZERO
    };

    MarketStats {
        symbol: global_symbol_from_risex_symbol(&m.base_asset_symbol),
        last_price,
        mark_price,
        index_price: parse_decimal(&m.index_price, "index_price"),
        volume_24h,
        turnover_24h,
        open_interest,
        price_change_24h,
        price_change_pct,
        high_price_24h: parse_decimal(&m.high_24h, "high_24h"),
        low_price_24h: parse_decimal(&m.low_24h, "low_24h"),
        funding_rate: parse_decimal(&m.current_funding_rate, "current_funding_rate"),
        timestamp: Utc::now(),
    }
}

// ── unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_parse_symbol_idempotent() {
        assert_eq!(risex_symbol_from_input("btc"), "BTC/USDC");
        assert_eq!(risex_symbol_from_input("BTC/USDC"), "BTC/USDC");
        assert_eq!(global_symbol_from_risex_symbol("BTC/USDC"), "BTC");
    }

    #[test]
    fn test_normalize_symbol_roundtrip() {
        assert_eq!(
            global_symbol_from_risex_symbol(&risex_symbol_from_input("BTC")),
            "BTC"
        );
        assert_eq!(
            global_symbol_from_risex_symbol(&risex_symbol_from_input("eth")),
            "ETH"
        );
    }

    #[test]
    fn test_interval_to_nanos() {
        assert_eq!(interval_to_nanos("1m").unwrap(), 60_000_000_000);
        assert_eq!(interval_to_nanos("1h").unwrap(), 3_600_000_000_000);
        assert_eq!(interval_to_nanos("1d").unwrap(), 86_400_000_000_000);
        assert!(interval_to_nanos("3m").is_err());
    }

    #[test]
    fn test_nanos_str_to_datetime() {
        // 1_700_000_000 seconds = 2023-11-14T22:13:20Z
        let nanos = 1_700_000_000_000_000_000u64.to_string();
        let dt = nanos_str_to_datetime(&nanos);
        assert_eq!(dt.timestamp(), 1_700_000_000);
    }

    #[test]
    fn test_ticker_volume_turnover() {
        let m = mock_market_info("95000", "100000");
        let ob = mock_empty_ob();
        let ticker = to_ticker(&m, &ob);

        assert_eq!(ticker.turnover_24h, dec!(100000));
        assert!((ticker.volume_24h - dec!(100000) / dec!(95000)).abs() < dec!(0.000001));
        assert!(ticker.volume_24h < ticker.turnover_24h);
    }

    #[test]
    fn test_price_change_pct_is_ratio() {
        let m = mock_market_info_with_change("94050", "-950");
        let ob = mock_empty_ob();
        let ticker = to_ticker(&m, &ob);
        assert!(ticker.price_change_pct > dec!(-0.1));
        assert!(ticker.price_change_pct < dec!(0.0));
        assert!(ticker.price_change_pct > dec!(-1.0));
    }

    #[test]
    fn test_open_interest_value() {
        let m = mock_oi_market("50.5", "95000");
        let oi = to_open_interest(&m);
        assert_eq!(oi.open_interest, dec!(50.5));
        assert!((oi.open_value - dec!(50.5) * dec!(95000)).abs() < dec!(1));
    }

    #[test]
    fn test_funding_rate_is_ratio() {
        let m = mock_funding_market("0.000100000000000000");
        let fr = to_funding_rate(&m);
        assert!(fr.funding_rate > dec!(0.000001));
        assert!(fr.funding_rate < dec!(0.01));
    }

    #[test]
    fn test_funding_interval_hours() {
        let m = mock_funding_interval_market("3600000000000");
        let fr = to_funding_rate(&m);
        assert_eq!(fr.funding_interval, 1);
    }

    #[test]
    fn test_orderbook_sorted_no_cross() {
        let resp = mock_ob_response();
        let ob = to_orderbook(&resp, "BTC", 50);

        for i in 0..ob.bids.len().saturating_sub(1) {
            assert!(
                ob.bids[i].price >= ob.bids[i + 1].price,
                "bids not descending"
            );
        }
        for i in 0..ob.asks.len().saturating_sub(1) {
            assert!(
                ob.asks[i].price <= ob.asks[i + 1].price,
                "asks not ascending"
            );
        }
        if let (Some(bid), Some(ask)) = (ob.bids.first(), ob.asks.first()) {
            assert!(
                bid.price < ask.price,
                "crossed book: bid {} >= ask {}",
                bid.price,
                ask.price
            );
        }
    }

    #[test]
    fn test_orderbook_depth_clamped() {
        let resp = mock_ob_response_large(200);
        let ob = to_orderbook(&resp, "BTC", 10);
        assert!(ob.bids.len() <= 10);
        assert!(ob.asks.len() <= 10);
    }

    #[test]
    fn test_kline_close_time() {
        let bar = mock_kline_bar("1700000000000000000");
        let kline = to_kline("BTC", &bar, "1h").unwrap();
        assert!(kline.close_time > kline.open_time);
        let diff = kline.close_time - kline.open_time;
        assert!(diff.num_minutes() >= 59);
    }

    #[test]
    fn test_decimal_rounding() {
        let m = mock_market_info("95123.456789", "9876543.123456789");
        let ob = mock_empty_ob();
        let ticker = to_ticker(&m, &ob);
        assert_eq!(ticker.turnover_24h.scale(), 2);
        assert!(ticker.volume_24h.scale() <= 6);
    }

    // ── mock helpers ──────────────────────────────────────────────────────────

    fn mock_market_info(last_price: &str, quote_vol: &str) -> ApiMarketInfo {
        ApiMarketInfo {
            market_id: "1".to_string(),
            base_asset_symbol: "BTC/USDC".to_string(),
            quote_asset_symbol: "USDC".to_string(),
            display_name: "BTC/USDC".to_string(),
            active: true,
            visible: Some(true),
            config: mock_config(),
            last_price: Some(last_price.to_string()),
            mark_price: Some(last_price.to_string()),
            index_price: Some(last_price.to_string()),
            high_24h: Some(last_price.to_string()),
            low_24h: Some(last_price.to_string()),
            change_24h: Some("0".to_string()),
            quote_volume_24h: Some(quote_vol.to_string()),
            open_interest: Some("100".to_string()),
            current_funding_rate: Some("0.000100000000000000".to_string()),
            predicted_funding_rate: Some("0.000100000000000000".to_string()),
            funding_rate_8h: None,
            funding_interval: Some("3600000000000".to_string()),
            next_funding_time: None,
        }
    }

    fn mock_market_info_with_change(last_price: &str, change: &str) -> ApiMarketInfo {
        let mut m = mock_market_info(last_price, "1000000");
        m.change_24h = Some(change.to_string());
        m
    }

    fn mock_oi_market(oi: &str, mark: &str) -> ApiMarketInfo {
        let mut m = mock_market_info(mark, "1000000");
        m.open_interest = Some(oi.to_string());
        m
    }

    fn mock_funding_market(rate: &str) -> ApiMarketInfo {
        let mut m = mock_market_info("95000", "1000000");
        m.current_funding_rate = Some(rate.to_string());
        m
    }

    fn mock_funding_interval_market(interval_ns: &str) -> ApiMarketInfo {
        let mut m = mock_market_info("95000", "1000000");
        m.funding_interval = Some(interval_ns.to_string());
        m
    }

    fn mock_config() -> ApiMarketConfig {
        ApiMarketConfig {
            name: "BTC/USDC".to_string(),
            max_leverage: Some("50".to_string()),
            min_order_size: Some("0.001".to_string()),
            step_size: Some("0.001".to_string()),
            step_price: Some("0.1".to_string()),
            open_interest_limit: None,
        }
    }

    fn mock_empty_ob() -> ApiGetOrderbookResponse {
        ApiGetOrderbookResponse {
            market_id: "1".to_string(),
            bids: vec![ApiPriceLevel {
                price: "94990".to_string(),
                quantity: "1.5".to_string(),
                order_count: None,
            }],
            asks: vec![ApiPriceLevel {
                price: "95010".to_string(),
                quantity: "1.2".to_string(),
                order_count: None,
            }],
            total_bids: None,
            total_asks: None,
        }
    }

    fn mock_ob_response() -> ApiGetOrderbookResponse {
        ApiGetOrderbookResponse {
            market_id: "1".to_string(),
            bids: vec![
                ApiPriceLevel {
                    price: "94990".to_string(),
                    quantity: "1.0".to_string(),
                    order_count: None,
                },
                ApiPriceLevel {
                    price: "95000".to_string(),
                    quantity: "2.0".to_string(),
                    order_count: None,
                },
                ApiPriceLevel {
                    price: "94980".to_string(),
                    quantity: "0.5".to_string(),
                    order_count: None,
                },
            ],
            asks: vec![
                ApiPriceLevel {
                    price: "95020".to_string(),
                    quantity: "1.0".to_string(),
                    order_count: None,
                },
                ApiPriceLevel {
                    price: "95010".to_string(),
                    quantity: "2.0".to_string(),
                    order_count: None,
                },
            ],
            total_bids: None,
            total_asks: None,
        }
    }

    fn mock_ob_response_large(n: usize) -> ApiGetOrderbookResponse {
        let bids = (0..n)
            .map(|i| ApiPriceLevel {
                price: format!("{}", 95000 - i),
                quantity: "1".to_string(),
                order_count: None,
            })
            .collect();
        let asks = (0..n)
            .map(|i| ApiPriceLevel {
                price: format!("{}", 95100 + i),
                quantity: "1".to_string(),
                order_count: None,
            })
            .collect();
        ApiGetOrderbookResponse {
            market_id: "1".to_string(),
            bids,
            asks,
            total_bids: None,
            total_asks: None,
        }
    }

    fn mock_kline_bar(time_nanos: &str) -> ApiKlineBar {
        ApiKlineBar {
            market_id: None,
            interval: Some("1h".to_string()),
            time: time_nanos.to_string(),
            open: "95000".to_string(),
            high: "95500".to_string(),
            low: "94500".to_string(),
            close: "95200".to_string(),
            volume: "100".to_string(),
        }
    }
}
