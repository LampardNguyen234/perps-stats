use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OrderSide {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Market {
    pub symbol: String,
    pub contract: String,
    pub contract_size: Decimal,
    pub price_scale: i32,
    pub quantity_scale: i32,
    pub min_order_qty: Decimal,
    pub max_order_qty: Decimal,
    pub min_order_value: Decimal,
    pub max_leverage: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticker {
    pub symbol: String,
    pub last_price: Decimal,
    pub mark_price: Decimal,
    pub index_price: Decimal,
    pub best_bid_price: Decimal,
    pub best_bid_qty: Decimal,
    pub best_ask_price: Decimal,
    pub best_ask_qty: Decimal,
    pub volume_24h: Decimal,
    pub turnover_24h: Decimal,
    pub price_change_24h: Decimal,
    pub price_change_pct: Decimal,
    pub high_price_24h: Decimal,
    pub low_price_24h: Decimal,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrderbookLevel {
    pub price: Decimal,
    pub quantity: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Orderbook {
    pub symbol: String,
    pub bids: Vec<OrderbookLevel>,
    pub asks: Vec<OrderbookLevel>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FundingRate {
    pub symbol: String,
    pub funding_rate: Decimal,
    pub predicted_rate: Decimal,
    pub funding_time: DateTime<Utc>,
    pub next_funding_time: DateTime<Utc>,
    pub funding_interval: i32, // in hours
    pub funding_rate_cap_floor: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenInterest {
    pub symbol: String,
    pub open_interest: Decimal,
    pub open_value: Decimal,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Kline {
    pub symbol: String,
    pub interval: String,
    pub open_time: DateTime<Utc>,
    pub close_time: DateTime<Utc>,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    pub volume: Decimal,
    pub turnover: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trade {
    pub id: String,
    pub symbol: String,
    pub price: Decimal,
    pub quantity: Decimal,
    pub side: OrderSide,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketStats {
    pub symbol: String,
    pub volume_24h: Decimal,
    pub turnover_24h: Decimal,
    pub open_interest: Decimal,
    pub funding_rate: Decimal,
    pub last_price: Decimal,
    pub mark_price: Decimal,
    pub index_price: Decimal,
    pub price_change_24h: Decimal,
    pub price_change_pct: Decimal,
    pub high_price_24h: Decimal,
    pub low_price_24h: Decimal,
    pub timestamp: DateTime<Utc>,
}

/// Represents total notional (price × qty) available within fixed spread
/// thresholds (in basis points) for both bid and ask sides.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct LiquidityDepthStats {
    pub timestamp: DateTime<Utc>,
    pub exchange: String,
    pub symbol: String,
    pub mid_price: Decimal,

    // Bid notionals
    pub bid_1bps: Decimal,
    pub bid_2_5bps: Decimal,
    pub bid_5bps: Decimal,
    pub bid_10bps: Decimal,
    pub bid_20bps: Decimal,

    // Ask notionals
    pub ask_1bps: Decimal,
    pub ask_2_5bps: Decimal,
    pub ask_5bps: Decimal,
    pub ask_10bps: Decimal,
    pub ask_20bps: Decimal,
}
