use serde::{Deserialize, Serialize};

/// Nord (01.xyz) markets info response from `GET /info`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NordMarketsInfo {
    pub markets: Vec<NordMarketInfo>,
    pub tokens: Vec<NordTokenInfo>,
}

/// A single market entry from the Nord `/info` endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NordMarketInfo {
    pub market_id: u32,
    pub symbol: String,
    pub price_decimals: u32,
    pub size_decimals: u32,
    pub base_token_id: u32,
    pub quote_token_id: u32,
    pub imf: f64,
    pub mmf: f64,
    pub cmf: f64,
}

/// A single token entry from the Nord `/info` endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NordTokenInfo {
    pub token_id: u32,
    pub symbol: String,
    pub decimals: u32,
    pub mint_addr: String,
    pub weight_bps: u32,
}

/// Nord market stats response from `GET /market/{id}/stats`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NordMarketStats {
    pub index_price: Option<f64>,
    pub index_price_conf: Option<f64>,
    #[serde(default)]
    pub frozen: Option<bool>,
    pub volume_base_24h: f64,
    pub volume_quote_24h: f64,
    pub high_24h: Option<f64>,
    pub low_24h: Option<f64>,
    pub close_24h: Option<f64>,
    pub prev_close_24h: Option<f64>,
    pub perp_stats: Option<NordPerpStats>,
}

/// Perpetual-specific statistics nested within `NordMarketStats`
///
/// Note: The API returns snake_case field names for this struct,
/// so no serde renaming is needed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NordPerpStats {
    pub mark_price: Option<f64>,
    pub aggregated_funding_index: f64,
    pub funding_rate: f64,
    pub next_funding_time: String,
    pub open_interest: f64,
}

/// Nord orderbook response from `GET /market/{id}/orderbook`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NordOrderbookInfo {
    pub update_id: u64,
    pub asks: Vec<(f64, f64)>,
    pub bids: Vec<(f64, f64)>,
    pub asks_summary: NordOrderbookSummary,
    pub bids_summary: NordOrderbookSummary,
}

/// Summary statistics for one side of the Nord orderbook
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NordOrderbookSummary {
    pub sum: f64,
    pub count: u32,
}

/// Generic paginated result wrapper for Nord API responses
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NordPageResult<T> {
    pub items: Vec<T>,
    pub next_start_inclusive: Option<u64>,
}

/// A single trade from the Nord `GET /trades` endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NordTrade {
    pub trade_id: u64,
    pub maker_id: u32,
    pub taker_id: u32,
    pub market_id: u32,
    pub taker_side: String,
    pub price: f64,
    pub base_size: f64,
    pub order_id: u64,
    #[serde(default)]
    pub action_id: Option<u64>,
    pub time: String,
}

/// A single hourly market snapshot from the Nord `GET /market/{id}/history/PT1H` endpoint
///
/// Note: This is NOT OHLCV data. The endpoint returns hourly snapshots of
/// index price, mark price, and funding data.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NordMarketHistoryInfo {
    pub market_id: u32,
    pub time: String,
    pub action_id: u64,
    pub funding_index: f64,
    pub funding_rate: f64,
    pub index_price: f64,
    pub mark_price: f64,
}
