use serde::{Deserialize, Serialize};

/// Paradex WebSocket subscription request (JSON-RPC 2.0)
#[derive(Debug, Clone, Serialize)]
pub struct ParadexWsSubscribeRequest {
    pub jsonrpc: String, // "2.0"
    pub method: String,  // "subscribe"
    pub id: u32,
    pub params: ParadexWsSubscribeParams,
}

impl ParadexWsSubscribeRequest {
    pub fn new(id: u32, channel: String) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            method: "subscribe".to_string(),
            id,
            params: ParadexWsSubscribeParams { channel },
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ParadexWsSubscribeParams {
    /// Dotted channel name, e.g. "markets_summary.BTC-USD-PERP",
    /// "order_book.BTC-USD-PERP.snapshot@15@100ms", "trades.BTC-USD-PERP"
    pub channel: String,
}

/// Top-level incoming message envelope
#[derive(Debug, Clone, Deserialize)]
pub struct ParadexWsMessage {
    pub method: Option<String>,
    pub id: Option<u32>,
    pub params: Option<ParadexWsSubscriptionParams>,
    pub result: Option<serde_json::Value>,
    pub error: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ParadexWsSubscriptionParams {
    pub channel: String,
    pub data: serde_json::Value,
}

/// markets_summary.{market} data payload
#[derive(Debug, Clone, Deserialize)]
pub struct ParadexMarketSummaryItem {
    pub symbol: String,
    pub last_traded_price: String,
    pub mark_price: String,
    pub underlying_price: String,
    pub volume_24h: String,
    pub price_change_rate_24h: String,
    pub open_interest: String,
    pub funding_rate: String,
    pub created_at: u64,
    // bid/ask present in WS but may be empty strings when no liquidity
    pub bid: Option<String>,
    pub bid_size: Option<String>,
    pub ask: Option<String>,
    pub ask_size: Option<String>,
}

/// order_book.{market}.snapshot@15@100ms — single level entry in inserts/deletes
#[derive(Debug, Clone, Deserialize)]
pub struct ParadexOrderbookLevel {
    pub side: String, // "BUY" or "SELL"
    pub price: String,
    pub size: String,
}

/// order_book snapshot payload
#[derive(Debug, Clone, Deserialize)]
pub struct ParadexOrderbookSnapshot {
    pub market: String,
    pub last_updated_at: u64,
    /// "s" = full snapshot, "d" = delta
    pub update_type: String,
    pub inserts: Vec<ParadexOrderbookLevel>,
    #[serde(default)]
    pub deletes: Vec<ParadexOrderbookLevel>,
}

/// trades.{market} — single trade
#[derive(Debug, Clone, Deserialize)]
pub struct ParadexTradeItem {
    pub market: Option<String>,
    pub price: String,
    pub size: String,
    pub side: String,   // "BUY" or "SELL"
    pub timestamp: i64, // Unix seconds
}

/// funding_data.{market} payload
#[derive(Debug, Clone, Deserialize)]
pub struct ParadexFundingDataItem {
    pub market: String,
    pub funding_rate: String,
    pub created_at: u64,
}

/// Pong response to server ping
#[derive(Debug, Clone, Serialize)]
pub struct ParadexWsPong {
    #[serde(rename = "type")]
    pub msg_type: String, // "pong"
}
