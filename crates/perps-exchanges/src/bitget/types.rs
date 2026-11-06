use serde::Deserialize;

/// Bitget API v3 response envelope. `code == "00000"` means success.
#[derive(Debug, Deserialize, Clone)]
pub struct BitgetResponse<T> {
    pub code: String,
    pub msg: String,
    #[serde(rename = "requestTime")]
    pub request_time: i64,
    pub data: T,
}

/// `GET /api/v3/market/instruments`
#[derive(Debug, Deserialize, Clone)]
pub struct Instrument {
    pub symbol: String,
    #[serde(rename = "baseCoin")]
    pub base_coin: String,
    #[serde(rename = "quoteCoin")]
    pub quote_coin: String,
    #[serde(rename = "makerFeeRate")]
    pub maker_fee_rate: String,
    #[serde(rename = "takerFeeRate")]
    pub taker_fee_rate: String,
    #[serde(rename = "minOrderQty")]
    pub min_order_qty: String,
    #[serde(rename = "maxOrderQty")]
    pub max_order_qty: String,
    #[serde(rename = "pricePrecision")]
    pub price_precision: String,
    #[serde(rename = "quantityPrecision")]
    pub quantity_precision: String,
    #[serde(rename = "minOrderAmount")]
    pub min_order_amount: String,
    pub status: String,
    #[serde(rename = "minLeverage")]
    pub min_leverage: String,
    #[serde(rename = "maxLeverage")]
    pub max_leverage: String,
    #[serde(rename = "type")]
    pub inst_type: String,
}

/// `GET /api/v3/market/tickers` — single call carries price, mark/index, funding, and OI.
#[derive(Debug, Deserialize, Clone)]
pub struct TickerData {
    pub symbol: String,
    pub ts: String,
    #[serde(rename = "lastPrice")]
    pub last_price: String,
    #[serde(rename = "openPrice24h")]
    pub open_price_24h: String,
    #[serde(rename = "highPrice24h")]
    pub high_price_24h: String,
    #[serde(rename = "lowPrice24h")]
    pub low_price_24h: String,
    #[serde(rename = "ask1Price")]
    pub ask1_price: String,
    #[serde(rename = "bid1Price")]
    pub bid1_price: String,
    #[serde(rename = "bid1Size")]
    pub bid1_size: String,
    #[serde(rename = "ask1Size")]
    pub ask1_size: String,
    #[serde(rename = "price24hPcnt")]
    pub price_24h_pcnt: String,
    pub volume24h: String,
    pub turnover24h: String,
    #[serde(rename = "indexPrice")]
    pub index_price: String,
    #[serde(rename = "markPrice")]
    pub mark_price: String,
    #[serde(rename = "fundingRate")]
    pub funding_rate: String,
    #[serde(rename = "openInterest")]
    pub open_interest: String,
}

/// `GET /api/v3/market/current-fund-rate` — only `fundingRateInterval` isn't already on
/// `tickers`, so this is called solely to get the interval (hours).
#[derive(Debug, Deserialize, Clone)]
pub struct FundingRateData {
    pub symbol: String,
    #[serde(rename = "fundingRate")]
    pub funding_rate: String,
    #[serde(rename = "fundingRateInterval")]
    pub funding_rate_interval: String,
    #[serde(rename = "nextUpdate")]
    pub next_update: String,
    #[serde(rename = "nextFundingRate")]
    pub next_funding_rate: String,
    #[serde(rename = "maxFundingRate")]
    pub max_funding_rate: String,
}

/// `GET /api/v3/market/fills`
#[derive(Debug, Deserialize, Clone)]
pub struct FillData {
    #[serde(rename = "execId")]
    pub exec_id: String,
    pub price: String,
    pub size: String,
    pub side: String,
    pub ts: String,
}

/// `GET /api/v3/market/history-fund-rate` — wraps its list as `resultList`, unlike every
/// other endpoint here (`data` is the array directly or an object with `list`).
#[derive(Debug, Deserialize, Clone)]
pub struct HistoryFundResult {
    #[serde(rename = "resultList")]
    pub result_list: Vec<HistoryFundEntry>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct HistoryFundEntry {
    pub symbol: String,
    #[serde(rename = "fundingRate")]
    pub funding_rate: String,
    #[serde(rename = "fundingRateTimestamp")]
    pub funding_rate_timestamp: String,
}

/// `GET /api/v3/market/orderbook`
///
/// Live responses return price/qty as raw JSON numbers (not strings, despite the
/// Phase 0 sample) — use `serde_json::Value` to tolerate either encoding.
#[derive(Debug, Deserialize, Clone)]
pub struct OrderbookData {
    pub a: Vec<[serde_json::Value; 2]>,
    pub b: Vec<[serde_json::Value; 2]>,
    pub ts: String,
}
