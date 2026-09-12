use serde::{Deserialize, Serialize};

/// Extended WebSocket orderbook level
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExtendedWsLevel {
    /// Price
    #[serde(rename = "p")]
    pub price: String,

    /// Quantity — the delta in DELTA messages, already absolute in SNAPSHOT messages
    #[serde(rename = "q")]
    pub quantity: String,

    /// Absolute resulting quantity at this price level after applying the update.
    /// Only present on DELTA messages; SNAPSHOT messages have no `c` since `q` is
    /// already absolute there.
    #[serde(default, rename = "c")]
    pub cumulative_quantity: Option<String>,
}

/// Extended WebSocket orderbook data (nested inside message)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExtendedWsOrderbookData {
    /// Update type: "SNAPSHOT" or "DELTA"
    #[serde(rename = "t")]
    pub update_type: String,

    /// Market identifier (e.g., "BTC-USD")
    #[serde(rename = "m")]
    pub market: String,

    /// Bid levels
    #[serde(default, rename = "b")]
    pub bids: Vec<ExtendedWsLevel>,

    /// Ask levels
    #[serde(default, rename = "a")]
    pub asks: Vec<ExtendedWsLevel>,

    /// Timestamp in milliseconds
    #[serde(default, rename = "ts")]
    pub timestamp: Option<i64>,

    /// Sequence number
    #[serde(default, rename = "seq")]
    pub sequence: Option<i64>,
}

/// Extended WebSocket orderbook update message
/// Extended provides:
/// - Initial response: Full snapshot
/// - Every minute: New snapshot
/// - Between snapshots: Delta updates (only changes)
/// - Best Bid & Ask: Always provided as snapshots
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExtendedWsOrderbook {
    /// Message type: "SNAPSHOT" or "DELTA"
    #[serde(rename = "type")]
    pub message_type: String,

    /// Orderbook data (nested)
    pub data: ExtendedWsOrderbookData,
}

/// Extended WebSocket subscription message
#[derive(Debug, Clone, Serialize)]
pub struct ExtendedWsSubscribe {
    #[serde(rename = "type")]
    pub message_type: String, // "subscribe"
    pub channel: String, // e.g., "orderbook"
    pub market: String,  // e.g., "BTC-USD"
}

/// Extended WebSocket unsubscribe message
#[derive(Debug, Clone, Serialize)]
pub struct ExtendedWsUnsubscribe {
    #[serde(rename = "type")]
    pub message_type: String, // "unsubscribe"
    pub channel: String,
    pub market: String,
}

/// Extended WebSocket subscription response
#[derive(Debug, Clone, Deserialize)]
pub struct ExtendedWsSubscriptionResponse {
    #[serde(rename = "type")]
    pub message_type: String, // "subscribed" or "unsubscribed"
    pub channel: String,
    pub market: String,
}
