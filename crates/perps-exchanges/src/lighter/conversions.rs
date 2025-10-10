use anyhow::Result;
use chrono::Utc;
use perps_core::{FundingRate as CoreFundingRate, Market as CoreMarket, Orderbook, OrderbookLevel, Ticker};
use rust_decimal::Decimal;
use std::str::FromStr;

use super::models::{FundingRate, Order, OrderBook, OrderBookDetail};

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
    let last_price = Decimal::from_f64_retain(detail.last_trade_price)
        .unwrap_or_else(|| Decimal::from(0));

    Ok(Ticker {
        symbol: detail.symbol.clone(),
        last_price,
        mark_price: last_price, // Use last price as mark price
        index_price: last_price, // Use last price as index price
        best_bid_price: last_price * Decimal::new(9999, 4), // Approximate (99.99% of last)
        best_bid_qty: Decimal::ZERO, // Not provided
        best_ask_price: last_price * Decimal::new(10001, 4), // Approximate (100.01% of last)
        best_ask_qty: Decimal::ZERO, // Not provided
        volume_24h: Decimal::from_f64_retain(detail.daily_base_token_volume)
            .unwrap_or_else(|| Decimal::from(0)),
        turnover_24h: Decimal::from_f64_retain(detail.daily_quote_token_volume)
            .unwrap_or_else(|| Decimal::from(0)),
        price_change_24h: Decimal::from_f64_retain(detail.daily_price_change)
            .unwrap_or_else(|| Decimal::from(0)),
        price_change_pct: Decimal::from_f64_retain(detail.daily_price_change / 100.0)
            .unwrap_or_else(|| Decimal::from(0)),
        high_price_24h: Decimal::from_f64_retain(detail.daily_price_high)
            .unwrap_or_else(|| Decimal::from(0)),
        low_price_24h: Decimal::from_f64_retain(detail.daily_price_low)
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
        funding_rate: Decimal::from_f64_retain(fr.rate)
            .unwrap_or_else(|| Decimal::from(0)),
        predicted_rate: Decimal::from_f64_retain(fr.rate)
            .unwrap_or_else(|| Decimal::from(0)),
        funding_time: Utc::now(),
        next_funding_time: Utc::now() + chrono::Duration::hours(8), // Assume 8 hour funding
        funding_interval: 8, // Assume 8 hour funding interval
        funding_rate_cap_floor: Decimal::from_str("0.75").unwrap_or(Decimal::ZERO),
    })
}
