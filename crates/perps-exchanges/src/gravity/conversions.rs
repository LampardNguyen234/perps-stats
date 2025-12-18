use super::types::*;
use anyhow::{anyhow, Result};
use perps_core::*;
use rust_decimal::Decimal;
use std::str::FromStr;

/// Convert nanoseconds (as string) to Unix seconds timestamp
fn nanos_to_unix_seconds(nanos_str: &str) -> Result<i64> {
    let nanos = i64::from_str(nanos_str)
        .map_err(|_| anyhow!("Failed to parse nanoseconds: {}", nanos_str))?;
    Ok(nanos / 1_000_000_000)
}

/// Parse Gravity 9-decimal price string to Decimal
fn parse_gravity_price(price_str: &str) -> Result<Decimal> {
    // Prices are in 9 decimals, parse as is (already in the right scale)
    Decimal::from_str(price_str).map_err(|_| anyhow!("Failed to parse price: {}", price_str))
}

/// Convert GravityTicker to perps_core::Ticker
///
/// Maps Gravity's 9-decimal prices and nanosecond timestamps to Decimal prices
/// and Unix second timestamps in the perps_core format.
pub fn gravity_ticker_to_ticker(gravity: GravityTicker, symbol: String) -> Result<Ticker> {
    let timestamp = nanos_to_unix_seconds(&gravity.event_time)?;

    // Parse all prices (9 decimals)
    let last_price = parse_gravity_price(&gravity.last_price)?;
    let mark_price = parse_gravity_price(&gravity.mark_price)?;
    let index_price = parse_gravity_price(&gravity.index_price)?;
    let best_bid_price = parse_gravity_price(&gravity.best_bid_price)?;
    let best_ask_price = parse_gravity_price(&gravity.best_ask_price)?;
    let best_bid_qty = Decimal::from_str(&gravity.best_bid_size)
        .map_err(|_| anyhow!("Failed to parse best bid size"))?;
    let best_ask_qty = Decimal::from_str(&gravity.best_ask_size)
        .map_err(|_| anyhow!("Failed to parse best ask size"))?;

    // Calculate 24h volume (sum of buy and sell)
    let volume_24h = {
        let buy = gravity
            .buy_volume_24h_b
            .as_ref()
            .and_then(|s| Decimal::from_str(s).ok())
            .unwrap_or(Decimal::ZERO);
        let sell = gravity
            .sell_volume_24h_b
            .as_ref()
            .and_then(|s| Decimal::from_str(s).ok())
            .unwrap_or(Decimal::ZERO);
        buy + sell
    };

    // Calculate 24h turnover (quote volume)
    let turnover_24h = {
        let buy = gravity
            .buy_volume_24h_q
            .as_ref()
            .and_then(|s| Decimal::from_str(s).ok())
            .unwrap_or(Decimal::ZERO);
        let sell = gravity
            .sell_volume_24h_q
            .as_ref()
            .and_then(|s| Decimal::from_str(s).ok())
            .unwrap_or(Decimal::ZERO);
        buy + sell
    };

    // Open interest from ticker
    let open_interest = gravity
        .open_interest
        .as_ref()
        .and_then(|s| Decimal::from_str(s).ok())
        .unwrap_or(Decimal::ZERO);

    // 24h high and low
    let high_price_24h = gravity
        .high_price
        .as_ref()
        .and_then(|s| parse_gravity_price(s).ok())
        .unwrap_or(Decimal::ZERO);
    let low_price_24h = gravity
        .low_price
        .as_ref()
        .and_then(|s| parse_gravity_price(s).ok())
        .unwrap_or(Decimal::ZERO);

    // Calculate price change from open/close
    // Note: price_change_pct is stored as decimal (e.g., -0.0119 for -1.19%), not percentage (e.g., -1.19)
    let (price_change_24h, price_change_pct) = {
        if let Some(open_str) = &gravity.open_price {
            if let Ok(open) = parse_gravity_price(open_str) {
                let change = last_price - open;
                let pct = if open > Decimal::ZERO {
                    change / open
                } else {
                    Decimal::ZERO
                };
                (change, pct)
            } else {
                (Decimal::ZERO, Decimal::ZERO)
            }
        } else {
            (Decimal::ZERO, Decimal::ZERO)
        }
    };

    // Calculate open interest notional (OI quantity × mark price)
    let open_interest_notional = open_interest * mark_price;

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
        price_change_24h,
        price_change_pct,
        high_price_24h,
        low_price_24h,
        timestamp: chrono::DateTime::<chrono::Utc>::from_timestamp(timestamp, 0)
            .ok_or_else(|| anyhow!("Invalid timestamp"))?,
    })
}

/// Convert GravityInstrument to perps_core::Market
///
/// Maps Gravity instrument metadata to perps_core Market structure.
pub fn gravity_market_to_market(gravity: GravityInstrument, symbol: String) -> Result<Market> {
    // Parse minimum order quantity
    let min_order_qty =
        Decimal::from_str(&gravity.min_size).map_err(|_| anyhow!("Failed to parse min_size"))?;

    // Max position size if available
    let max_order_qty = gravity
        .max_position_size
        .as_ref()
        .and_then(|s| Decimal::from_str(s).ok())
        .unwrap_or(Decimal::new(1000000, 0)); // Default high value

    // Determine price and quantity scales from decimals
    let price_scale = gravity.quote_decimals.unwrap_or(6);
    let quantity_scale = gravity.base_decimals.unwrap_or(8);

    // For Perpetuals, min_order_value is typically min notional value
    let min_order_value = min_order_qty * Decimal::from(100); // Assume ~100 as min notional

    Ok(Market {
        symbol,
        contract: "PERPETUAL".to_string(),
        contract_size: Decimal::ONE,
        price_scale,
        quantity_scale,
        min_order_qty,
        max_order_qty,
        min_order_value,
        max_leverage: Decimal::new(1000, 0), // Conservative default; Gravity may vary
    })
}

/// Convert GravityOrderbook to perps_core::Orderbook
///
/// Maps Gravity orderbook levels with 9-decimal prices to perps_core format.
pub fn gravity_orderbook_to_orderbook(
    gravity: GravityOrderbook,
    symbol: String,
) -> Result<Orderbook> {
    let timestamp = nanos_to_unix_seconds(&gravity.event_time)?;

    // Convert bid levels
    let bids = gravity
        .bids
        .into_iter()
        .map(|level| {
            Ok(OrderbookLevel {
                price: parse_gravity_price(&level.price)?,
                quantity: Decimal::from_str(&level.size)
                    .map_err(|_| anyhow!("Failed to parse bid size"))?,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    // Convert ask levels
    let asks = gravity
        .asks
        .into_iter()
        .map(|level| {
            Ok(OrderbookLevel {
                price: parse_gravity_price(&level.price)?,
                quantity: Decimal::from_str(&level.size)
                    .map_err(|_| anyhow!("Failed to parse ask size"))?,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(Orderbook {
        symbol,
        bids,
        asks,
        timestamp: chrono::DateTime::<chrono::Utc>::from_timestamp(timestamp, 0)
            .ok_or_else(|| anyhow!("Invalid timestamp"))?,
    })
}

/// Convert GravityFundingRate to perps_core::FundingRate
///
/// Phase 3: Maps funding rate data (implemented here for completeness)
pub fn gravity_funding_rate_to_funding_rate(
    gravity: GravityFundingRate,
    symbol: String,
) -> Result<FundingRate> {
    let funding_time = nanos_to_unix_seconds(&gravity.funding_time)?;

    let funding_rate = Decimal::from_str(&gravity.funding_rate)
        .map_err(|_| anyhow!("Failed to parse funding rate"))?;

    let predicted_rate = gravity
        .funding_rate_8_h_avg
        .as_ref()
        .and_then(|s| Decimal::from_str(s).ok())
        .unwrap_or(Decimal::ZERO);

    Ok(FundingRate {
        symbol,
        funding_rate,
        predicted_rate,
        funding_time: chrono::DateTime::<chrono::Utc>::from_timestamp(funding_time, 0)
            .ok_or_else(|| anyhow!("Invalid funding_time"))?,
        next_funding_time: chrono::DateTime::<chrono::Utc>::from_timestamp(
            funding_time + 8 * 3600,
            0,
        )
        .ok_or_else(|| anyhow!("Invalid next_funding_time"))?, // 8 hours later
        funding_interval: 8,                   // Gravity uses 8-hour intervals
        funding_rate_cap_floor: Decimal::ZERO, // Not available from API
    })
}

/// Convert GravityKline to perps_core::Kline
///
/// Phase 3: Maps candlestick data (implemented for completeness)
pub fn gravity_kline_to_kline(gravity: GravityKline, symbol: String) -> Result<Kline> {
    let open_time_sec = nanos_to_unix_seconds(&gravity.open_time)?;
    let close_time_sec = nanos_to_unix_seconds(&gravity.close_time)?;

    let volume = Decimal::from_str(&gravity.volume_b)
        .map_err(|_| anyhow!("Failed to parse kline volume"))?;
    let turnover = Decimal::from_str(&gravity.volume_q)
        .map_err(|_| anyhow!("Failed to parse kline turnover"))?;

    Ok(Kline {
        symbol,
        interval: "1m".to_string(), // Default interval; should be passed as parameter
        open_time: chrono::DateTime::<chrono::Utc>::from_timestamp(open_time_sec, 0)
            .ok_or_else(|| anyhow!("Invalid open_time"))?,
        close_time: chrono::DateTime::<chrono::Utc>::from_timestamp(close_time_sec, 0)
            .ok_or_else(|| anyhow!("Invalid close_time"))?,
        open: parse_gravity_price(&gravity.open)?,
        high: parse_gravity_price(&gravity.high)?,
        low: parse_gravity_price(&gravity.low)?,
        close: parse_gravity_price(&gravity.close)?,
        volume,
        turnover,
    })
}

/// Convert GravityTrade to perps_core::Trade
///
/// Phase 3: Maps trade data (implemented for completeness)
pub fn gravity_trade_to_trade(gravity: GravityTrade, symbol: String) -> Result<Trade> {
    let timestamp = nanos_to_unix_seconds(&gravity.event_time)?;

    let side = if gravity.is_taker_buyer {
        OrderSide::Buy
    } else {
        OrderSide::Sell
    };

    Ok(Trade {
        id: gravity.trade_id,
        symbol,
        price: parse_gravity_price(&gravity.price)?,
        quantity: Decimal::from_str(&gravity.size)
            .map_err(|_| anyhow!("Failed to parse trade size"))?,
        side,
        timestamp: chrono::DateTime::<chrono::Utc>::from_timestamp(timestamp, 0)
            .ok_or_else(|| anyhow!("Invalid timestamp"))?,
    })
}

/// Convert GravityOpenInterest to perps_core::OpenInterest
///
/// Calculates open_value as open_interest * mark_price (notional value)
pub fn gravity_open_interest_to_open_interest(
    gravity: GravityOpenInterest,
    symbol: String,
) -> Result<OpenInterest> {
    let open_interest = Decimal::from_str(&gravity.open_interest)
        .map_err(|_| anyhow!("Failed to parse open interest"))?;

    // Calculate open value (notional) if mark price is available
    let open_value = if let Some(mark_price_str) = gravity.mark_price {
        let mark_price = Decimal::from_str(&mark_price_str)
            .map_err(|_| anyhow!("Failed to parse mark price"))?;
        open_interest * mark_price
    } else {
        Decimal::ZERO
    };

    Ok(OpenInterest {
        symbol,
        open_interest,
        open_value,
        timestamp: chrono::DateTime::<chrono::Utc>::from_timestamp(gravity.timestamp, 0)
            .ok_or_else(|| anyhow!("Invalid timestamp"))?,
    })
}
