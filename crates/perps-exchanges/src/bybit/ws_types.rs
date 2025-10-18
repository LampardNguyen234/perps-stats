use serde::{Deserialize, Serialize};

/// Bybit WebSocket subscription request
#[derive(Debug, Clone, Serialize)]
pub struct BybitWsSubscribeRequest {
    pub op: String, // "subscribe"
    pub args: Vec<String>,
}

/// Bybit WebSocket ticker message
/// Topic: tickers.{symbol}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BybitWsTicker {
    pub symbol: String,
    pub last_price: String,
    pub mark_price: String,
    pub index_price: String,
    pub bid1_price: String,
    pub bid1_size: String,
    pub ask1_price: String,
    pub ask1_size: String,
    pub volume24h: String,
    pub turnover24h: String,
    pub price24h_pcnt: String,
    pub high_price24h: String,
    pub low_price24h: String,
    #[serde(default)]
    pub prev_price24h: String,
}

/// Bybit WebSocket trade message
/// Topic: publicTrade.{symbol}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BybitWsTrade {
    #[serde(rename = "i")]
    pub id: String,
    #[serde(rename = "T")]
    pub timestamp: i64, // Milliseconds
    #[serde(rename = "p")]
    pub price: String,
    #[serde(rename = "v")]
    pub size: String,
    #[serde(rename = "S")]
    pub side: String, // "Buy" or "Sell"
    #[serde(rename = "s")]
    pub symbol: String,
}

/// Bybit WebSocket orderbook level
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BybitWsBookLevel {
    pub price: String,
    pub size: String,
}

/// Bybit WebSocket orderbook message
/// Topic: orderbook.{depth}.{symbol}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BybitWsOrderbook {
    pub s: String,           // Symbol
    pub b: Vec<Vec<String>>, // Bids [[price, size], ...]
    pub a: Vec<Vec<String>>, // Asks [[price, size], ...]
    pub u: i64,              // Update ID
    pub seq: i64,            // Sequence number
    #[serde(rename = "cts")]
    pub timestamp: i64, // Cross sequence timestamp
}

/// Bybit WebSocket response wrapper
#[derive(Debug, Clone, Deserialize)]
pub struct BybitWsResponse {
    pub topic: String,
    #[serde(rename = "type")]
    pub msg_type: String, // "snapshot" or "delta"
    pub ts: i64, // Timestamp
    pub data: serde_json::Value,
}

/// Bybit WebSocket subscription response
#[derive(Debug, Clone, Deserialize)]
pub struct BybitWsSubscriptionResponse {
    pub success: bool,
    pub ret_msg: String,
    pub op: String,
    #[serde(default)]
    pub conn_id: String,
}
