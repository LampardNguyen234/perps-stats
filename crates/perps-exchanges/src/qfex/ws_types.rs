use serde::{Deserialize, Serialize};

// ── Outbound (client → server) ────────────────────────────────────────────────

/// Subscribe/unsubscribe request sent to the MDS server.
///
/// `sig_figs` is an array (even for a single value), e.g. `Some(vec![0])`.
#[derive(Debug, Serialize)]
pub struct WsSubscribeRequest {
    pub r#type: String,
    pub channels: Vec<String>,
    pub symbols: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sig_figs: Option<Vec<u8>>,
}

// ── Inbound (server → client) ─────────────────────────────────────────────────

/// Server acknowledgement of a subscription.
#[derive(Debug, Clone, Deserialize)]
pub struct WsSubscribedMessage {
    pub r#type: String,
    pub channels: Vec<String>,
    pub symbols: Vec<String>,
    pub sig_figs: Vec<u8>,
}

/// L2 full orderbook snapshot.
///
/// Each message is a **complete** snapshot (not a delta). Arrives ~500ms per subscribed symbol.
/// `sequence` is global per-connection — use for gap detection only (no merge logic needed).
#[derive(Debug, Clone, Deserialize)]
pub struct WsL2Message {
    /// Global per-connection sequence number; use for gap detection.
    pub sequence: u64,
    pub r#type: String,
    /// ISO 8601 with nanosecond precision.
    pub time: String,
    /// QFEX symbol, e.g. `"NVDA-USD"`.
    pub symbol: String,
    /// `[price, qty]` pairs, descending order.
    pub bid: Vec<[String; 2]>,
    /// `[price, qty]` pairs, ascending order.
    pub ask: Vec<[String; 2]>,
    /// Aggregation level identifier (0 = raw prices).
    pub sig_figs: u8,
}

/// Best bid/offer update.
#[derive(Debug, Clone, Deserialize)]
pub struct WsBboMessage {
    pub sequence: Option<u64>,
    pub r#type: String,
    pub time: Option<String>,
    pub symbol: Option<String>,
    /// `[price, qty]` of best bid.
    pub bid: Vec<[String; 2]>,
    /// `[price, qty]` of best ask.
    pub ask: Vec<[String; 2]>,
}

/// Public trade execution.
#[derive(Debug, Clone, Deserialize)]
pub struct WsTradeMessage {
    pub trade_id: Option<String>,
    pub r#type: String,
    pub time: Option<String>,
    pub symbol: Option<String>,
    pub size: Option<String>,
    pub price: Option<String>,
    /// `"buy"` or `"sell"`.
    pub side: Option<String>,
    pub execution_type: Option<String>,
}

/// Funding rate update.
///
/// `rate` is a decimal ratio (same convention as `funding_rate_bps`).
#[derive(Debug, Clone, Deserialize)]
pub struct WsFundingMessage {
    pub r#type: String,
    pub time: Option<String>,
    pub symbol: Option<String>,
    pub rate: Option<f64>,
}

/// Minimal envelope used to dispatch incoming messages by `type` field.
#[derive(Debug, Deserialize)]
pub struct WsEnvelope {
    pub r#type: Option<String>,
}
