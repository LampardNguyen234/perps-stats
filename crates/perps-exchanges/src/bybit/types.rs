use serde::Deserialize;

// Bybit API wrapper structure
#[derive(Debug, Deserialize, Clone)]
pub struct BybitResponse<T> {
    #[serde(rename = "retCode")]
    pub ret_code: i32,
    #[serde(rename = "retMsg")]
    pub ret_msg: String,
    pub result: T,
}

// For /v5/market/instruments-info?category=linear
#[derive(Debug, Deserialize, Clone)]
pub struct InstrumentsResult {
    pub category: String,
    pub list: Vec<BybitInstrument>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct BybitInstrument {
    pub symbol: String,
    #[serde(rename = "contractType")]
    pub contract_type: String,
    pub status: String,
    #[serde(rename = "baseCoin")]
    pub base_coin: String,
    #[serde(rename = "quoteCoin")]
    pub quote_coin: String,
    #[serde(rename = "launchTime")]
    pub launch_time: String,
    #[serde(rename = "deliveryTime")]
    pub delivery_time: String,
    #[serde(rename = "deliveryFeeRate")]
    pub delivery_fee_rate: String,
    #[serde(rename = "priceScale")]
    pub price_scale: String,
    #[serde(rename = "leverageFilter")]
    pub leverage_filter: LeverageFilter,
    #[serde(rename = "priceFilter")]
    pub price_filter: PriceFilter,
    #[serde(rename = "lotSizeFilter")]
    pub lot_size_filter: LotSizeFilter,
    #[serde(rename = "unifiedMarginTrade")]
    pub unified_margin_trade: bool,
    #[serde(rename = "fundingInterval")]
    pub funding_interval: i64,
    #[serde(rename = "settleCoin")]
    pub settle_coin: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LeverageFilter {
    #[serde(rename = "minLeverage")]
    pub min_leverage: String,
    #[serde(rename = "maxLeverage")]
    pub max_leverage: String,
    #[serde(rename = "leverageStep")]
    pub leverage_step: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct PriceFilter {
    #[serde(rename = "minPrice")]
    pub min_price: String,
    #[serde(rename = "maxPrice")]
    pub max_price: String,
    #[serde(rename = "tickSize")]
    pub tick_size: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LotSizeFilter {
    #[serde(rename = "maxOrderQty")]
    pub max_order_qty: String,
    #[serde(rename = "minOrderQty")]
    pub min_order_qty: String,
    #[serde(rename = "qtyStep")]
    pub qty_step: String,
    #[serde(rename = "postOnlyMaxOrderQty")]
    pub post_only_max_order_qty: String,
    #[serde(rename = "maxMktOrderQty")]
    pub max_mkt_order_qty: Option<String>,
    #[serde(rename = "minNotionalValue")]
    pub min_notional_value: String,
}

// For /v5/market/tickers?category=linear
#[derive(Debug, Deserialize, Clone)]
pub struct TickersResult {
    pub category: String,
    pub list: Vec<BybitTicker>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct BybitTicker {
    pub symbol: String,
    #[serde(rename = "lastPrice")]
    pub last_price: String,
    #[serde(rename = "indexPrice")]
    pub index_price: String,
    #[serde(rename = "markPrice")]
    pub mark_price: String,
    #[serde(rename = "prevPrice24h")]
    pub prev_price_24h: String,
    #[serde(rename = "price24hPcnt")]
    pub price_24h_pcnt: String,
    #[serde(rename = "highPrice24h")]
    pub high_price_24h: String,
    #[serde(rename = "lowPrice24h")]
    pub low_price_24h: String,
    #[serde(rename = "prevPrice1h")]
    pub prev_price_1h: String,
    #[serde(rename = "openInterest")]
    pub open_interest: String,
    #[serde(rename = "openInterestValue")]
    pub open_interest_value: String,
    #[serde(rename = "turnover24h")]
    pub turnover_24h: String,
    #[serde(rename = "volume24h")]
    pub volume_24h: String,
    #[serde(rename = "fundingRate")]
    pub funding_rate: String,
    #[serde(rename = "nextFundingTime")]
    pub next_funding_time: String,
    #[serde(rename = "predictedDeliveryPrice")]
    pub predicted_delivery_price: String,
    #[serde(rename = "basisRate")]
    pub basis_rate: String,
    #[serde(rename = "deliveryFeeRate")]
    pub delivery_fee_rate: String,
    #[serde(rename = "deliveryTime")]
    pub delivery_time: String,
    #[serde(rename = "ask1Size")]
    pub ask1_size: String,
    #[serde(rename = "bid1Price")]
    pub bid1_price: String,
    #[serde(rename = "ask1Price")]
    pub ask1_price: String,
    #[serde(rename = "bid1Size")]
    pub bid1_size: String,
    #[serde(rename = "basis")]
    pub basis: String,
}

// For /v5/market/orderbook?category=linear&symbol=...
#[derive(Debug, Deserialize, Clone)]
pub struct OrderbookResult {
    pub s: String, // symbol
    pub b: Vec<(String, String)>, // bids: [price, size]
    pub a: Vec<(String, String)>, // asks: [price, size]
    pub ts: i64, // timestamp
    pub u: i64, // update id
}

// For /v5/market/recent-trade?category=linear&symbol=...
#[derive(Debug, Deserialize, Clone)]
pub struct TradesResult {
    pub category: String,
    pub list: Vec<BybitTrade>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct BybitTrade {
    #[serde(rename = "execId")]
    pub exec_id: String,
    pub symbol: String,
    pub price: String,
    pub size: String,
    pub side: String,
    pub time: String,
    #[serde(rename = "isBlockTrade")]
    pub is_block_trade: bool,
}

// For /v5/market/funding/history?category=linear&symbol=...
#[derive(Debug, Deserialize, Clone)]
pub struct FundingHistoryResult {
    pub category: String,
    pub list: Vec<BybitFundingRate>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct BybitFundingRate {
    pub symbol: String,
    #[serde(rename = "fundingRate")]
    pub funding_rate: String,
    #[serde(rename = "fundingRateTimestamp")]
    pub funding_rate_timestamp: String,
}

// For /v5/market/open-interest?category=linear&symbol=...
#[derive(Debug, Deserialize, Clone)]
pub struct OpenInterestResult {
    pub category: String,
    pub symbol: String,
    pub list: Vec<BybitOpenInterest>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct BybitOpenInterest {
    #[serde(rename = "openInterest")]
    pub open_interest: String,
    pub timestamp: String,
}

// For /v5/market/kline?category=linear&symbol=...
#[derive(Debug, Deserialize, Clone)]
pub struct KlinesResult {
    pub category: String,
    pub symbol: String,
    pub list: Vec<Vec<String>>, // [startTime, openPrice, highPrice, lowPrice, closePrice, volume, turnover]
}
