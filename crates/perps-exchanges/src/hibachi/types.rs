use serde::Deserialize;

// ---- exchange info ----

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExchangeInfoResponse {
    pub future_contracts: Vec<FutureContract>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FutureContract {
    pub symbol: String,            // e.g. "BTC/USDT-P"
    pub underlying_symbol: String, // e.g. "BTC"
    #[serde(default)]
    pub live: bool,
    pub display_name: Option<String>,
    pub min_notional: Option<String>,
    pub min_order_size: Option<String>,
    /// Valid granularity strings for the orderbook endpoint, e.g. `["0.1", "1", "10", "100"]`.
    #[serde(default)]
    pub orderbook_granularities: Vec<String>,
}

// ---- prices ----

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PricesResponse {
    pub trade_price: Option<String>,
    pub mark_price: Option<String>,
    pub spot_price: Option<String>,
    pub ask_price: Option<String>,
    pub bid_price: Option<String>,
    pub funding_rate_estimation: Option<FundingRateEstimation>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FundingRateEstimation {
    pub estimated_funding_rate: Option<String>,
    /// Unix seconds
    pub next_funding_timestamp: Option<i64>,
}

// ---- stats ----

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatsResponse {
    pub high_24h: Option<String>,
    pub low_24h: Option<String>,
    /// Base volume
    pub volume_24h: Option<String>,
}

// ---- open interest ----

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OIResponse {
    /// Base quantity
    pub total_quantity: Option<String>,
}

// ---- orderbook ----

#[derive(Debug, Clone, Deserialize)]
pub struct OrderbookResponse {
    pub ask: Option<OrderbookSide>,
    pub bid: Option<OrderbookSide>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OrderbookSide {
    pub levels: Vec<OrderbookLevel>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OrderbookLevel {
    pub price: String,
    pub quantity: String,
}

// ---- trades ----

#[derive(Debug, Deserialize)]
pub struct TradesResponse {
    pub trades: Vec<TradeEntry>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TradeEntry {
    pub price: Option<String>,
    pub quantity: Option<String>,
    /// "Buy" or "Sell"
    pub taker_side: Option<String>,
    /// Unix seconds
    pub timestamp: Option<i64>,
}

// ---- klines ----

#[derive(Debug, Deserialize)]
pub struct KlinesResponse {
    pub klines: Vec<KlineEntry>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KlineEntry {
    pub open: Option<String>,
    pub high: Option<String>,
    pub low: Option<String>,
    pub close: Option<String>,
    /// Notional (quote) volume
    pub volume_notional: Option<String>,
    /// Unix seconds
    pub timestamp: Option<i64>,
}

// ---- funding rate history ----

#[derive(Debug, Deserialize)]
pub struct FundingRatesResponse {
    pub data: Vec<FundingRateEntry>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FundingRateEntry {
    /// Unix seconds (float)
    pub funding_timestamp: Option<f64>,
    pub funding_rate: Option<String>,
}
