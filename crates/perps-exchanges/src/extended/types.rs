use serde::{Deserialize, Serialize};

/// Response wrapper for Extended Exchange API
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExtendedResponse<T> {
    pub status: String, // "OK" or "ERROR"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ExtendedError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pagination: Option<Pagination>,
}

/// Error response structure
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExtendedError {
    pub code: String,
    pub message: String,
}

/// Pagination information
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Pagination {
    pub cursor: Option<i64>,
    pub count: Option<i64>,
}

/// Market information
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtendedMarket {
    pub name: String,
    pub asset_name: String,
    pub asset_precision: i32,
    pub collateral_asset_name: String,
    pub collateral_asset_precision: i32,
    pub active: bool,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub market_stats: Option<ExtendedMarketStats>,
}

/// Market statistics (ticker data)
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtendedMarketStats {
    #[serde(default)]
    pub daily_volume: String,
    #[serde(default)]
    pub daily_volume_base: String,
    #[serde(default)]
    pub daily_price_change: String,
    #[serde(default)]
    pub daily_price_change_percentage: String,
    #[serde(default)]
    pub daily_low: String,
    #[serde(default)]
    pub daily_high: String,
    #[serde(default)]
    pub last_price: String,
    #[serde(default)]
    pub ask_price: String,
    #[serde(default)]
    pub bid_price: String,
    #[serde(default)]
    pub mark_price: String,
    #[serde(default)]
    pub index_price: String,
    #[serde(default)]
    pub funding_rate: String,
    #[serde(default)]
    pub next_funding_rate: i64,
    #[serde(default)]
    pub open_interest: String,
    #[serde(default)]
    pub open_interest_base: String,
}

/// Orderbook snapshot
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExtendedOrderbook {
    pub market: String,
    pub bid: Vec<ExtendedOrderbookLevel>,
    pub ask: Vec<ExtendedOrderbookLevel>,
}

/// Orderbook level (price and quantity)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExtendedOrderbookLevel {
    pub price: String,
    pub qty: String,
}

/// Trade data
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub struct ExtendedTrade {
    pub id: i64,
    #[serde(rename = "m")]
    pub market: String,
    #[serde(rename = "S")]
    pub side: String, // "BUY" or "SELL"
    #[serde(rename = "tT")]
    pub trade_type: String,
    #[serde(rename = "T")]
    pub timestamp: i64, // milliseconds
    #[serde(rename = "p")]
    pub price: String,
    #[serde(rename = "q")]
    pub quantity: String,
}

/// Kline/Candlestick data
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub struct ExtendedKline {
    #[serde(rename = "o")]
    pub open: String,
    #[serde(rename = "h")]
    pub high: String,
    #[serde(rename = "l")]
    pub low: String,
    #[serde(rename = "c")]
    pub close: String,
    #[serde(rename = "v")]
    pub volume: String,
    #[serde(rename = "T")]
    pub timestamp: i64, // milliseconds
}

/// Funding rate data
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExtendedFundingRate {
    #[serde(rename = "m")]
    pub market: String,
    #[serde(rename = "T")]
    pub timestamp: i64, // seconds or milliseconds
    #[serde(rename = "f")]
    pub funding_rate: String,
}

/// Open interest data (extracted from market stats)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExtendedOpenInterest {
    pub open_interest: String,
    pub open_interest_base: String,
}
