use serde::Deserialize;

/// All RISEx endpoints return {"data": ..., "request_id": "..."}.
#[derive(Debug, Clone, Deserialize)]
pub struct ApiEnvelope<T> {
    pub data: T,
    pub request_id: Option<String>,
}

/// Data payload from GET /v1/markets
#[derive(Debug, Clone, Deserialize)]
pub struct ApiGetMarketsResponse {
    pub markets: Vec<ApiMarketInfo>,
}

/// Single market entry from /v1/markets
#[derive(Debug, Clone, Deserialize)]
pub struct ApiMarketInfo {
    pub market_id: String,
    pub base_asset_symbol: String,
    pub quote_asset_symbol: String,
    pub display_name: String,
    pub available: bool,
    pub visible: Option<bool>,
    pub config: ApiMarketConfig,

    // Ticker fields
    pub last_price: Option<String>,
    pub mark_price: Option<String>,
    pub index_price: Option<String>,
    pub high_24h: Option<String>,
    pub low_24h: Option<String>,
    pub change_24h: Option<String>,
    pub quote_volume_24h: Option<String>,

    // Open interest
    pub open_interest: Option<String>,

    // Funding
    pub current_funding_rate: Option<String>,
    pub predicted_funding_rate: Option<String>,
    pub funding_rate_8h: Option<String>,
    /// nanoseconds as string
    pub funding_interval: Option<String>,
    pub next_funding_time: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiMarketConfig {
    pub name: String,
    pub max_leverage: Option<String>,
    pub min_order_size: Option<String>,
    pub step_size: Option<String>,
    pub step_price: Option<String>,
    pub open_interest_limit: Option<String>,
}

/// Data payload from GET /v1/orderbook
#[derive(Debug, Clone, Deserialize)]
pub struct ApiGetOrderbookResponse {
    pub market_id: String,
    pub bids: Vec<ApiPriceLevel>,
    pub asks: Vec<ApiPriceLevel>,
    pub total_bids: Option<String>,
    pub total_asks: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiPriceLevel {
    pub price: String,
    pub quantity: String,
    pub order_count: Option<i32>,
}

/// Data payload from GET /v1/markets/id/{id}/trading-view-data
#[derive(Debug, Clone, Deserialize)]
pub struct ApiGetKlinesResponse {
    pub data: Vec<ApiKlineBar>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiKlineBar {
    pub market_id: Option<String>,
    /// string label e.g. "1h"
    pub interval: Option<String>,
    /// nanoseconds
    pub time: String,
    pub open: String,
    pub high: String,
    pub low: String,
    pub close: String,
    pub volume: String,
}

/// Data payload from GET /v1/markets/id/{id}/trade-history
#[derive(Debug, Clone, Deserialize)]
pub struct ApiGetTradeHistoryResponse {
    pub market_id: Option<String>,
    pub trades: Vec<ApiTrade>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiTrade {
    pub id: Option<String>,
    /// "BUY" or "SELL" — this is the maker side
    pub maker_side: String,
    pub price: String,
    pub size: String,
    /// nanoseconds
    pub time: String,
    pub block_number: Option<String>,
}

/// Data payload from GET /v1/markets/id/{id}/funding-rate-history
#[derive(Debug, Clone, Deserialize)]
pub struct ApiGetFundingHistoryResponse {
    pub market_id: Option<String>,
    pub records: Vec<ApiFundingRecord>,
    pub page: Option<i64>,
    pub has_next_page: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiFundingRecord {
    /// 18-decimal ratio
    pub funding_rate: String,
    pub accumulated_funding: Option<String>,
    pub index_price: Option<String>,
    /// nanoseconds
    pub start_time: String,
    /// nanoseconds
    pub end_time: String,
    pub block_number: Option<String>,
    pub tx_hash: Option<String>,
}
