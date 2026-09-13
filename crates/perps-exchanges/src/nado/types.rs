use serde::Deserialize;
use std::collections::HashMap;

/// Response from the /pairs endpoint
#[derive(Debug, Deserialize, Clone)]
pub struct Pair {
    pub product_id: u32,
    pub ticker_id: String,
    pub base: String,
    pub quote: String,
}

/// Response from the /tickers endpoint
/// Returns a HashMap<ticker_id, TickerData>
pub type TickersResponse = HashMap<String, TickerData>;

#[derive(Debug, Deserialize, Clone)]
pub struct TickerData {
    pub product_id: u32,
    pub ticker_id: String,
    pub base_currency: String,
    pub quote_currency: String,
    pub last_price: f64,
    pub base_volume: f64,
    pub quote_volume: f64,
    pub price_change_percent_24h: f64,
}

/// Response from the /contracts endpoint
/// Returns a HashMap<ticker_id, ContractData>
pub type ContractsResponse = HashMap<String, ContractData>;

#[derive(Debug, Deserialize, Clone)]
pub struct ContractData {
    pub product_id: u32,
    pub ticker_id: String,
    pub base_currency: String,
    pub quote_currency: String,
    pub last_price: f64,
    pub base_volume: f64,
    pub quote_volume: f64,
    pub product_type: String,
    pub contract_price: f64,
    pub contract_price_currency: String,
    pub open_interest: f64,
    pub open_interest_usd: f64,
    pub index_price: f64,
    pub mark_price: f64,
    pub funding_rate: f64,
    pub next_funding_rate_timestamp: i64,
    pub price_change_percent_24h: f64,
}

/// Response envelope from `GET /query?type=market_liquidity&...`
/// (all Nado gateway queries wrap their payload in `{status, data, request_type}`).
#[derive(Debug, Deserialize, Clone)]
pub struct MarketLiquidityQueryResponse {
    pub data: MarketLiquidityData,
}

/// `data` field of a `market_liquidity` query response.
#[derive(Debug, Deserialize, Clone)]
pub struct MarketLiquidityData {
    pub product_id: u32,
    /// Bids are [[price_x18, size_x18], ...], best price first.
    pub bids: Vec<[String; 2]>,
    /// Asks are [[price_x18, size_x18], ...], best price first.
    pub asks: Vec<[String; 2]>,
    /// Nanosecond timestamp - lives in the same clock as the WS book_depth stream's
    /// min/max/last_max_timestamp chain (per Nado's docs), unlike /orderbook's millisecond
    /// wall-clock capture time. This is what makes it usable as apply_snapshot's last_update_id.
    pub timestamp: String,
}

/// Response from the /trades endpoint
#[derive(Debug, Deserialize, Clone)]
pub struct TradeData {
    pub product_id: u32,
    pub ticker_id: String,
    pub trade_id: i64,
    pub price: f64,
    pub base_filled: f64,
    pub quote_filled: f64,
    pub timestamp: i64,
    pub trade_type: String, // "buy" or "sell"
}

/// Response from the /assets endpoint
#[derive(Debug, Deserialize, Clone)]
pub struct AssetData {
    pub product_id: u32,
    pub name: String,
    pub symbol: String,
    pub maker_fee: f64,
    pub taker_fee: f64,
    pub can_withdraw: bool,
    pub can_deposit: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ticker_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub market_type: Option<String>, // "spot" or "perp"
}
