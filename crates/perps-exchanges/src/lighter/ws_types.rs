use serde::{Deserialize, Serialize};

/// Lighter WebSocket subscription request
#[derive(Debug, Clone, Serialize)]
pub struct LighterWsSubscribeRequest {
    #[serde(rename = "type")]
    pub msg_type: String, // "subscribe"
    pub channel: String,  // e.g., "order_book/0", "market_stats/0", "trade/0"
}

/// Lighter WebSocket orderbook message
/// Channel: order_book:{MARKET_INDEX}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LighterWsOrderbook {
    pub channel: String,
    pub order_book: LighterOrderBook,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LighterOrderBook {
    pub asks: Vec<LighterOrderbookLevel>,
    pub bids: Vec<LighterOrderbookLevel>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LighterOrderbookLevel {
    pub price: String,
    pub size: String,
}

/// Lighter WebSocket market stats message
/// Channel: market_stats:{MARKET_INDEX}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LighterWsMarketStats {
    pub channel: String,
    pub market_stats: LighterMarketStatsData,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LighterMarketStatsData {
    pub symbol: String,
    pub market_id: u64,
    pub index_price: f64,
    pub mark_price: f64,
    pub last_trade_price: f64,
    pub daily_volume: f64,
    pub daily_quote_volume: f64,
    pub daily_price_change: f64,
    pub daily_price_high: f64,
    pub daily_price_low: f64,
    pub funding_rate: f64,
    pub open_interest: f64,
    #[serde(default)]
    pub timestamp: Option<i64>,
}

/// Lighter WebSocket trade message
/// Channel: trade:{MARKET_INDEX}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LighterWsTrade {
    pub channel: String,
    pub trades: Vec<LighterTradeData>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LighterTradeData {
    pub trade_id: String,
    pub market_id: u64,
    pub symbol: String,
    pub price: String,
    pub size: String,
    pub side: String, // "buy" or "sell"
    pub timestamp: i64,
}

/// Lighter WebSocket response wrapper (can be any channel)
#[derive(Debug, Clone, Deserialize)]
pub struct LighterWsResponse {
    pub channel: Option<String>,
    #[serde(flatten)]
    pub data: serde_json::Value,
}
