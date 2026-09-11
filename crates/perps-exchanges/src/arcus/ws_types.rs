use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::types::ArcusMarket;

/// Arcus WebSocket subscription request.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArcusWsSubscribe {
    #[serde(rename = "type")]
    pub msg_type: &'static str,
    pub channel: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n_levels: Option<u32>,
}

impl ArcusWsSubscribe {
    pub fn market(channel: &str, id: String) -> Self {
        Self {
            msg_type: "subscribe",
            channel: channel.to_string(),
            id: Some(id),
            n_levels: None,
        }
    }

    pub fn orderbook(id: String, n_levels: u32) -> Self {
        Self {
            msg_type: "subscribe",
            channel: "l2Orderbook".to_string(),
            id: Some(id),
            n_levels: Some(n_levels.clamp(1, 100)),
        }
    }

    pub fn markets() -> Self {
        Self {
            msg_type: "subscribe",
            channel: "markets".to_string(),
            id: None,
            n_levels: None,
        }
    }
}

/// Generic envelope used by Arcus server-pushed messages.
///
/// `channel` is absent from the initial `connected` frame, while `contents` is absent from
/// errors and degraded notices. Defaults let the dispatch loop recognize those frames without
/// treating them as malformed channel payloads.
#[derive(Debug, Deserialize)]
pub struct ArcusWsEnvelope {
    #[serde(rename = "type")]
    pub msg_type: String,
    #[serde(default)]
    pub channel: String,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub contents: serde_json::Value,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default, rename = "retryAfterMs")]
    pub retry_after_ms: Option<u64>,
}

/// Full L2 book contents. Arcus may omit the sequence/timestamp fields when their value is zero.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArcusWsOrderbookContents {
    pub bids: Vec<[String; 2]>,
    pub asks: Vec<[String; 2]>,
    pub last_sequence_id: i64,
    #[serde(default)]
    pub global_sequence_id: Option<i64>,
    #[serde(default)]
    pub timestamp: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArcusWsPriceSize {
    pub price: String,
    pub size: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArcusWsBboContents {
    pub best_bid: Option<ArcusWsPriceSize>,
    pub best_ask: Option<ArcusWsPriceSize>,
    pub timestamp: i64,
    pub last_sequence_id: i64,
    #[serde(default)]
    pub global_sequence_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArcusWsMarketsContents {
    pub is_snapshot: bool,
    /// Live and documented payloads key this map by the numeric market id serialized as text.
    /// Consumers intentionally use `market_display_name` from the value instead of this key.
    pub markets: HashMap<String, ArcusWsMarketEntry>,
}

/// The WebSocket markets channel uses the same per-market shape as `GET /v1/markets`.
pub type ArcusWsMarketEntry = ArcusMarket;
