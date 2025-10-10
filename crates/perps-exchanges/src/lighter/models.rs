use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LighterResponse<T> {
    pub code: i32,
    #[serde(flatten)]
    pub data: T,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OrderBooksResponse {
    pub order_books: Vec<OrderBook>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OrderBook {
    pub symbol: String,
    pub market_id: u64,
    pub status: String,
    pub taker_fee: String,
    pub maker_fee: String,
    pub liquidation_fee: String,
    pub min_base_amount: String,
    pub min_quote_amount: String,
    pub order_quote_limit: String,
    pub supported_size_decimals: u8,
    pub supported_price_decimals: u8,
    pub supported_quote_decimals: u8,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OrderBookDetailsResponse {
    pub order_book_details: Vec<OrderBookDetail>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OrderBookDetail {
    pub symbol: String,
    pub market_id: u64,
    pub status: String,
    pub taker_fee: String,
    pub maker_fee: String,
    pub liquidation_fee: String,
    pub min_base_amount: String,
    pub min_quote_amount: String,
    pub order_quote_limit: String,
    pub supported_size_decimals: u8,
    pub supported_price_decimals: u8,
    pub supported_quote_decimals: u8,
    pub size_decimals: u8,
    pub price_decimals: u8,
    pub quote_multiplier: u64,
    pub default_initial_margin_fraction: u64,
    pub min_initial_margin_fraction: u64,
    pub maintenance_margin_fraction: u64,
    pub closeout_margin_fraction: u64,
    pub last_trade_price: f64,
    pub daily_trades_count: u64,
    pub daily_base_token_volume: f64,
    pub daily_quote_token_volume: f64,
    pub daily_price_low: f64,
    pub daily_price_high: f64,
    pub daily_price_change: f64,
    pub open_interest: f64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OrderBookOrdersResponse {
    pub total_bids: u64,
    pub total_asks: u64,
    #[serde(default)]
    pub bids: Vec<Order>,
    #[serde(default)]
    pub asks: Vec<Order>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Order {
    pub order_index: u64,
    pub order_id: String,
    pub owner_account_index: u64,
    pub initial_base_amount: String,
    pub remaining_base_amount: String,
    pub price: String,
    pub order_expiry: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FundingRatesResponse {
    pub funding_rates: Vec<FundingRate>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FundingRate {
    pub market_id: u64,
    pub exchange: String,
    pub symbol: String,
    pub rate: f64,
}
