use serde::{Deserialize, Serialize};

/// Outbound: subscribe or unsubscribe request.
#[derive(Debug, Serialize)]
pub struct WsRequest {
    pub method: String,
    pub params: WsRequestParams,
}

#[derive(Debug, Serialize)]
pub struct WsRequestParams {
    pub channel: String,
    pub market_ids: Vec<u64>,
}

/// Envelope used to dispatch inbound messages by type field.
#[derive(Debug, Deserialize)]
pub struct WsMsgType {
    #[serde(rename = "type")]
    pub msg_type: String,
}

/// A single price level. Prices and quantities are human-readable decimal strings.
#[derive(Debug, Deserialize, Clone)]
pub struct WsLevel {
    pub price: String,
    /// "0" means delete this price level from the local book.
    pub quantity: String,
}

/// The nested `data` payload inside both snapshot and update messages.
#[derive(Debug, Deserialize)]
pub struct WsLevels {
    pub bids: Vec<WsLevel>,
    pub asks: Vec<WsLevel>,
}

/// Full orderbook snapshot — sent once immediately after subscribing.
#[derive(Debug, Deserialize)]
pub struct WsSnapshot {
    /// RISEx sends market_id as a JSON string at the top level (e.g. "18").
    pub market_id: String,
    pub data: WsLevels,
}

/// Incremental orderbook update. quantity "0" = delete that price level.
#[derive(Debug, Deserialize)]
pub struct WsUpdate {
    /// RISEx sends market_id as a JSON string at the top level (e.g. "18").
    pub market_id: String,
    pub data: WsLevels,
    /// CRC32-IEEE of the book state after this update is applied.
    pub checksum: u32,
}
