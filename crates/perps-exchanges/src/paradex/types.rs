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
#[derive(Debug, Deserialize, Clone)]
pub struct KlinesResponse {
    pub results: Vec<(u64, String, String, String, String, String)>,
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
