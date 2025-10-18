use serde::{Deserialize, Serialize};

/// Aster WebSocket ticker stream message (Binance-compatible)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AsterWsTicker {
    #[serde(rename = "e")]
    pub event_type: String, // "24hrTicker"
    #[serde(rename = "E")]
    pub event_time: i64,
    #[serde(rename = "s")]
    pub symbol: String,
    #[serde(rename = "p")]
    pub price_change: String,
    #[serde(rename = "P")]
    pub price_change_percent: String,
    #[serde(rename = "w")]
    pub weighted_avg_price: String,
    #[serde(rename = "c")]
    pub close_price: String, // Last price
    #[serde(rename = "Q")]
    pub last_quantity: String,
    #[serde(rename = "o")]
    pub open_price: String,
    #[serde(rename = "h")]
    pub high_price: String,
    #[serde(rename = "l")]
    pub low_price: String,
    #[serde(rename = "v")]
    pub volume: String, // Total traded base asset volume
    #[serde(rename = "q")]
    pub quote_volume: String, // Total traded quote asset volume
    #[serde(rename = "O")]
    pub open_time: i64, // Statistics open time
    #[serde(rename = "C")]
    pub close_time: i64, // Statistics close time
    #[serde(rename = "F")]
    pub first_trade_id: i64,
    #[serde(rename = "L")]
    pub last_trade_id: i64,
    #[serde(rename = "n")]
    pub trade_count: i64, // Total number of trades
}

/// Aster WebSocket trade stream message (Binance-compatible)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AsterWsTrade {
    #[serde(rename = "e")]
    pub event_type: String, // "aggTrade"
    #[serde(rename = "E")]
    pub event_time: i64,
    #[serde(rename = "s")]
    pub symbol: String,
    #[serde(rename = "a")]
    pub aggregate_trade_id: i64,
    #[serde(rename = "p")]
    pub price: String,
    #[serde(rename = "q")]
    pub quantity: String,
    #[serde(rename = "f")]
    pub first_trade_id: i64,
    #[serde(rename = "l")]
    pub last_trade_id: i64,
    #[serde(rename = "T")]
    pub trade_time: i64,
    #[serde(rename = "m")]
    pub is_buyer_maker: bool,
}

/// Aster WebSocket orderbook update message (Binance-compatible)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AsterWsDepthUpdate {
    #[serde(rename = "e")]
    pub event_type: String, // "depthUpdate"
    #[serde(rename = "E")]
    pub event_time: i64,
    #[serde(rename = "s")]
    pub symbol: String,
    #[serde(rename = "U")]
    pub first_update_id: i64,
    #[serde(rename = "u")]
    pub final_update_id: i64,
    #[serde(rename = "pu")]
    pub previous_update_id: i64, // Previous update ID for continuity checking
    #[serde(rename = "b")]
    pub bids: Vec<(String, String)>, // (price, quantity)
    #[serde(rename = "a")]
    pub asks: Vec<(String, String)>,
}

/// Aster WebSocket mark price stream message (Binance-compatible, includes funding rate)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AsterWsMarkPrice {
    #[serde(rename = "e")]
    pub event_type: String, // "markPriceUpdate"
    #[serde(rename = "E")]
    pub event_time: i64,
    #[serde(rename = "s")]
    pub symbol: String,
    #[serde(rename = "p")]
    pub mark_price: String,
    #[serde(rename = "i")]
    pub index_price: String,
    #[serde(rename = "P")]
    pub estimated_settle_price: String,
    #[serde(rename = "r")]
    pub funding_rate: String,
    #[serde(rename = "T")]
    pub next_funding_time: i64,
}

/// Combined stream wrapper (for multi-stream endpoint)
#[derive(Debug, Clone, Deserialize)]
pub struct AsterWsCombinedStream {
    pub stream: String,
    pub data: serde_json::Value,
}
