use serde::{Deserialize, Serialize};

/// Paradex WebSocket subscription request
#[derive(Debug, Clone, Serialize)]
pub struct ParadexWsSubscribeRequest {
    pub method: String, // "SUBSCRIBE"
    pub params: ParadexWsSubscribeParams,
}

#[derive(Debug, Clone, Serialize)]
pub struct ParadexWsSubscribeParams {
    pub channel: String,           // e.g., "markets_summary", "order_book", "trades"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub market: Option<String>,    // e.g., "BTC-USD-PERP"
}

/// Paradex WebSocket subscription response
#[derive(Debug, Clone, Deserialize)]
pub struct ParadexWsSubscribeResponse {
    pub channel: String,
    pub result: bool,
}

/// Paradex WebSocket market summary message
/// Channel: markets_summary
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ParadexWsMarketSummary {
    pub channel: String,
    pub params: ParadexMarketSummaryData,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ParadexMarketSummaryData {
    pub data: ParadexMarketSummaryItem,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ParadexMarketSummaryItem {
    pub symbol: String,
    pub last_traded_price: String,
    pub mark_price: String,
    pub underlying_price: String, // index_price
    pub volume_24h: String,
    pub price_change_rate_24h: String,
    pub open_interest: String,
    pub funding_rate: String,
    pub created_at: u64, // Milliseconds
}

/// Paradex WebSocket order book message
/// Channel: order_book
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ParadexWsOrderbook {
    pub channel: String,
    pub params: ParadexOrderbookData,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ParadexOrderbookData {
    pub data: ParadexOrderbookSnapshot,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ParadexOrderbookSnapshot {
    pub market: String,
    pub bids: Vec<(String, String)>, // (price, quantity)
    pub asks: Vec<(String, String)>, // (price, quantity)
    pub last_updated_at: u64,        // Milliseconds
}

/// Paradex WebSocket trades message
/// Channel: trades
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ParadexWsTrades {
    pub channel: String,
    pub params: ParadexTradesData,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ParadexTradesData {
    pub data: Vec<ParadexTradeItem>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ParadexTradeItem {
    pub market: String,
    pub price: String,
    pub size: String,
    pub side: String,     // "buy" or "sell"
    pub timestamp: i64,   // Unix timestamp in seconds
}

/// Paradex WebSocket funding data message
/// Channel: funding_data
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ParadexWsFundingData {
    pub channel: String,
    pub params: ParadexFundingDataParams,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ParadexFundingDataParams {
    pub data: ParadexFundingDataItem,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ParadexFundingDataItem {
    pub market: String,
    pub funding_rate: String,
    pub created_at: u64, // Milliseconds
}

/// Paradex WebSocket ping message
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ParadexWsPing {
    #[serde(rename = "type")]
    pub msg_type: String, // "ping"
}

/// Paradex WebSocket pong response
#[derive(Debug, Clone, Serialize)]
pub struct ParadexWsPong {
    #[serde(rename = "type")]
    pub msg_type: String, // "pong"
}

/// Generic WebSocket response wrapper
#[derive(Debug, Clone, Deserialize)]
pub struct ParadexWsResponse {
    #[serde(rename = "type")]
    pub msg_type: Option<String>,
    pub channel: Option<String>,
    #[serde(flatten)]
    pub data: serde_json::Value,
}
