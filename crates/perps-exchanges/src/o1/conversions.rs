use super::types::*;
use anyhow::{anyhow, Result};
use chrono::{DateTime, Duration, Utc};
use perps_core::*;
use rust_decimal::Decimal;
use std::str::FromStr;

/// Convert an `f64` value to `Decimal` via string to preserve clean formatting.
///
/// Going through the string representation avoids the floating-point noise that
/// `Decimal::from_f64_retain()` introduces (e.g. `0.00144` staying as `0.00144`
/// instead of `0.0014400000000000000906913433`).
fn f64_to_decimal(v: f64) -> Decimal {
    if v.is_nan() || v.is_infinite() {
        return Decimal::ZERO;
    }
    Decimal::from_str(&v.to_string()).unwrap_or(Decimal::ZERO)
}

/// Convert an `Option<f64>` value to `Decimal`, returning `Decimal::ZERO`
/// when the value is `None` or not representable.
fn opt_f64_to_decimal(v: Option<f64>) -> Decimal {
    v.map(f64_to_decimal).unwrap_or(Decimal::ZERO)
}

/// Convert a `NordMarketInfo` to a perps_core `Market`.
///
/// The global symbol is derived by stripping the trailing "USD" suffix from
/// the exchange symbol (e.g. "BTCUSD" becomes "BTC"). Max leverage is
/// calculated as `1 / imf` (initial margin fraction).
pub fn nord_market_to_market(m: &NordMarketInfo) -> Market {
    // Strip "USD" suffix to get global symbol (e.g., "BTCUSD" → "BTC")
    let symbol = m
        .symbol
        .strip_suffix("USD")
        .unwrap_or(&m.symbol)
        .to_string();

    let max_leverage = if m.imf > 0.0 {
        f64_to_decimal(1.0 / m.imf)
    } else {
        Decimal::from(50)
    };

    Market {
        symbol,
        contract: m.symbol.clone(),
        contract_size: Decimal::ONE,
        price_scale: m.price_decimals as i32,
        quantity_scale: m.size_decimals as i32,
        min_order_qty: Decimal::ZERO,
        max_order_qty: Decimal::ZERO,
        min_order_value: Decimal::ZERO,
        max_leverage,
    }
}

/// Convert a `NordMarketStats` and `NordOrderbookInfo` pair into a perps_core `Ticker`.
///
/// Uses `close_24h` as a proxy for the last traded price. Best bid/ask are
/// taken from the top of the orderbook. Price change is computed from
/// `close_24h - prev_close_24h`.
pub fn nord_stats_and_orderbook_to_ticker(
    stats: &NordMarketStats,
    ob: &NordOrderbookInfo,
    symbol: String,
) -> Result<Ticker> {
    let close = opt_f64_to_decimal(stats.close_24h);
    let prev_close = opt_f64_to_decimal(stats.prev_close_24h);

    let mark_price = stats
        .perp_stats
        .as_ref()
        .and_then(|ps| ps.mark_price)
        .map(f64_to_decimal)
        .unwrap_or(Decimal::ZERO);

    let index_price = opt_f64_to_decimal(stats.index_price);

    let (best_bid_price, best_bid_qty) = ob
        .bids
        .first()
        .map(|&(p, q)| (f64_to_decimal(p), f64_to_decimal(q)))
        .unwrap_or((Decimal::ZERO, Decimal::ZERO));

    let (best_ask_price, best_ask_qty) = ob
        .asks
        .first()
        .map(|&(p, q)| (f64_to_decimal(p), f64_to_decimal(q)))
        .unwrap_or((Decimal::ZERO, Decimal::ZERO));

    let volume_24h = f64_to_decimal(stats.volume_base_24h);
    let turnover_24h = f64_to_decimal(stats.volume_quote_24h);

    let open_interest = stats
        .perp_stats
        .as_ref()
        .map(|ps| f64_to_decimal(ps.open_interest))
        .unwrap_or(Decimal::ZERO);

    let open_interest_notional = open_interest * close;

    let price_change_24h = close - prev_close;
    let price_change_pct = if prev_close > Decimal::ZERO {
        (price_change_24h / prev_close) * Decimal::from(100)
    } else {
        Decimal::ZERO
    };

    Ok(Ticker {
        symbol,
        last_price: close,
        mark_price,
        index_price,
        best_bid_price,
        best_bid_qty,
        best_ask_price,
        best_ask_qty,
        volume_24h,
        turnover_24h,
        open_interest,
        open_interest_notional,
        price_change_24h,
        price_change_pct,
        high_price_24h: opt_f64_to_decimal(stats.high_24h),
        low_price_24h: opt_f64_to_decimal(stats.low_24h),
        timestamp: Utc::now(),
    })
}

/// Convert a `NordOrderbookInfo` into a perps_core `Orderbook`.
///
/// Each `(price, size)` tuple is mapped to an `OrderbookLevel`.
pub fn nord_orderbook_to_orderbook(ob: &NordOrderbookInfo, symbol: String) -> Orderbook {
    let bids = ob
        .bids
        .iter()
        .map(|&(price, size)| OrderbookLevel {
            price: f64_to_decimal(price),
            quantity: f64_to_decimal(size),
        })
        .collect();

    let asks = ob
        .asks
        .iter()
        .map(|&(price, size)| OrderbookLevel {
            price: f64_to_decimal(price),
            quantity: f64_to_decimal(size),
        })
        .collect();

    Orderbook {
        symbol,
        bids,
        asks,
        timestamp: Utc::now(),
    }
}

/// Convert `NordMarketStats` perp stats into a perps_core `FundingRate`.
///
/// Returns an error if `perp_stats` is absent.  `next_funding_time` is
/// parsed as RFC 3339; if parsing fails, `Utc::now()` is used as a fallback.
pub fn nord_stats_to_funding_rate(stats: &NordMarketStats, symbol: String) -> Result<FundingRate> {
    let perp = stats
        .perp_stats
        .as_ref()
        .ok_or_else(|| anyhow!("no perp_stats available for {}", symbol))?;

    let funding_rate = f64_to_decimal(perp.funding_rate);

    let next_funding_time: DateTime<Utc> = DateTime::parse_from_rfc3339(&perp.next_funding_time)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now());

    // Funding time is inferred as one interval before next_funding_time
    let funding_time = next_funding_time - Duration::hours(1);

    Ok(FundingRate {
        symbol,
        funding_rate,
        predicted_rate: funding_rate,
        funding_time,
        next_funding_time,
        funding_interval: 1,
        funding_rate_cap_floor: Decimal::ZERO,
    })
}

/// Convert `NordMarketStats` perp stats into a perps_core `OpenInterest`.
///
/// Returns an error if `perp_stats` is absent.  The notional value is
/// estimated as `open_interest * close_24h`.
pub fn nord_stats_to_open_interest(
    stats: &NordMarketStats,
    symbol: String,
) -> Result<OpenInterest> {
    let perp = stats
        .perp_stats
        .as_ref()
        .ok_or_else(|| anyhow!("no perp_stats available for {}", symbol))?;

    let open_interest = f64_to_decimal(perp.open_interest);
    let close = opt_f64_to_decimal(stats.close_24h);
    let open_value = open_interest * close;

    Ok(OpenInterest {
        symbol,
        open_interest,
        open_value,
        timestamp: Utc::now(),
    })
}

/// Convert `NordMarketStats` into a perps_core `MarketStats`.
///
/// Combines price, volume, open interest, and funding data from the stats
/// and its nested `perp_stats` field. Returns an error if `perp_stats` is
/// absent.
pub fn nord_stats_to_market_stats(stats: &NordMarketStats, symbol: String) -> Result<MarketStats> {
    let perp = stats
        .perp_stats
        .as_ref()
        .ok_or_else(|| anyhow!("no perp_stats available for {}", symbol))?;

    let close = opt_f64_to_decimal(stats.close_24h);
    let prev_close = opt_f64_to_decimal(stats.prev_close_24h);
    let price_change_24h = close - prev_close;
    let price_change_pct = if prev_close > Decimal::ZERO {
        (price_change_24h / prev_close) * Decimal::from(100)
    } else {
        Decimal::ZERO
    };

    let mark_price = perp.mark_price.map(f64_to_decimal).unwrap_or(Decimal::ZERO);

    Ok(MarketStats {
        symbol,
        volume_24h: f64_to_decimal(stats.volume_base_24h),
        turnover_24h: f64_to_decimal(stats.volume_quote_24h),
        open_interest: f64_to_decimal(perp.open_interest),
        funding_rate: f64_to_decimal(perp.funding_rate),
        last_price: close,
        mark_price,
        index_price: opt_f64_to_decimal(stats.index_price),
        price_change_24h,
        price_change_pct,
        high_price_24h: opt_f64_to_decimal(stats.high_24h),
        low_price_24h: opt_f64_to_decimal(stats.low_24h),
        timestamp: Utc::now(),
    })
}

/// Convert a `NordMarketHistoryInfo` hourly snapshot into a perps_core `Kline`.
///
/// Note: The Nord history endpoint returns hourly market snapshots (index/mark price,
/// funding data), NOT traditional OHLCV candles. We map `mark_price` as the price
/// fields and set volume to zero since it's not available per-hour.
pub fn nord_history_to_kline(h: &NordMarketHistoryInfo, symbol: String) -> Result<Kline> {
    let open_time: DateTime<Utc> = DateTime::parse_from_rfc3339(&h.time)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| anyhow!("failed to parse kline time '{}': {}", h.time, e))?;

    let close_time = open_time + Duration::hours(1);
    let price = f64_to_decimal(h.mark_price);

    Ok(Kline {
        symbol,
        interval: "1h".to_string(),
        open_time,
        close_time,
        open: price,
        high: price,
        low: price,
        close: price,
        volume: Decimal::ZERO,
        turnover: Decimal::ZERO,
    })
}

/// Convert a `NordTrade` into a perps_core `Trade`.
///
/// The side string `"bid"` maps to `OrderSide::Buy` and `"ask"` maps to
/// `OrderSide::Sell`. The timestamp is parsed as RFC 3339.
pub fn nord_trade_to_trade(t: &NordTrade, symbol: String) -> Result<Trade> {
    let side = match t.taker_side.as_str() {
        "bid" => OrderSide::Buy,
        "ask" => OrderSide::Sell,
        other => return Err(anyhow!("unknown trade side: {}", other)),
    };

    let timestamp: DateTime<Utc> = DateTime::parse_from_rfc3339(&t.time)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| anyhow!("failed to parse trade timestamp '{}': {}", t.time, e))?;

    Ok(Trade {
        id: t.trade_id.to_string(),
        symbol,
        price: f64_to_decimal(t.price),
        quantity: f64_to_decimal(t.base_size),
        side,
        timestamp,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_f64_to_decimal() {
        assert_eq!(f64_to_decimal(0.0), Decimal::ZERO);
        assert_eq!(f64_to_decimal(1.5), Decimal::from_str("1.5").unwrap());
        assert_eq!(f64_to_decimal(0.00144), Decimal::from_str("0.00144").unwrap());
        // NaN should fall back to ZERO
        assert_eq!(f64_to_decimal(f64::NAN), Decimal::ZERO);
        assert_eq!(f64_to_decimal(f64::INFINITY), Decimal::ZERO);
    }

    #[test]
    fn test_nord_market_to_market() {
        let m = NordMarketInfo {
            market_id: 0,
            symbol: "BTCUSD".to_string(),
            price_decimals: 1,
            size_decimals: 5,
            base_token_id: 0,
            quote_token_id: 0,
            imf: 0.02,
            mmf: 0.01,
            cmf: 0.0125,
        };

        let market = nord_market_to_market(&m);
        assert_eq!(market.symbol, "BTC");
        assert_eq!(market.contract, "BTCUSD");
        assert_eq!(market.contract_size, Decimal::ONE);
        assert_eq!(market.price_scale, 1);
        assert_eq!(market.quantity_scale, 5);
        assert_eq!(market.min_order_qty, Decimal::ZERO);
        assert_eq!(market.max_order_qty, Decimal::ZERO);
        // 1 / 0.02 = 50
        assert_eq!(market.max_leverage, f64_to_decimal(50.0));
    }

    #[test]
    fn test_nord_stats_to_ticker() {
        let stats = NordMarketStats {
            index_price: Some(100000.0),
            index_price_conf: Some(0.5),
            frozen: None,
            volume_base_24h: 500.0,
            volume_quote_24h: 50000000.0,
            high_24h: Some(101000.0),
            low_24h: Some(99000.0),
            close_24h: Some(100500.0),
            prev_close_24h: Some(100000.0),
            perp_stats: Some(NordPerpStats {
                mark_price: Some(100450.0),
                aggregated_funding_index: 1.0,
                funding_rate: 0.0001,
                next_funding_time: "2026-03-10T12:00:00Z".to_string(),
                open_interest: 1200.0,
            }),
        };

        let ob = NordOrderbookInfo {
            update_id: 1,
            asks: vec![(100550.0, 0.5)],
            bids: vec![(100450.0, 0.3)],
            asks_summary: NordOrderbookSummary {
                sum: 100.0,
                count: 50,
            },
            bids_summary: NordOrderbookSummary {
                sum: 95.0,
                count: 45,
            },
        };

        let ticker = nord_stats_and_orderbook_to_ticker(&stats, &ob, "BTC".to_string()).unwrap();
        assert_eq!(ticker.symbol, "BTC");
        assert_eq!(ticker.last_price, f64_to_decimal(100500.0));
        assert_eq!(ticker.mark_price, f64_to_decimal(100450.0));
        assert_eq!(ticker.index_price, f64_to_decimal(100000.0));
        assert_eq!(ticker.best_bid_price, f64_to_decimal(100450.0));
        assert_eq!(ticker.best_ask_price, f64_to_decimal(100550.0));
        assert_eq!(ticker.volume_24h, f64_to_decimal(500.0));
        assert_eq!(ticker.high_price_24h, f64_to_decimal(101000.0));
        assert_eq!(ticker.low_price_24h, f64_to_decimal(99000.0));
        // price_change_24h = 100500 - 100000 = 500
        assert_eq!(ticker.price_change_24h, f64_to_decimal(500.0));
    }

    #[test]
    fn test_nord_orderbook_to_orderbook() {
        let ob = NordOrderbookInfo {
            update_id: 42,
            asks: vec![(100.5, 1.0), (101.0, 2.0)],
            bids: vec![(100.0, 1.5), (99.5, 3.0)],
            asks_summary: NordOrderbookSummary { sum: 3.0, count: 2 },
            bids_summary: NordOrderbookSummary { sum: 4.5, count: 2 },
        };

        let orderbook = nord_orderbook_to_orderbook(&ob, "ETH".to_string());
        assert_eq!(orderbook.symbol, "ETH");
        assert_eq!(orderbook.bids.len(), 2);
        assert_eq!(orderbook.asks.len(), 2);
        assert_eq!(orderbook.bids[0].price, f64_to_decimal(100.0));
        assert_eq!(orderbook.bids[0].quantity, f64_to_decimal(1.5));
        assert_eq!(orderbook.asks[0].price, f64_to_decimal(100.5));
        assert_eq!(orderbook.asks[0].quantity, f64_to_decimal(1.0));
    }

    #[test]
    fn test_nord_trade_to_trade_bid() {
        let t = NordTrade {
            trade_id: 123,
            maker_id: 1,
            taker_id: 2,
            market_id: 0,
            taker_side: "bid".to_string(),
            price: 100000.0,
            base_size: 0.5,
            order_id: 999,
            action_id: Some(1000),
            time: "2026-03-10T10:00:00Z".to_string(),
        };

        let trade = nord_trade_to_trade(&t, "BTC".to_string()).unwrap();
        assert_eq!(trade.id, "123");
        assert_eq!(trade.side, OrderSide::Buy);
        assert_eq!(trade.price, f64_to_decimal(100000.0));
        assert_eq!(trade.quantity, f64_to_decimal(0.5));
    }

    #[test]
    fn test_nord_trade_to_trade_ask() {
        let t = NordTrade {
            trade_id: 456,
            maker_id: 3,
            taker_id: 4,
            market_id: 0,
            taker_side: "ask".to_string(),
            price: 50000.0,
            base_size: 1.0,
            order_id: 888,
            action_id: None,
            time: "2026-03-10T11:00:00Z".to_string(),
        };

        let trade = nord_trade_to_trade(&t, "ETH".to_string()).unwrap();
        assert_eq!(trade.side, OrderSide::Sell);
    }

    #[test]
    fn test_nullable_fields_default_zero() {
        let stats = NordMarketStats {
            index_price: None,
            index_price_conf: None,
            frozen: None,
            volume_base_24h: 0.0,
            volume_quote_24h: 0.0,
            high_24h: None,
            low_24h: None,
            close_24h: None,
            prev_close_24h: None,
            perp_stats: None,
        };

        let ob = NordOrderbookInfo {
            update_id: 0,
            asks: vec![],
            bids: vec![],
            asks_summary: NordOrderbookSummary { sum: 0.0, count: 0 },
            bids_summary: NordOrderbookSummary { sum: 0.0, count: 0 },
        };

        let ticker = nord_stats_and_orderbook_to_ticker(&stats, &ob, "BTC".to_string()).unwrap();
        assert_eq!(ticker.last_price, Decimal::ZERO);
        assert_eq!(ticker.mark_price, Decimal::ZERO);
        assert_eq!(ticker.index_price, Decimal::ZERO);
        assert_eq!(ticker.best_bid_price, Decimal::ZERO);
        assert_eq!(ticker.best_ask_price, Decimal::ZERO);
        assert_eq!(ticker.open_interest, Decimal::ZERO);
        assert_eq!(ticker.price_change_24h, Decimal::ZERO);
        assert_eq!(ticker.price_change_pct, Decimal::ZERO);
    }

    #[test]
    fn test_nord_stats_to_funding_rate() {
        let stats = NordMarketStats {
            index_price: Some(100000.0),
            index_price_conf: None,
            frozen: None,
            volume_base_24h: 0.0,
            volume_quote_24h: 0.0,
            high_24h: None,
            low_24h: None,
            close_24h: None,
            prev_close_24h: None,
            perp_stats: Some(NordPerpStats {
                mark_price: Some(100000.0),
                aggregated_funding_index: 1.0,
                funding_rate: 0.0001,
                next_funding_time: "2026-03-10T12:00:00Z".to_string(),
                open_interest: 500.0,
            }),
        };

        let fr = nord_stats_to_funding_rate(&stats, "BTC".to_string()).unwrap();
        assert_eq!(fr.symbol, "BTC");
        assert_eq!(fr.funding_rate, f64_to_decimal(0.0001));
        assert_eq!(fr.funding_interval, 1);
    }

    #[test]
    fn test_nord_stats_to_funding_rate_no_perp_stats() {
        let stats = NordMarketStats {
            index_price: None,
            index_price_conf: None,
            frozen: None,
            volume_base_24h: 0.0,
            volume_quote_24h: 0.0,
            high_24h: None,
            low_24h: None,
            close_24h: None,
            prev_close_24h: None,
            perp_stats: None,
        };

        let result = nord_stats_to_funding_rate(&stats, "BTC".to_string());
        assert!(result.is_err());
    }

    #[test]
    fn test_nord_history_to_kline() {
        let h = NordMarketHistoryInfo {
            market_id: 0,
            time: "2026-03-10T10:00:00Z".to_string(),
            action_id: 12345,
            funding_index: -100000.0,
            funding_rate: 0.0001,
            index_price: 100000.0,
            mark_price: 100500.0,
        };

        let kline = nord_history_to_kline(&h, "BTC".to_string()).unwrap();
        assert_eq!(kline.symbol, "BTC");
        assert_eq!(kline.interval, "1h");
        // mark_price is used as the price for all OHLC fields
        assert_eq!(kline.open, f64_to_decimal(100500.0));
        assert_eq!(kline.close, f64_to_decimal(100500.0));
        assert_eq!(kline.volume, Decimal::ZERO);
        assert_eq!(kline.turnover, Decimal::ZERO);
        // close_time should be 1 hour after open_time
        assert_eq!(kline.close_time - kline.open_time, Duration::hours(1));
    }

    #[test]
    fn test_nord_stats_to_open_interest() {
        let stats = NordMarketStats {
            index_price: Some(100000.0),
            index_price_conf: None,
            frozen: None,
            volume_base_24h: 0.0,
            volume_quote_24h: 0.0,
            high_24h: None,
            low_24h: None,
            close_24h: Some(100500.0),
            prev_close_24h: None,
            perp_stats: Some(NordPerpStats {
                mark_price: Some(100000.0),
                aggregated_funding_index: 1.0,
                funding_rate: 0.0001,
                next_funding_time: "2026-03-10T12:00:00Z".to_string(),
                open_interest: 1200.0,
            }),
        };

        let oi = nord_stats_to_open_interest(&stats, "BTC".to_string()).unwrap();
        assert_eq!(oi.symbol, "BTC");
        assert_eq!(oi.open_interest, f64_to_decimal(1200.0));
        // open_value = 1200 * 100500
        let expected_value = f64_to_decimal(1200.0) * f64_to_decimal(100500.0);
        assert_eq!(oi.open_value, expected_value);
    }

    #[test]
    fn test_nord_stats_to_open_interest_no_perp_stats() {
        let stats = NordMarketStats {
            index_price: None,
            index_price_conf: None,
            frozen: None,
            volume_base_24h: 0.0,
            volume_quote_24h: 0.0,
            high_24h: None,
            low_24h: None,
            close_24h: None,
            prev_close_24h: None,
            perp_stats: None,
        };

        let result = nord_stats_to_open_interest(&stats, "BTC".to_string());
        assert!(result.is_err());
    }
}
