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

/// Response from the /orderbook endpoint
#[derive(Debug, Deserialize, Clone)]
pub struct OrderbookResponse {
    pub product_id: u32,
    pub ticker_id: String,
    /// Bids are [[price, quantity], ...]
    pub bids: Vec<[f64; 2]>,
    /// Asks are [[price, quantity], ...]
    pub asks: Vec<[f64; 2]>,
    pub timestamp: i64,
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
