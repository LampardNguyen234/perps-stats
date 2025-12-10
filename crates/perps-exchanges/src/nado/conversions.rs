use anyhow::{Context, Result};
use chrono::{TimeZone, Utc};
use perps_core::{
    FundingRate, Market, MarketStats, OpenInterest, Orderbook, OrderbookLevel, Ticker,
};
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;

use super::types::{ContractData, OrderbookResponse, Pair, TickerData};

/// Convert Nado Pair to core Market
pub fn pair_to_market(pair: &Pair) -> Result<Market> {
    Ok(Market {
        symbol: pair.ticker_id.clone(),
        contract: format!("{}-{}", pair.base, pair.quote),
        contract_size: Decimal::from(1), // 1:1 for Nado perps
        price_scale: -2,                 // Default 2 decimals (can be refined later)
        quantity_scale: -3,              // Default 3 decimals (can be refined later)
        min_order_qty: Decimal::from_str_exact("0.001")?,
        max_order_qty: Decimal::from(1000000), // Default max
        min_order_value: Decimal::from(10),    // Default $10 min
        max_leverage: Decimal::from(20),       // Default max leverage
    })
}

/// Merge Nado TickerData, ContractData, and best bid/ask to create a complete Ticker
pub fn merge_ticker_contract_and_orderbook(
    ticker: &TickerData, 
    contract: &ContractData,
    best_bid: Option<&OrderbookLevel>,
    best_ask: Option<&OrderbookLevel>,
) -> Result<Ticker> {
    let last_price = Decimal::from_f64(ticker.last_price).context("Failed to parse last_price")?;

    let mark_price =
        Decimal::from_f64(contract.mark_price).context("Failed to parse mark_price")?;

    let index_price =
        Decimal::from_f64(contract.index_price).context("Failed to parse index_price")?;

    let volume_24h = Decimal::from_f64(ticker.base_volume).unwrap_or(Decimal::ZERO);

    let turnover_24h = Decimal::from_f64(ticker.quote_volume).unwrap_or(Decimal::ZERO);

    // Nado API returns percentage as number (e.g., 2.32 for 2.32%)
    // Convert to decimal form (e.g., 2.32 -> 0.0232)
    let price_change_pct =
        Decimal::from_f64(ticker.price_change_percent_24h).unwrap_or(Decimal::ZERO) / Decimal::from(100);

    // Calculate price change in absolute terms
    let price_change_24h = if last_price > Decimal::ZERO && price_change_pct != Decimal::ZERO {
        last_price * price_change_pct
    } else {
        Decimal::ZERO
    };

    let open_interest = Decimal::from_f64(contract.open_interest).unwrap_or(Decimal::ZERO);

    let open_interest_notional =
        Decimal::from_f64(contract.open_interest_usd).unwrap_or(Decimal::ZERO);

    // Extract best bid/ask from orderbook
    let (best_bid_price, best_bid_qty) = if let Some(bid) = best_bid {
        (bid.price, bid.quantity)
    } else {
        (Decimal::ZERO, Decimal::ZERO)
    };

    let (best_ask_price, best_ask_qty) = if let Some(ask) = best_ask {
        (ask.price, ask.quantity)
    } else {
        (Decimal::ZERO, Decimal::ZERO)
    };

    Ok(Ticker {
        symbol: ticker.ticker_id.clone(),
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
        high_price_24h: Decimal::ZERO, // Not available from Nado API
        low_price_24h: Decimal::ZERO,  // Not available from Nado API
        timestamp: Utc::now(),
    })
}

/// Merge Nado TickerData and ContractData to create a complete Ticker (legacy version without orderbook)
pub fn merge_ticker_and_contract(ticker: &TickerData, contract: &ContractData) -> Result<Ticker> {
    let last_price = Decimal::from_f64(ticker.last_price).context("Failed to parse last_price")?;

    let mark_price =
        Decimal::from_f64(contract.mark_price).context("Failed to parse mark_price")?;

    let index_price =
        Decimal::from_f64(contract.index_price).context("Failed to parse index_price")?;

    let volume_24h = Decimal::from_f64(ticker.base_volume).unwrap_or(Decimal::ZERO);

    let turnover_24h = Decimal::from_f64(ticker.quote_volume).unwrap_or(Decimal::ZERO);

    // Nado API returns percentage as number (e.g., 2.32 for 2.32%)
    // Convert to decimal form (e.g., 2.32 -> 0.0232)
    let price_change_pct =
        Decimal::from_f64(ticker.price_change_percent_24h).unwrap_or(Decimal::ZERO) / Decimal::from(100);

    // Calculate price change in absolute terms
    let price_change_24h = if last_price > Decimal::ZERO && price_change_pct != Decimal::ZERO {
        last_price * price_change_pct
    } else {
        Decimal::ZERO
    };

    let open_interest = Decimal::from_f64(contract.open_interest).unwrap_or(Decimal::ZERO);

    let open_interest_notional =
        Decimal::from_f64(contract.open_interest_usd).unwrap_or(Decimal::ZERO);

    Ok(Ticker {
        symbol: ticker.ticker_id.clone(),
        last_price,
        mark_price,
        index_price,
        // For MVP, we'll set best_bid/ask to ZERO to avoid extra API call
        // These can be populated via orderbook if needed
        best_bid_price: Decimal::ZERO,
        best_bid_qty: Decimal::ZERO,
        best_ask_price: Decimal::ZERO,
        best_ask_qty: Decimal::ZERO,
        volume_24h,
        turnover_24h,
        open_interest,
        open_interest_notional,
        price_change_24h,
        price_change_pct,
        high_price_24h: Decimal::ZERO, // Not available from Nado API
        low_price_24h: Decimal::ZERO,  // Not available from Nado API
        timestamp: Utc::now(),
    })
}

/// Convert Nado OrderbookResponse to core Orderbook
pub fn orderbook_response_to_orderbook(response: &OrderbookResponse) -> Result<Orderbook> {
    let mut bid_levels = Vec::new();
    let mut ask_levels = Vec::new();

    // Convert bids [[price, quantity], ...]
    for bid in &response.bids {
        let price = Decimal::from_f64(bid[0]).context("Failed to parse bid price")?;
        let quantity = Decimal::from_f64(bid[1]).context("Failed to parse bid quantity")?;

        bid_levels.push(OrderbookLevel { price, quantity });
    }

    // Convert asks [[price, quantity], ...]
    for ask in &response.asks {
        let price = Decimal::from_f64(ask[0]).context("Failed to parse ask price")?;
        let quantity = Decimal::from_f64(ask[1]).context("Failed to parse ask quantity")?;

        ask_levels.push(OrderbookLevel { price, quantity });
    }

    // Ensure bids are sorted descending (highest first)
    bid_levels.sort_by(|a, b| b.price.cmp(&a.price));

    // Ensure asks are sorted ascending (lowest first)
    ask_levels.sort_by(|a, b| a.price.cmp(&b.price));

    // Convert timestamp (milliseconds to DateTime)
    let timestamp = Utc
        .timestamp_millis_opt(response.timestamp)
        .single()
        .unwrap_or_else(Utc::now);

    Ok(Orderbook {
        symbol: response.ticker_id.clone(),
        bids: bid_levels,
        asks: ask_levels,
        timestamp,
    })
}

/// Convert ContractData to FundingRate
pub fn contract_to_funding_rate(contract: &ContractData) -> Result<FundingRate> {
    let funding_rate =
        Decimal::from_f64(contract.funding_rate).context("Failed to parse funding_rate")?;

    // Note: Nado's funding_rate appears to be a 24h rate
    // Divide by 24 to get hourly rate
    let hourly_funding_rate = funding_rate / Decimal::from(24);

    let next_funding_time = Utc
        .timestamp_opt(contract.next_funding_rate_timestamp, 0)
        .single()
        .unwrap_or_else(Utc::now);

    Ok(FundingRate {
        symbol: contract.ticker_id.clone(),
        funding_rate: hourly_funding_rate,
        predicted_rate: hourly_funding_rate, // Use same as current
        funding_time: Utc::now(),
        next_funding_time,
        funding_interval: 1, // 1 hour (assuming standard)
        funding_rate_cap_floor: Decimal::from_str_exact("0.75").unwrap_or(Decimal::ZERO),
    })
}

/// Convert ContractData to OpenInterest
pub fn contract_to_open_interest(contract: &ContractData) -> Result<OpenInterest> {
    let open_interest =
        Decimal::from_f64(contract.open_interest).context("Failed to parse open_interest")?;

    let open_value = Decimal::from_f64(contract.open_interest_usd)
        .context("Failed to parse open_interest_usd")?;

    Ok(OpenInterest {
        symbol: contract.ticker_id.clone(),
        open_interest,
        open_value,
        timestamp: Utc::now(),
    })
}

/// Convert ContractData to MarketStats
pub fn contract_to_market_stats(contract: &ContractData) -> Result<MarketStats> {
    let last_price =
        Decimal::from_f64(contract.last_price).context("Failed to parse last_price")?;

    let mark_price =
        Decimal::from_f64(contract.mark_price).context("Failed to parse mark_price")?;

    let index_price =
        Decimal::from_f64(contract.index_price).context("Failed to parse index_price")?;

    let volume_24h = Decimal::from_f64(contract.base_volume).unwrap_or(Decimal::ZERO);

    let turnover_24h = Decimal::from_f64(contract.quote_volume).unwrap_or(Decimal::ZERO);

    // Nado API returns percentage as number (e.g., 2.32 for 2.32%)
    // Convert to decimal form (e.g., 2.32 -> 0.0232)
    let price_change_pct =
        Decimal::from_f64(contract.price_change_percent_24h).unwrap_or(Decimal::ZERO) / Decimal::from(100);

    let price_change_24h = if last_price > Decimal::ZERO && price_change_pct != Decimal::ZERO {
        last_price * price_change_pct
    } else {
        Decimal::ZERO
    };

    let open_interest = Decimal::from_f64(contract.open_interest).unwrap_or(Decimal::ZERO);

    let funding_rate =
        Decimal::from_f64(contract.funding_rate).unwrap_or(Decimal::ZERO) / Decimal::from(24); // Convert to hourly

    Ok(MarketStats {
        symbol: contract.ticker_id.clone(),
        last_price,
        mark_price,
        index_price,
        volume_24h,
        turnover_24h,
        price_change_24h,
        price_change_pct,
        high_price_24h: Decimal::ZERO, // Not available from Nado
        low_price_24h: Decimal::ZERO,  // Not available from Nado
        open_interest,
        funding_rate,
        timestamp: Utc::now(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pair_to_market() {
        let pair = Pair {
            product_id: 1,
            ticker_id: "BTC-PERP_USDT0".to_string(),
            base: "BTC-PERP".to_string(),
            quote: "USDT0".to_string(),
        };

        let market = pair_to_market(&pair).unwrap();
        assert_eq!(market.symbol, "BTC-PERP_USDT0");
        assert_eq!(market.contract, "BTC-PERP-USDT0");
    }

    #[test]
    fn test_orderbook_conversion() {
        let response = OrderbookResponse {
            product_id: 1,
            ticker_id: "BTC-PERP_USDT0".to_string(),
            bids: vec![[50000.0, 1.5], [49999.0, 2.0]],
            asks: vec![[50001.0, 1.0], [50002.0, 1.5]],
            timestamp: 1694379600000,
        };

        let orderbook = orderbook_response_to_orderbook(&response).unwrap();
        assert_eq!(orderbook.symbol, "BTC-PERP_USDT0");
        assert_eq!(orderbook.bids.len(), 2);
        assert_eq!(orderbook.asks.len(), 2);

        // Check sorting
        assert!(orderbook.bids[0].price > orderbook.bids[1].price); // Descending
        assert!(orderbook.asks[0].price < orderbook.asks[1].price); // Ascending
    }

    #[test]
    fn test_funding_rate_conversion() {
        let contract = ContractData {
            product_id: 1,
            ticker_id: "BTC-PERP_USDT0".to_string(),
            base_currency: "BTC".to_string(),
            quote_currency: "USDT0".to_string(),
            last_price: 50000.0,
            base_volume: 100.0,
            quote_volume: 5000000.0,
            product_type: "perp".to_string(),
            contract_price: 50000.0,
            contract_price_currency: "USD".to_string(),
            open_interest: 1000.0,
            open_interest_usd: 50000000.0,
            index_price: 50010.0,
            mark_price: 50005.0,
            funding_rate: 0.0024, // 24h rate
            next_funding_rate_timestamp: 1694379600,
            price_change_percent_24h: 2.5,
        };

        let funding_rate = contract_to_funding_rate(&contract).unwrap();
        assert_eq!(funding_rate.symbol, "BTC-PERP_USDT0");
        // Should be divided by 24 to get hourly rate
        assert_eq!(
            funding_rate.funding_rate,
            Decimal::from_f64(0.0024 / 24.0).unwrap()
        );
    }
}
