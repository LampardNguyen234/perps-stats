use serde::Deserialize;

// KuCoin API wrapper structure
#[derive(Debug, Deserialize, Clone)]
pub struct KucoinResponse<T> {
    pub code: String,
    pub data: T,
}

// For /api/v1/contracts/active
#[derive(Debug, Deserialize, Clone)]
pub struct KucoinContract {
    pub symbol: String,
    #[serde(rename = "baseCurrency")]
    pub base_currency: String,
    #[serde(rename = "quoteCurrency")]
    pub quote_currency: String,
    #[serde(rename = "settleCurrency")]
    pub settle_currency: String,
    #[serde(rename = "tickSize")]
    pub tick_size: f64,
    #[serde(rename = "lotSize")]
    pub lot_size: i64,
    #[serde(rename = "maxOrderQty")]
    pub max_order_qty: i64,
    #[serde(rename = "maxLeverage")]
    pub max_leverage: u32,
    #[serde(rename = "initialMargin")]
    pub initial_margin: f64,
    #[serde(rename = "multiplier")]
    pub multiplier: f64,
    pub status: String,
}

// For /api/v1/ticker?symbol=...
#[derive(Debug, Deserialize, Clone)]
pub struct KucoinTicker {
    pub sequence: u64,
    pub symbol: String,
    pub side: String,
    pub size: i64,
    #[serde(rename = "tradeId")]
    pub trade_id: String,
    pub price: String,
    #[serde(rename = "bestBidPrice")]
    pub best_bid_price: String,
    #[serde(rename = "bestBidSize")]
    pub best_bid_size: i64,
    #[serde(rename = "bestAskPrice")]
    pub best_ask_price: String,
    #[serde(rename = "bestAskSize")]
    pub best_ask_size: i64,
    #[serde(rename = "ts")]
    pub timestamp: i64,
}

// For /api/v1/level2/snapshot?symbol=...
#[derive(Debug, Deserialize, Clone)]
pub struct KucoinOrderbook {
    pub symbol: String,
    pub sequence: u64,
    pub asks: Vec<(f64, i64)>,
    pub bids: Vec<(f64, i64)>,
    #[serde(rename = "ts")]
    pub timestamp: i64,
}

// For /api/v1/trade/history?symbol=...
#[derive(Debug, Deserialize, Clone)]
pub struct KucoinTrade {
    pub sequence: u64,
    #[serde(rename = "tradeId")]
    pub trade_id: String,
    #[serde(rename = "takerOrderId")]
    pub taker_order_id: String,
    #[serde(rename = "makerOrderId")]
    pub maker_order_id: String,
    pub price: String,
    pub size: i64,
    pub side: String,
    #[serde(rename = "ts")]
    pub timestamp: i64,
}

// For /api/v1/funding-rate/{symbol}/current
#[derive(Debug, Deserialize, Clone)]
pub struct KucoinFundingRate {
    pub symbol: String,
    #[serde(rename = "granularity")]
    pub granularity: i64,
    #[serde(rename = "timePoint")]
    pub time_point: i64,
    pub value: f64,
    #[serde(rename = "predictedValue")]
    pub predicted_value: Option<f64>,
}

// For /api/v1/interest/query?symbol=...
#[derive(Debug, Deserialize, Clone)]
pub struct KucoinOpenInterest {
    pub symbol: String,
    #[serde(rename = "openInterest")]
    pub open_interest: String,
    #[serde(rename = "timestamp")]
    pub timestamp: i64,
}

// For /api/v1/contract/{symbol} and /api/v1/contracts/active
#[derive(Debug, Deserialize, Clone)]
pub struct KucoinContractDetail {
    pub symbol: String,
    #[serde(rename = "lotSize")]
    pub lot_size: i64,
    #[serde(rename = "tickSize")]
    pub tick_size: f64,
    #[serde(rename = "maxOrderQty")]
    pub max_order_qty: i64,
    pub multiplier: f64,
    #[serde(rename = "maxLeverage")]
    pub max_leverage: u32,
    #[serde(rename = "turnoverOf24h", default)]
    pub turnover_of_24h: Option<f64>,
    #[serde(rename = "volumeOf24h", default)]
    pub volume_of_24h: Option<f64>,
    #[serde(rename = "markPrice", default)]
    pub mark_price: Option<f64>,
    #[serde(rename = "indexPrice", default)]
    pub index_price: Option<f64>,
    #[serde(rename = "lastTradePrice", default)]
    pub last_trade_price: Option<f64>,
    #[serde(rename = "lowPrice", default)]
    pub low_price: Option<f64>,
    #[serde(rename = "highPrice", default)]
    pub high_price: Option<f64>,
    #[serde(rename = "priceChgPct", default)]
    pub price_chg_pct: Option<f64>,
    #[serde(rename = "priceChg", default)]
    pub price_chg: Option<f64>,
    #[serde(rename = "openInterest", default)]
    pub open_interest: Option<String>,
}
