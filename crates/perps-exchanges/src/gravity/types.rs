use serde::{Deserialize, Serialize};

/// Gravity API response wrapper structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GravityApiResponse<T> {
    pub result: Option<T>,
    pub code: Option<i32>,
    pub message: Option<String>,
}

/// Gravity instrument metadata from `/full/v1/all_instruments`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GravityInstrument {
    pub instrument: String,
    pub instrument_hash: Option<String>,
    pub base: String,
    pub quote: String,
    pub kind: String,
    pub venues: Option<Vec<String>>,
    pub settlement_period: String,
    pub base_decimals: Option<i32>,
    pub quote_decimals: Option<i32>,
    pub tick_size: Option<String>,
    pub min_size: String,
    #[serde(default)]
    pub create_time: Option<String>, // API returns as string (Unix nanoseconds)
    pub max_position_size: Option<String>,
    #[serde(default)]
    pub funding_interval_hours: Option<i32>,
    #[serde(default)]
    pub adjusted_funding_rate_cap: Option<String>,
    #[serde(default)]
    pub adjusted_funding_rate_floor: Option<String>,
}

/// Gravity ticker response from `/full/v1/ticker`
/// All prices are in 9 decimals (divide by 1_000_000_000 for actual value)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GravityTicker {
    pub instrument: String,
    #[serde(rename = "event_time")]
    pub event_time: String, // Unix nanoseconds as string
    pub mark_price: String,  // 9 decimals
    pub index_price: String, // 9 decimals
    pub last_price: String,  // 9 decimals
    pub last_size: Option<String>,
    pub mid_price: String,      // 9 decimals
    pub best_bid_price: String, // 9 decimals
    pub best_bid_size: String,
    pub best_ask_price: String, // 9 decimals
    pub best_ask_size: String,
    #[serde(default)]
    pub funding_rate_8h_curr: Option<String>,
    #[serde(default)]
    pub funding_rate_8h_avg: Option<String>,
    pub buy_volume_24h_b: Option<String>, // Base asset
    pub sell_volume_24h_b: Option<String>,
    pub buy_volume_24h_q: Option<String>, // Quote asset
    pub sell_volume_24h_q: Option<String>,
    pub high_price: Option<String>, // 9 decimals
    pub low_price: Option<String>,  // 9 decimals
    pub open_price: Option<String>, // 9 decimals
    pub open_interest: Option<String>,
}

/// Single orderbook level (price, size)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GravityOrderbookLevel {
    pub price: String, // 9 decimals
    pub size: String,
    #[serde(default)]
    pub num_orders: Option<u32>,
}

/// Gravity orderbook response from `/full/v1/book`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GravityOrderbook {
    pub instrument: String,
    #[serde(rename = "event_time")]
    pub event_time: String, // Unix nanoseconds as string
    pub bids: Vec<GravityOrderbookLevel>,
    pub asks: Vec<GravityOrderbookLevel>,
}

/// Gravity funding rate response from `/full/v1/funding`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GravityFundingRate {
    pub instrument: String,
    pub funding_rate: String, // Percentage points (e.g., "0.0001" = 0.01%)
    pub funding_time: String, // Unix nanoseconds as string
    pub mark_price: String,   // 9 decimals
    pub funding_rate_8_h_avg: Option<String>,
}

/// Gravity kline/candlestick response from `/full/v1/kline`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GravityKline {
    pub open_time: String,   // Unix nanoseconds as string
    pub close_time: String,  // Unix nanoseconds as string
    pub open: String,        // 9 decimals
    pub close: String,       // 9 decimals
    pub high: String,        // 9 decimals
    pub low: String,         // 9 decimals
    pub volume_b: String,    // Base asset volume
    pub volume_q: String,    // Quote asset volume
    pub trades: Option<u32>, // Number of trades in this candle
    pub instrument: String,
}

/// Gravity trade response from `/full/v1/trade` or `/full/v1/trade_history`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GravityTrade {
    pub trade_id: String,
    pub instrument: String,
    pub is_taker_buyer: bool,
    pub size: String,
    pub price: String,       // 9 decimals
    pub mark_price: String,  // 9 decimals
    pub index_price: String, // 9 decimals
    pub interest_rate: Option<String>,
    pub forward_price: Option<String>, // 9 decimals
    pub venue: Option<String>,         // "ORDERBOOK" or "RFQ"
    pub is_rpi: Option<bool>,          // Retail Price Improvement
    #[serde(rename = "event_time")]
    pub event_time: String, // Unix nanoseconds as string
}

/// Gravity open interest response from market data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GravityOpenInterest {
    pub instrument: String,
    pub open_interest: String,
    pub timestamp: i64,
    #[serde(skip)]
    pub mark_price: Option<String>, // Optional mark price for calculating notional value
}
