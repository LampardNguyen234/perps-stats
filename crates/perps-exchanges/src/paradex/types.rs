use serde::Deserialize;

// For /v1/markets
#[derive(Debug, Deserialize, Clone)]
pub struct MarketsResponse {
    pub results: Vec<ParadexMarket>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ParadexMarket {
    pub symbol: String,
    #[serde(rename = "price_tick_size")]
    pub price_tick_size: String,
    #[serde(rename = "order_size_increment")]
    pub order_size_increment: String,
    #[serde(rename = "min_notional")]
    pub min_notional: String,
}

// For /v1/orderbook/{symbol}
#[derive(Debug, Deserialize, Clone)]
pub struct OrderbookResponse {
    pub market: String,
    pub seq_no: u64,
    pub last_updated_at: u64,
    pub asks: Vec<(String, String)>,
    pub bids: Vec<(String, String)>,
}

// For /v1/trades?symbol=...
#[derive(Debug, Deserialize, Clone)]
pub struct TradesResponse {
    pub trades: Vec<ParadexTrade>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ParadexTrade {
    pub price: String,
    pub size: String,
    pub side: String,
    pub timestamp: i64,
}

// For /v1/funding/data?market=...
#[derive(Debug, Deserialize, Clone)]
pub struct FundingRateResponse {
    pub results: Vec<ParadexFundingRate>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ParadexFundingRate {
    pub market: String,
    #[serde(rename = "funding_rate")]
    pub funding_rate: String,
    #[serde(rename = "created_at")]
    pub created_at: u64,
}

// For /v1/markets/klines
// Format: [timestamp, open, high, low, close, volume]
// All price and volume fields are returned as numbers, not strings
#[derive(Debug, Deserialize, Clone)]
pub struct KlinesResponse {
    pub results: Vec<(u64, f64, f64, f64, f64, f64)>,
}

// For /v1/bbo/{symbol}
#[derive(Debug, Deserialize, Clone)]
pub struct BboResponse {
    pub market: String,
    pub seq_no: u64,
    pub ask: String,
    pub ask_size: String,
    pub bid: String,
    pub bid_size: String,
    pub last_updated_at: u64,
}

// For /v1/markets/summary
#[derive(Debug, Deserialize, Clone)]
pub struct MarketSummaryResponse {
    pub results: Vec<ParadexMarketSummary>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ParadexMarketSummary {
    pub symbol: String,
    #[serde(rename = "mark_price")]
    pub mark_price: String,
    #[serde(rename = "last_traded_price")]
    pub last_traded_price: String,
    pub bid: String,
    pub ask: String,
    #[serde(rename = "volume_24h")]
    pub volume_24h: String,
    #[serde(rename = "price_change_rate_24h")]
    pub price_change_rate_24h: String,
    #[serde(rename = "open_interest")]
    pub open_interest: String,
    #[serde(rename = "funding_rate")]
    pub funding_rate: String,
    #[serde(rename = "underlying_price")]
    pub underlying_price: String,
    #[serde(rename = "created_at")]
    pub created_at: u64,
    // All other fields are optional and ignored
    #[serde(flatten)]
    pub extra: Option<serde_json::Value>,
}
