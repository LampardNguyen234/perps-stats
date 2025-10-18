use serde::{Deserialize, Serialize};

/// Hyperliquid WebSocket subscription request wrapper
#[derive(Debug, Clone, Serialize)]
pub struct HyperliquidWsSubscribeRequest {
    pub method: String, // "subscribe"
    pub subscription: HyperliquidWsSubscription,
}

/// Hyperliquid WebSocket subscription details
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum HyperliquidWsSubscription {
    #[serde(rename = "trades")]
    Trades { coin: String },
    #[serde(rename = "l2Book")]
    L2Book { coin: String },
    #[serde(rename = "allMids")]
    AllMids,
    #[serde(rename = "candle")]
    Candle { coin: String, interval: String },
}

/// Hyperliquid WebSocket trade message
/// Doc: https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/websocket/subscriptions
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HyperliquidWsTrade {
    pub coin: String,
    pub side: String, // "A" for ask (sell), "B" for bid (buy)
    pub px: String,   // Price
    pub sz: String,   // Size
    pub hash: String,
    pub time: i64, // Milliseconds timestamp
    #[serde(default)]
    pub tid: Option<i64>, // Trade ID
}

/// Hyperliquid WebSocket L2 book message
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HyperliquidWsBook {
    pub coin: String,
    pub time: i64,                                // Milliseconds timestamp
    pub levels: Vec<Vec<HyperliquidWsBookLevel>>, // [bids, asks]
}

/// Book level [price, size]
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HyperliquidWsBookLevel {
    pub px: String, // Price
    pub sz: String, // Size
    pub n: i64,     // Number of orders
}

/// Hyperliquid WebSocket all mids message
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HyperliquidWsAllMids {
    pub mids: serde_json::Value, // Map of coin -> mid price
}

/// Hyperliquid WebSocket candle message
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HyperliquidWsCandle {
    #[serde(rename = "t")]
    pub open_time: i64, // Open timestamp (milliseconds)
    #[serde(rename = "T")]
    pub close_time: i64, // Close timestamp (milliseconds)
    #[serde(rename = "s")]
    pub coin: String,
    #[serde(rename = "i")]
    pub interval: String, // "1m", "5m", "15m", "1h", etc.
    #[serde(rename = "o")]
    pub open: String,
    #[serde(rename = "c")]
    pub close: String,
    #[serde(rename = "h")]
    pub high: String,
    #[serde(rename = "l")]
    pub low: String,
    #[serde(rename = "v")]
    pub volume: String,
    #[serde(rename = "n")]
    pub trade_count: i64,
}

/// WebSocket response wrapper
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "channel", content = "data")]
pub enum HyperliquidWsResponse {
    #[serde(rename = "trades")]
    Trades(Vec<HyperliquidWsTrade>),
    #[serde(rename = "l2Book")]
    L2Book(HyperliquidWsBook),
    #[serde(rename = "allMids")]
    AllMids(HyperliquidWsAllMids),
    #[serde(rename = "candle")]
    Candle(HyperliquidWsCandle),
    #[serde(rename = "subscriptionResponse")]
    SubscriptionResponse(serde_json::Value),
}
