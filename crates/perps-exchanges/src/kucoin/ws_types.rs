use serde::{Deserialize, Serialize};

/// KuCoin WebSocket token response
#[derive(Debug, Clone, Deserialize)]
pub struct KuCoinWsTokenResponse {
    pub code: String,
    pub data: KuCoinWsTokenData,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KuCoinWsTokenData {
    pub token: String,
    pub instance_servers: Vec<KuCoinWsServer>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KuCoinWsServer {
    pub endpoint: String,
    pub encrypt: bool,
    pub protocol: String,
    pub ping_interval: i64,
    pub ping_timeout: i64,
}

/// KuCoin WebSocket subscription request
#[derive(Debug, Clone, Serialize)]
pub struct KuCoinWsSubscribeRequest {
    pub id: String,
    #[serde(rename = "type")]
    pub msg_type: String, // "subscribe"
    pub topic: String,
    #[serde(rename = "privateChannel")]
    pub private_channel: bool,
    pub response: bool,
}

/// KuCoin WebSocket ticker message
/// Topic: /contractMarket/ticker:{symbol}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KuCoinWsTicker {
    pub symbol: String,
    pub last_traded_price: Option<String>,
    pub mark_price: Option<String>,
    pub index_price: Option<String>,
    pub best_bid_price: Option<String>,
    pub best_bid_size: Option<i64>,
    pub best_ask_price: Option<String>,
    pub best_ask_size: Option<i64>,
    #[serde(rename = "ts")]
    pub timestamp: i64, // Milliseconds
}

/// KuCoin WebSocket execution (trade) message
/// Topic: /contractMarket/execution:{symbol}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KuCoinWsExecution {
    pub symbol: String,
    pub sequence: i64,
    pub side: String, // "buy" or "sell"
    pub size: i64,
    pub price: String,
    #[serde(rename = "takerOrderId")]
    pub taker_order_id: String,
    pub trade_id: String,
    #[serde(rename = "ts")]
    pub timestamp: i64, // Nanoseconds
}

/// KuCoin WebSocket level 2 orderbook message
/// Topic: /contractMarket/level2:{symbol}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct KuCoinWsLevel2 {
    pub symbol: String,
    pub sequence: i64,
    pub change: String, // Format: "price,side,size"
    #[serde(rename = "ts")]
    pub timestamp: i64, // Nanoseconds
}

/// KuCoin WebSocket klines/candlestick message
/// Topic: /contractMarket/limitCandle:{symbol}_{interval}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct KuCoinWsKline {
    pub symbol: String,
    pub candles: Vec<String>, // [start_time, open, close, high, low, volume, amount]
    pub time: i64, // Milliseconds
}

/// KuCoin WebSocket response wrapper
#[derive(Debug, Clone, Deserialize)]
pub struct KuCoinWsResponse {
    #[serde(rename = "type")]
    pub msg_type: String, // "message", "welcome", "ack", "pong"
    pub topic: Option<String>,
    pub subject: Option<String>,
    pub data: Option<serde_json::Value>,
}
