use anyhow::Result;
use chrono::{DateTime, TimeZone, Utc};
use perps_core::{FundingRate as CoreFundingRate, Kline, Market as CoreMarket, Orderbook, OrderbookLevel, Ticker};
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;
use std::str::FromStr;

use super::models::{Candlestick, FundingRate, Order, OrderBook, OrderBookDetail};

/// Convert Lighter OrderBook to core Market
pub fn to_market(orderbook: &OrderBook) -> Result<CoreMarket> {
    Ok(CoreMarket {
        symbol: orderbook.symbol.clone(),
        contract: format!("{}-PERP", orderbook.symbol), // Lighter perps format
        contract_size: Decimal::from(1), // 1:1 for most perps
        price_scale: -(orderbook.supported_price_decimals as i32),
        quantity_scale: -(orderbook.supported_size_decimals as i32),
        min_order_qty: Decimal::from_str(&orderbook.min_base_amount)?,
        max_order_qty: Decimal::from(1000000), // Default max, not provided by API
        min_order_value: Decimal::from_str(&orderbook.min_quote_amount)?,
        max_leverage: Decimal::from(20), // Default, not provided in basic order book response
    })
}

/// Convert Lighter OrderBookDetail to core Ticker
pub fn to_ticker(detail: &OrderBookDetail) -> Result<Ticker> {
    let last_price = Decimal::from_f64(detail.last_trade_price)
        .unwrap_or_else(|| Decimal::from(0));

    let price_change_pct_raw = Decimal::from_f64(detail.daily_price_change)
        .unwrap_or_else(|| Decimal::from(0));

    // Convert percentage to decimal to match other exchanges (e.g., -1.19% -> -0.0119)
    let price_change_pct = price_change_pct_raw / Decimal::from(100);

    let price_change_24h = if last_price > Decimal::ZERO {
        price_change_pct * last_price
    } else {
        Decimal::ZERO
    };

    Ok(Ticker {
        symbol: detail.symbol.clone(),
        last_price,
        mark_price: last_price, // Use last price as mark price
        index_price: last_price, // Use last price as index price
        best_bid_price: last_price * Decimal::new(9999, 4), // Approximate (99.99% of last)
        best_bid_qty: Decimal::ZERO, // Not provided
        best_ask_price: last_price * Decimal::new(10001, 4), // Approximate (100.01% of last)
        best_ask_qty: Decimal::ZERO, // Not provided
        volume_24h: Decimal::from_f64(detail.daily_base_token_volume)
            .unwrap_or_else(|| Decimal::from(0)),
        turnover_24h: Decimal::from_f64(detail.daily_quote_token_volume)
            .unwrap_or_else(|| Decimal::from(0)),
        open_interest: Decimal::ZERO,
        open_interest_notional: Decimal::ZERO,
        price_change_24h,
        price_change_pct,
        high_price_24h: Decimal::from_f64(detail.daily_price_high)
            .unwrap_or_else(|| Decimal::from(0)),
        low_price_24h: Decimal::from_f64(detail.daily_price_low)
            .unwrap_or_else(|| Decimal::from(0)),
        timestamp: Utc::now(),
    })
}

/// Convert Lighter OrderBookDetail to core Ticker with orderbook data for best bid/ask
pub fn to_ticker_with_orderbook(detail: &OrderBookDetail, orderbook: &Orderbook) -> Result<Ticker> {
    let last_price = Decimal::from_f64(detail.last_trade_price)
        .unwrap_or_else(|| Decimal::from(0));

    let price_change_pct_raw = Decimal::from_f64(detail.daily_price_change)
        .unwrap_or_else(|| Decimal::from(0));

    // Convert percentage to decimal to match other exchanges (e.g., -1.19% -> -0.0119)
    let price_change_pct = price_change_pct_raw / Decimal::from(100);

    let price_change_24h = if last_price > Decimal::ZERO {
        price_change_pct * last_price
    } else {
        Decimal::ZERO
    };

    // Extract best bid/ask from orderbook
    let (best_bid_price, best_bid_qty) = if let Some(best_bid) = orderbook.bids.first() {
        (best_bid.price, best_bid.quantity)
    } else {
        (last_price * Decimal::new(9999, 4), Decimal::ZERO) // Fallback
    };

    let (best_ask_price, best_ask_qty) = if let Some(best_ask) = orderbook.asks.first() {
        (best_ask.price, best_ask.quantity)
    } else {
        (last_price * Decimal::new(10001, 4), Decimal::ZERO) // Fallback
    };

    let open_interest = Decimal::from_f64(detail.open_interest).unwrap();

    Ok(Ticker {
        symbol: detail.symbol.clone(),
        last_price,
        mark_price: last_price, // Use last price as mark price
        index_price: last_price, // Use last price as index price
        best_bid_price,
        best_bid_qty,
        best_ask_price,
        best_ask_qty,
        volume_24h: Decimal::from_f64(detail.daily_base_token_volume)
            .unwrap_or_else(|| Decimal::from(0)),
        turnover_24h: Decimal::from_f64(detail.daily_quote_token_volume)
            .unwrap_or_else(|| Decimal::from(0)),
        open_interest,
        open_interest_notional: open_interest * last_price,
        price_change_24h,
        price_change_pct,
        high_price_24h: Decimal::from_f64(detail.daily_price_high)
            .unwrap_or_else(|| Decimal::from(0)),
        low_price_24h: Decimal::from_f64(detail.daily_price_low)
            .unwrap_or_else(|| Decimal::from(0)),
        timestamp: Utc::now(),
    })
}

/// Convert Lighter orders to core Orderbook
pub fn to_orderbook(
    symbol: &str,
    bids: &[Order],
    asks: &[Order],
) -> Result<Orderbook> {
    let mut bid_levels: Vec<OrderbookLevel> = Vec::new();
    let mut ask_levels: Vec<OrderbookLevel> = Vec::new();

    // Convert bids
    for bid in bids {
        let price = Decimal::from_str(&bid.price)?;
        let quantity = Decimal::from_str(&bid.remaining_base_amount)?;

        bid_levels.push(OrderbookLevel { price, quantity });
    }

    // Convert asks
    for ask in asks {
        let price = Decimal::from_str(&ask.price)?;
        let quantity = Decimal::from_str(&ask.remaining_base_amount)?;

        ask_levels.push(OrderbookLevel { price, quantity });
    }

    // Sort bids descending (highest first)
    bid_levels.sort_by(|a, b| b.price.cmp(&a.price));

    // Sort asks ascending (lowest first)
    ask_levels.sort_by(|a, b| a.price.cmp(&b.price));

    Ok(Orderbook {
        symbol: symbol.to_string(),
        bids: bid_levels,
        asks: ask_levels,
        timestamp: Utc::now(),
    })
}

/// Convert Lighter FundingRate to core FundingRate
pub fn to_funding_rate(fr: &FundingRate) -> Result<CoreFundingRate> {
    Ok(CoreFundingRate {
        symbol: fr.symbol.clone(),
        funding_rate: Decimal::from_f64(fr.rate)
            .unwrap_or_else(|| Decimal::from(0)),
        predicted_rate: Decimal::from_f64(fr.rate)
            .unwrap_or_else(|| Decimal::from(0)),
        funding_time: Utc::now(),
        next_funding_time: Utc::now() + chrono::Duration::hours(8), // Assume 8 hour funding
        funding_interval: 8, // Assume 8 hour funding interval
        funding_rate_cap_floor: Decimal::from_str("0.75").unwrap_or(Decimal::ZERO),
    })
}

/// Convert millisecond timestamp to DateTime<Utc>
fn timestamp_to_datetime(timestamp_ms: i64) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(timestamp_ms)
        .single()
        .unwrap_or_else(Utc::now)
}

/// Convert Lighter Candlestick to core Kline
pub fn to_kline(symbol: &str, interval: &str, cs: &Candlestick) -> Result<Kline> {
    let open_time = timestamp_to_datetime(cs.timestamp);
    // Estimate close time based on interval
    let close_time = estimate_close_time(&open_time, interval);

    Ok(Kline {
        symbol: symbol.to_string(),
        interval: interval.to_string(),
        open_time,
        close_time,
        open: Decimal::from_f64(cs.open)
            .unwrap_or_else(|| Decimal::from(0)),
        high: Decimal::from_f64(cs.high)
            .unwrap_or_else(|| Decimal::from(0)),
        low: Decimal::from_f64(cs.low)
            .unwrap_or_else(|| Decimal::from(0)),
        close: Decimal::from_f64(cs.close)
            .unwrap_or_else(|| Decimal::from(0)),
        volume: Decimal::from_f64(cs.volume0)
            .unwrap_or_else(|| Decimal::from(0)),
        turnover: Decimal::from_f64(cs.volume1)
            .unwrap_or_else(|| Decimal::from(0)),
    })
}

/// Estimate close time based on interval
fn estimate_close_time(open_time: &DateTime<Utc>, interval: &str) -> DateTime<Utc> {
    let duration = match interval {
        "1m" => chrono::Duration::minutes(1),
        "3m" => chrono::Duration::minutes(3),
        "5m" => chrono::Duration::minutes(5),
        "15m" => chrono::Duration::minutes(15),
        "30m" => chrono::Duration::minutes(30),
        "1h" => chrono::Duration::hours(1),
        "2h" => chrono::Duration::hours(2),
        "4h" => chrono::Duration::hours(4),
        "6h" => chrono::Duration::hours(6),
        "8h" => chrono::Duration::hours(8),
        "12h" => chrono::Duration::hours(12),
        "1d" => chrono::Duration::days(1),
        "1w" => chrono::Duration::weeks(1),
        _ => chrono::Duration::hours(1), // Default to 1 hour
    };

    *open_time + duration
}
