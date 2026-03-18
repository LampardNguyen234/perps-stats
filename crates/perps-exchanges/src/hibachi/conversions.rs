use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, TimeZone, Utc};
use perps_core::{
    FundingRate, Kline, Market, MarketStats, OpenInterest, OrderSide, Orderbook, OrderbookLevel,
    Ticker, Trade,
};
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;

use super::types::*;

// ---- helpers ----

fn parse_decimal(s: &str, field: &str) -> Result<Decimal> {
    Decimal::from_str_exact(s)
        .or_else(|_| {
            s.parse::<f64>()
                .ok()
                .and_then(Decimal::from_f64)
                .ok_or_else(|| anyhow!(""))
        })
        .with_context(|| format!("Failed to parse {} as decimal: {:?}", field, s))
}

fn parse_decimal_opt(opt: &Option<String>, field: &str) -> Decimal {
    opt.as_deref()
        .and_then(|s| parse_decimal(s, field).ok())
        .unwrap_or(Decimal::ZERO)
}

fn unix_secs_to_datetime(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(secs, 0).single().unwrap_or_else(Utc::now)
}

// ---- FutureContract → Market ----

/// Convert a Hibachi `FutureContract` to a core `Market`.
pub fn future_contract_to_market(contract: &FutureContract) -> Result<Market> {
    let min_order_qty = contract
        .min_order_size
        .as_deref()
        .and_then(|s| parse_decimal(s, "min_order_size").ok())
        .unwrap_or(Decimal::ONE);

    let min_order_value = contract
        .min_notional
        .as_deref()
        .and_then(|s| parse_decimal(s, "min_notional").ok())
        .unwrap_or(Decimal::ZERO);

    Ok(Market {
        symbol: contract.underlying_symbol.clone(),
        contract: contract.symbol.clone(),
        contract_size: Decimal::ONE,
        price_scale: 2,
        quantity_scale: 8,
        min_order_qty,
        max_order_qty: Decimal::from(1_000_000),
        min_order_value,
        max_leverage: Decimal::from(20),
    })
}

// ---- Ticker from Prices + Stats + OI + Orderbook top ----

/// Build a core `Ticker` by merging `/prices`, `/stats`, `/open-interest`, and `/orderbook?depth=1`.
///
/// The orderbook call provides `best_bid_qty` and `best_ask_qty` which are not available in
/// the `/prices` response.
pub fn prices_stats_oi_ob_to_ticker(
    prices: &PricesResponse,
    stats: &StatsResponse,
    oi: &OIResponse,
    ob: &OrderbookResponse,
    symbol: String,
) -> Result<Ticker> {
    let last_price = parse_decimal_opt(&prices.trade_price, "trade_price");
    let mark_price = parse_decimal_opt(&prices.mark_price, "mark_price");
    let index_price = parse_decimal_opt(&prices.spot_price, "spot_price");
    let best_bid_price = parse_decimal_opt(&prices.bid_price, "bid_price");
    let best_ask_price = parse_decimal_opt(&prices.ask_price, "ask_price");

    // Extract best bid/ask quantities from the top-of-book orderbook response
    let best_bid_qty = ob
        .bid
        .as_ref()
        .and_then(|side| side.levels.first())
        .and_then(|l| parse_decimal(&l.quantity, "bid.quantity").ok())
        .unwrap_or(Decimal::ZERO);
    let best_ask_qty = ob
        .ask
        .as_ref()
        .and_then(|side| side.levels.first())
        .and_then(|l| parse_decimal(&l.quantity, "ask.quantity").ok())
        .unwrap_or(Decimal::ZERO);

    // volume_24h from /stats is base volume; compute turnover as base × last
    let volume_24h = parse_decimal_opt(&stats.volume_24h, "volume_24h");
    let turnover_24h = if last_price > Decimal::ZERO {
        volume_24h * last_price
    } else {
        Decimal::ZERO
    };

    let high_price_24h = parse_decimal_opt(&stats.high_24h, "high_24h");
    let low_price_24h = parse_decimal_opt(&stats.low_24h, "low_24h");

    let open_interest = parse_decimal_opt(&oi.total_quantity, "total_quantity");
    let open_interest_notional = if open_interest > Decimal::ZERO && mark_price > Decimal::ZERO {
        open_interest * mark_price
    } else {
        Decimal::ZERO
    };

    Ok(Ticker {
        symbol,
        last_price,
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
        price_change_24h: Decimal::ZERO,
        price_change_pct: Decimal::ZERO,
        high_price_24h,
        low_price_24h,
        timestamp: Utc::now(),
    })
}

// ---- Orderbook ----

/// Convert a Hibachi `OrderbookResponse` to a core `Orderbook`, truncating to `depth` levels.
pub fn orderbook_to_orderbook_truncated(
    ob: OrderbookResponse,
    symbol: &str,
    depth: usize,
) -> Result<Orderbook> {
    orderbook_to_orderbook_inner(ob, symbol.to_string(), Some(depth))
}

/// Convert a Hibachi `OrderbookResponse` to a core `Orderbook`.
pub fn orderbook_to_orderbook(ob: OrderbookResponse, symbol: String) -> Result<Orderbook> {
    orderbook_to_orderbook_inner(ob, symbol, None)
}

fn orderbook_to_orderbook_inner(
    ob: OrderbookResponse,
    symbol: String,
    depth: Option<usize>,
) -> Result<Orderbook> {
    let mut bids = ob
        .bid
        .map(|side| side.levels)
        .unwrap_or_default()
        .iter()
        .map(|l| {
            Ok(OrderbookLevel {
                price: parse_decimal(&l.price, "bid.price")?,
                quantity: parse_decimal(&l.quantity, "bid.quantity")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let mut asks = ob
        .ask
        .map(|side| side.levels)
        .unwrap_or_default()
        .iter()
        .map(|l| {
            Ok(OrderbookLevel {
                price: parse_decimal(&l.price, "ask.price")?,
                quantity: parse_decimal(&l.quantity, "ask.quantity")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    bids.sort_by(|a, b| b.price.cmp(&a.price));
    asks.sort_by(|a, b| a.price.cmp(&b.price));

    if let Some(d) = depth {
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

// ---- FundingRate from PricesResponse ----

/// Extract a `FundingRate` from a Hibachi `/prices` response.
pub fn prices_to_funding_rate(prices: &PricesResponse, symbol: String) -> Result<FundingRate> {
    let fr_est = prices.funding_rate_estimation.as_ref();

    let funding_rate = fr_est
        .and_then(|f| f.estimated_funding_rate.as_deref())
        .and_then(|s| parse_decimal(s, "estimated_funding_rate").ok())
        .unwrap_or(Decimal::ZERO);

    let next_funding_time = fr_est
        .and_then(|f| f.next_funding_timestamp)
        .map(unix_secs_to_datetime)
        .unwrap_or_else(|| Utc::now() + chrono::Duration::hours(8));

    Ok(FundingRate {
        symbol,
        funding_rate,
        predicted_rate: funding_rate,
        funding_time: Utc::now(),
        next_funding_time,
        funding_interval: 8,
        funding_rate_cap_floor: Decimal::from_str_exact("0.75").unwrap_or(Decimal::ZERO),
    })
}

// ---- FundingRateEntry → FundingRate ----

/// Convert a Hibachi funding rate history entry to a core `FundingRate`.
pub fn funding_rate_entry_to_funding_rate(
    entry: &FundingRateEntry,
    symbol: String,
) -> Result<FundingRate> {
    let funding_rate = entry
        .funding_rate
        .as_deref()
        .map(|s| parse_decimal(s, "funding_rate"))
        .transpose()?
        .unwrap_or(Decimal::ZERO);

    let funding_time = entry
        .funding_timestamp
        .map(|ts| unix_secs_to_datetime(ts as i64))
        .unwrap_or_else(Utc::now);

    Ok(FundingRate {
        symbol,
        funding_rate,
        predicted_rate: funding_rate,
        funding_time,
        next_funding_time: funding_time + chrono::Duration::hours(8),
        funding_interval: 8,
        funding_rate_cap_floor: Decimal::from_str_exact("0.75").unwrap_or(Decimal::ZERO),
    })
}

// ---- OpenInterest ----

/// Convert a Hibachi OI response to a core `OpenInterest`.
pub fn oi_response_to_open_interest(
    oi: &OIResponse,
    mark_price: Decimal,
    symbol: String,
) -> Result<OpenInterest> {
    let open_interest = parse_decimal_opt(&oi.total_quantity, "total_quantity");
    let open_value = if open_interest > Decimal::ZERO && mark_price > Decimal::ZERO {
        open_interest * mark_price
    } else {
        Decimal::ZERO
    };

    Ok(OpenInterest {
        symbol,
        open_interest,
        open_value,
        timestamp: Utc::now(),
    })
}

// ---- KlineEntry → Kline ----

/// Convert a Hibachi kline entry to a core `Kline`.
///
/// `volume_notional` is quote volume; base volume is derived as `notional / close`.
pub fn kline_to_kline(k: &KlineEntry, symbol: String, interval: String) -> Result<Kline> {
    let open = parse_decimal_opt(&k.open, "open");
    let high = parse_decimal_opt(&k.high, "high");
    let low = parse_decimal_opt(&k.low, "low");
    let close = parse_decimal_opt(&k.close, "close");
    let turnover = parse_decimal_opt(&k.volume_notional, "volume_notional");
    let volume = if close > Decimal::ZERO {
        turnover / close
    } else {
        Decimal::ZERO
    };

    let open_time = k
        .timestamp
        .map(unix_secs_to_datetime)
        .unwrap_or_else(Utc::now);

    Ok(Kline {
        symbol,
        interval,
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

// ---- TradeEntry → Trade ----

/// Convert a Hibachi trade entry to a core `Trade`.
pub fn trade_entry_to_trade(t: &TradeEntry, symbol: String) -> Result<Trade> {
    let price = t
        .price
        .as_deref()
        .map(|s| parse_decimal(s, "price"))
        .transpose()?
        .unwrap_or(Decimal::ZERO);

    let quantity = t
        .quantity
        .as_deref()
        .map(|s| parse_decimal(s, "quantity"))
        .transpose()?
        .unwrap_or(Decimal::ZERO);

    let side = match t.taker_side.as_deref() {
        Some("Buy") | Some("buy") => OrderSide::Buy,
        Some("Sell") | Some("sell") => OrderSide::Sell,
        _ => OrderSide::Buy,
    };

    let timestamp = t
        .timestamp
        .map(unix_secs_to_datetime)
        .unwrap_or_else(Utc::now);

    Ok(Trade {
        id: timestamp.timestamp_millis().to_string(),
        symbol,
        price,
        quantity,
        side,
        timestamp,
    })
}

// ---- MarketStats from Prices + Stats + OI ----

/// Build a core `MarketStats` by merging responses from `/prices`, `/stats`, and `/open-interest`.
pub fn prices_stats_oi_to_market_stats(
    prices: &PricesResponse,
    stats: &StatsResponse,
    oi: &OIResponse,
    symbol: String,
) -> Result<MarketStats> {
    let last_price = parse_decimal_opt(&prices.trade_price, "trade_price");
    let mark_price = parse_decimal_opt(&prices.mark_price, "mark_price");
    let index_price = parse_decimal_opt(&prices.spot_price, "spot_price");
    let volume_24h = parse_decimal_opt(&stats.volume_24h, "volume_24h");
    let turnover_24h = if last_price > Decimal::ZERO {
        volume_24h * last_price
    } else {
        Decimal::ZERO
    };
    let high_price_24h = parse_decimal_opt(&stats.high_24h, "high_24h");
    let low_price_24h = parse_decimal_opt(&stats.low_24h, "low_24h");
    let open_interest = parse_decimal_opt(&oi.total_quantity, "total_quantity");
    let funding_rate = prices
        .funding_rate_estimation
        .as_ref()
        .and_then(|f| f.estimated_funding_rate.as_deref())
        .and_then(|s| parse_decimal(s, "estimated_funding_rate").ok())
        .unwrap_or(Decimal::ZERO);

    Ok(MarketStats {
        symbol,
        last_price,
        mark_price,
        index_price,
        volume_24h,
        turnover_24h,
        price_change_24h: Decimal::ZERO,
        price_change_pct: Decimal::ZERO,
        high_price_24h,
        low_price_24h,
        open_interest,
        funding_rate,
        timestamp: Utc::now(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_prices() -> PricesResponse {
        PricesResponse {
            trade_price: Some("95000.0".to_string()),
            mark_price: Some("95010.0".to_string()),
            spot_price: Some("95005.0".to_string()),
            ask_price: Some("95020.0".to_string()),
            bid_price: Some("94990.0".to_string()),
            funding_rate_estimation: Some(FundingRateEstimation {
                estimated_funding_rate: Some("0.0001".to_string()),
                next_funding_timestamp: Some(1_700_000_000),
            }),
        }
    }

    fn make_stats() -> StatsResponse {
        StatsResponse {
            high_24h: Some("96000.0".to_string()),
            low_24h: Some("94000.0".to_string()),
            volume_24h: Some("100.0".to_string()),
        }
    }

    fn make_oi() -> OIResponse {
        OIResponse {
            total_quantity: Some("50.5".to_string()),
        }
    }

    #[test]
    fn test_ticker_conversion() {
        let prices = make_prices();
        let stats = make_stats();
        let oi = make_oi();

        let ob = OrderbookResponse {
            bid: Some(OrderbookSide {
                levels: vec![OrderbookLevel {
                    price: "94990.0".to_string(),
                    quantity: "1.5".to_string(),
                }],
            }),
            ask: Some(OrderbookSide {
                levels: vec![OrderbookLevel {
                    price: "95020.0".to_string(),
                    quantity: "0.8".to_string(),
                }],
            }),
        };
        let ticker =
            prices_stats_oi_ob_to_ticker(&prices, &stats, &oi, &ob, "BTC".to_string()).unwrap();

        assert_eq!(ticker.symbol, "BTC");
        assert_eq!(
            ticker.last_price,
            Decimal::from_str_exact("95000.0").unwrap()
        );
        assert_eq!(
            ticker.mark_price,
            Decimal::from_str_exact("95010.0").unwrap()
        );
        assert_eq!(
            ticker.best_bid_price,
            Decimal::from_str_exact("94990.0").unwrap()
        );
        assert_eq!(
            ticker.best_ask_price,
            Decimal::from_str_exact("95020.0").unwrap()
        );
        assert_eq!(
            ticker.high_price_24h,
            Decimal::from_str_exact("96000.0").unwrap()
        );
        assert_eq!(
            ticker.open_interest,
            Decimal::from_str_exact("50.5").unwrap()
        );
        assert_eq!(ticker.best_bid_qty, Decimal::from_str_exact("1.5").unwrap());
        assert_eq!(ticker.best_ask_qty, Decimal::from_str_exact("0.8").unwrap());
        // turnover = 100 * 95000
        assert!(ticker.turnover_24h > Decimal::ZERO);
    }

    #[test]
    fn test_funding_rate_from_prices() {
        let prices = make_prices();
        let fr = prices_to_funding_rate(&prices, "BTC".to_string()).unwrap();

        assert_eq!(fr.symbol, "BTC");
        assert_eq!(fr.funding_rate, Decimal::from_str_exact("0.0001").unwrap());
    }

    #[test]
    fn test_trade_conversion_buy() {
        let entry = TradeEntry {
            price: Some("95000.0".to_string()),
            quantity: Some("0.5".to_string()),
            taker_side: Some("Buy".to_string()),
            timestamp: Some(1_700_000_000),
        };
        let trade = trade_entry_to_trade(&entry, "BTC".to_string()).unwrap();

        assert_eq!(trade.symbol, "BTC");
        assert_eq!(trade.side, OrderSide::Buy);
        assert_eq!(trade.price, Decimal::from_str_exact("95000.0").unwrap());
    }

    #[test]
    fn test_trade_conversion_sell() {
        let entry = TradeEntry {
            price: Some("95000.0".to_string()),
            quantity: Some("0.5".to_string()),
            taker_side: Some("Sell".to_string()),
            timestamp: Some(1_700_000_000),
        };
        let trade = trade_entry_to_trade(&entry, "BTC".to_string()).unwrap();

        assert_eq!(trade.side, OrderSide::Sell);
    }

    #[test]
    fn test_kline_conversion() {
        let entry = KlineEntry {
            open: Some("94000.0".to_string()),
            high: Some("96000.0".to_string()),
            low: Some("93000.0".to_string()),
            close: Some("95000.0".to_string()),
            volume_notional: Some("9500000.0".to_string()), // 100 BTC × 95000
            timestamp: Some(1_700_000_000),
        };
        let kline = kline_to_kline(&entry, "BTC".to_string(), "1h".to_string()).unwrap();

        assert_eq!(kline.symbol, "BTC");
        assert_eq!(kline.interval, "1h");
        assert_eq!(kline.close, Decimal::from_str_exact("95000.0").unwrap());
        // volume = notional / close = 9500000 / 95000 = 100
        assert_eq!(kline.volume, Decimal::from(100));
    }

    #[test]
    fn test_market_stats_conversion() {
        let prices = make_prices();
        let stats = make_stats();
        let oi = make_oi();

        let ms = prices_stats_oi_to_market_stats(&prices, &stats, &oi, "BTC".to_string()).unwrap();

        assert_eq!(ms.symbol, "BTC");
        assert_eq!(ms.funding_rate, Decimal::from_str_exact("0.0001").unwrap());
        assert_eq!(ms.open_interest, Decimal::from_str_exact("50.5").unwrap());
    }
}
