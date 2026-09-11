//! Response types for EdgeX's public REST API (`https://edgex-prod-v2.edgex.exchange`).
//!
//! Every endpoint wraps its payload in the same `{code,data,msg,errorParam,...}` envelope,
//! but the shape of `data` differs per endpoint: `getMetaData` returns an object, while
//! `getTicker`/`getDepth`/`getLatestFundingRate` return an array even when queried by a
//! single `contractId`. `getKline`/`getFundingRatePage` return a paginated object
//! (`dataList` + `nextPageOffsetData`). All prices/quantities/ids are JSON strings; only
//! booleans (`enableTrade`, `isStock`, ...) and `getDepth`'s `level` are native JSON types.

use serde::Deserialize;

/// Common response envelope wrapping every EdgeX REST payload.
///
/// `data` is `Option` because an API error can carry a null/absent payload alongside a
/// non-`"SUCCESS"` `code`.
#[derive(Debug, Clone, Deserialize)]
pub struct EdgexResponse<T> {
    pub code: String,
    pub data: Option<T>,
    #[serde(default)]
    pub msg: Option<String>,
    #[serde(default, rename = "errorParam")]
    pub error_param: Option<serde_json::Value>,
    /// Envelope response timestamp (milliseconds, as a string). `getDepth` records carry no
    /// event timestamp of their own, so `Orderbook.timestamp` is derived from this field.
    #[serde(default, rename = "responseTime")]
    pub response_time: String,
}

/// `GET /api/v2/public/meta/getMetaData` payload.
#[derive(Debug, Clone, Deserialize)]
pub struct MetadataPayload {
    #[serde(rename = "contractList")]
    pub contract_list: Vec<ContractMeta>,
    #[serde(rename = "coinList")]
    pub coin_list: Vec<CoinMeta>,
}

/// One entry of `MetadataPayload::contract_list`. Field set confirmed live against
/// `edgex-prod-v2.edgex.exchange` on 2026-09-11 (see `docs/plan/edgex_exchange_integration/api_reference.md`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContractMeta {
    pub contract_id: String,
    pub contract_name: String,
    pub base_coin_id: String,
    pub quote_coin_id: String,
    pub tick_size: String,
    pub step_size: String,
    pub min_order_size: String,
    pub max_order_size: String,
    pub default_taker_fee_rate: String,
    pub default_maker_fee_rate: String,
    pub default_leverage: String,
    pub funding_rate_interval_min: String,
    pub funding_min_rate: String,
    pub funding_max_rate: String,
    pub display_max_leverage: String,
    pub enable_trade: bool,
    pub enable_display: bool,
    pub is_stock: bool,
    pub is_fx: bool,
}

/// One entry of `MetadataPayload::coin_list`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoinMeta {
    pub coin_id: String,
    pub coin_name: String,
}

/// `GET /api/v2/public/quote/getTicker` record. The endpoint has no bid/ask fields —
/// callers merge with a `DepthRecord` for top-of-book.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TickerRecord {
    pub contract_id: String,
    pub contract_name: String,
    pub price_change: String,
    pub price_change_percent: String,
    pub size: String,
    pub value: String,
    pub high: String,
    pub low: String,
    pub open: String,
    pub close: String,
    pub end_time: String,
    pub last_price: String,
    pub index_price: String,
    pub oracle_price: String,
    pub mark_price: String,
    pub open_interest: String,
    pub funding_rate: String,
    pub funding_time: String,
    pub next_funding_time: String,
}

/// `GET /api/v2/public/quote/getDepth` record. Carries no event timestamp — callers use
/// the response envelope's `responseTime` instead.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DepthRecord {
    pub contract_id: String,
    pub level: i32,
    pub asks: Vec<BookOrder>,
    pub bids: Vec<BookOrder>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BookOrder {
    pub price: String,
    pub size: String,
}

/// `GET /api/v2/public/quote/getKline` paginated payload.
#[derive(Debug, Clone, Deserialize)]
pub struct PageDataKline {
    #[serde(rename = "dataList")]
    pub data_list: Vec<KlineRecord>,
    #[serde(default, rename = "nextPageOffsetData")]
    pub next_page_offset_data: String,
}

/// One candle. EdgeX exposes no close timestamp — callers derive `close_time` from
/// `kline_time` + the requested interval's duration.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KlineRecord {
    pub kline_time: String,
    pub size: String,
    pub value: String,
    pub high: String,
    pub low: String,
    pub open: String,
    pub close: String,
}

/// `GET /api/v2/public/funding/getLatestFundingRate` record and each row of
/// `GET /api/v2/public/funding/getFundingRatePage` (same shape).
///
/// `forecast_funding_rate` can be an empty string on settlement rows — model as `String`,
/// not `Decimal`, and apply a fallback at conversion time rather than fail to deserialize.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FundingRateRecord {
    pub contract_id: String,
    pub funding_time: String,
    pub funding_rate: String,
    #[serde(default)]
    pub forecast_funding_rate: String,
    #[serde(default)]
    pub funding_rate_interval_min: Option<String>,
    #[serde(default)]
    pub is_settlement: bool,
}

/// `GET /api/v2/public/funding/getFundingRatePage` paginated payload.
#[derive(Debug, Clone, Deserialize)]
pub struct PageDataFundingRate {
    #[serde(rename = "dataList")]
    pub data_list: Vec<FundingRateRecord>,
    #[serde(default, rename = "nextPageOffsetData")]
    pub next_page_offset_data: String,
}
