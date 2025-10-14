use serde::{Deserialize, Serialize};

/// Response wrapper for all Pacifica API responses
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PacificaResponse<T> {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

/// Market information from /api/v1/info endpoint
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PacificaMarket {
    pub symbol: String,
    pub tick_size: String,
    pub min_tick: String,
    pub max_tick: String,
    pub lot_size: String,
    pub max_leverage: i32,
    pub isolated_only: bool,
    pub min_order_size: String,
    pub max_order_size: String,
    pub funding_rate: String,
    pub next_funding_rate: String,
}

/// Price data from /api/v1/info/prices endpoint
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PacificaPrice {
    pub funding: String,
    pub mark: String,
    pub mid: String,
    pub next_funding: String,
    pub open_interest: String,
    pub oracle: String,
    pub symbol: String,
    pub timestamp: i64,
    pub volume_24h: String,
    pub yesterday_price: String,
}

/// Orderbook data from /api/v1/book endpoint
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PacificaOrderbookData {
    /// Symbol
    pub s: String,
    /// Levels - nested array: [[bids...], [asks...]]
    /// Index 0 = bids, Index 1 = asks
    pub l: Vec<Vec<PacificaOrderbookLevel>>,
    /// Timestamp in milliseconds
    pub t: i64,
}

/// Individual orderbook level
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PacificaOrderbookLevel {
    /// Price
    pub p: String,
    /// Amount/quantity
    pub a: String,
    /// Number of orders
    pub n: i32,
}

/// Trade data from /api/v1/trades endpoint
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PacificaTrade {
    pub event_type: String, // "fulfill_taker" or "fulfill_maker"
    pub price: String,
    pub amount: String,
    pub side: String, // "open_long", "open_short", "close_long", "close_short"
    pub cause: String, // "normal", "market_liquidation", etc.
    pub created_at: i64, // milliseconds
}

/// Kline/candlestick data from /api/v1/kline endpoint
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PacificaKline {
    pub t: i64,
    pub o: String,
    pub h: String,
    pub l: String,
    pub c: String,
    pub v: String,
}

/// Funding rate history from /api/v1/funding_rate/history endpoint
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PacificaFundingRate {
    pub symbol: String,
    pub funding_rate: String,
    pub timestamp: i64,
}
