use serde::{Deserialize, Serialize};

/// Pacifica WebSocket subscription request
/// Format: {"method": "subscribe", "params": {"source": "book", "symbol": "SOL", "agg_level": 1}}
#[derive(Debug, Clone, Serialize)]
pub struct PacificaWsSubscribeRequest {
    pub method: String,
    pub params: PacificaWsSubscribeParams,
}

#[derive(Debug, Clone, Serialize)]
pub struct PacificaWsSubscribeParams {
    pub source: String, // "book"
    pub symbol: String,
    pub agg_level: u32, // Aggregation level for orderbook
}

/// Pacifica WebSocket response wrapper
/// Format: {"channel":"subscribe","data":...} or {"channel":"book","data":...}
#[derive(Debug, Clone, Deserialize)]
pub struct PacificaWsResponse {
    pub channel: String, // "subscribe", "book", "error"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Pacifica WebSocket orderbook update message
/// Based on similar structure to REST API response
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PacificaWsOrderbookUpdate {
    /// Symbol
    #[serde(rename = "s")]
    pub symbol: String,
    /// Levels - nested array: [[bids...], [asks...]]
    /// Index 0 = bids, Index 1 = asks
    #[serde(rename = "l")]
    pub levels: Vec<Vec<PacificaWsOrderbookLevel>>,
    /// Timestamp in milliseconds
    #[serde(rename = "t")]
    pub timestamp: i64,
    /// Sequence number (if provided by exchange)
    #[serde(rename = "seq", skip_serializing_if = "Option::is_none")]
    pub sequence: Option<i64>,
}

/// Individual orderbook level in WebSocket update
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PacificaWsOrderbookLevel {
    /// Price
    #[serde(rename = "p")]
    pub price: String,
    /// Amount/quantity
    #[serde(rename = "a")]
    pub amount: String,
    /// Number of orders (optional)
    #[serde(rename = "n", skip_serializing_if = "Option::is_none")]
    pub num_orders: Option<i32>,
}

/// Pacifica WebSocket ping request
#[derive(Debug, Clone, Serialize)]
pub struct PacificaWsPingRequest {
    #[serde(rename = "type")]
    pub msg_type: String, // "ping"
}
