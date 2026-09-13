use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// StandX encodes numeric fields inconsistently: some endpoints (and even different
/// fields within the same endpoint, e.g. `query_symbol_market`) send a JSON string,
/// others send a native JSON number. Live-verified 2026-09-13 against
/// `https://perps.standx.com/api/*`. Accept either.
fn value_to_decimal(v: &Value) -> Result<Decimal, String> {
    match v {
        Value::String(s) => Decimal::from_str_exact(s)
            .or_else(|_| s.parse::<f64>().ok().and_then(Decimal::from_f64).ok_or(()))
            .map_err(|_| format!("cannot parse {:?} as decimal", s)),
        Value::Number(n) => n
            .as_i64()
            .map(Decimal::from)
            .or_else(|| n.as_f64().and_then(Decimal::from_f64_retain))
            .ok_or_else(|| format!("cannot parse {:?} as decimal", n)),
        other => Err(format!("expected string or number, got {:?}", other)),
    }
}

pub(super) fn de_decimal<'de, D>(deserializer: D) -> Result<Decimal, D::Error>
where
    D: Deserializer<'de>,
{
    let v = Value::deserialize(deserializer)?;
    value_to_decimal(&v).map_err(serde::de::Error::custom)
}

// ---- query_market_overview ----

#[derive(Debug, Deserialize)]
pub struct MarketOverviewResponse {
    pub symbols: Vec<MarketOverviewSymbol>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MarketOverviewSymbol {
    pub base: String,
    pub symbol: String,
    #[serde(deserialize_with = "de_decimal")]
    pub last_price: Decimal,
}

// ---- query_symbol_info (response is an array even for one symbol) ----

#[derive(Debug, Clone, Deserialize)]
pub struct SymbolInfo {
    pub symbol: String,
    #[serde(deserialize_with = "de_decimal")]
    pub min_order_qty: Decimal,
    #[serde(deserialize_with = "de_decimal")]
    pub max_order_qty: Decimal,
    pub price_tick_decimals: i32,
    pub qty_tick_decimals: i32,
    #[serde(deserialize_with = "de_decimal")]
    pub maker_fee: Decimal,
    #[serde(deserialize_with = "de_decimal")]
    pub taker_fee: Decimal,
    #[serde(deserialize_with = "de_decimal")]
    pub max_leverage: Decimal,
    /// Live-verified 2026-09-13: symmetric bound (e.g. BTC "0.000375", XAU "0.00125");
    /// funding_rate_floor is the same magnitude, negated.
    #[serde(deserialize_with = "de_decimal")]
    pub funding_rate_cap: Decimal,
}

// ---- query_symbol_market (per-symbol ticker/stats) ----

#[derive(Debug, Clone, Deserialize)]
pub struct SymbolMarket {
    pub symbol: String,
    #[serde(deserialize_with = "de_decimal")]
    pub last_price: Decimal,
    #[serde(deserialize_with = "de_decimal")]
    pub mark_price: Decimal,
    #[serde(deserialize_with = "de_decimal")]
    pub index_price: Decimal,
    /// `[best_bid_price, best_ask_price]` — not a spread width.
    pub spread: [String; 2],
    #[serde(deserialize_with = "de_decimal")]
    pub high_price_24h: Decimal,
    #[serde(deserialize_with = "de_decimal")]
    pub low_price_24h: Decimal,
    #[serde(deserialize_with = "de_decimal")]
    pub volume_24h: Decimal,
    #[serde(deserialize_with = "de_decimal")]
    pub volume_quote_24h: Decimal,
    #[serde(deserialize_with = "de_decimal")]
    pub open_interest: Decimal,
    #[serde(deserialize_with = "de_decimal")]
    pub open_interest_notional: Decimal,
    #[serde(deserialize_with = "de_decimal")]
    pub funding_rate: Decimal,
    pub next_funding_time: String,
    /// Absolute 24h price change (`last_price - open_price_24h`).
    #[serde(deserialize_with = "de_decimal")]
    pub price_change: Decimal,
    /// Live-verified 2026-09-13: this is a PERCENTAGE (e.g. `0.0621` means 0.0621%),
    /// not a ratio — confirmed by cross-checking BTC-USD's `(last_price -
    /// open_price_24h) / open_price_24h * 100` against this field. Divide by 100
    /// in `conversions.rs` to get the project's decimal-ratio convention.
    #[serde(deserialize_with = "de_decimal")]
    pub price_change_pct: Decimal,
    pub time: String,
}

// ---- query_depth_book ----

#[derive(Debug, Clone, Deserialize)]
pub struct DepthBookResponse {
    pub bids: Vec<[String; 2]>,
    pub asks: Vec<[String; 2]>,
}

// ---- query_recent_trades (array) ----

#[derive(Debug, Clone, Deserialize)]
pub struct RecentTrade {
    #[serde(deserialize_with = "de_decimal")]
    pub price: Decimal,
    #[serde(deserialize_with = "de_decimal")]
    pub qty: Decimal,
    pub is_buyer_taker: bool,
    /// RFC3339 with sub-second precision.
    pub time: String,
}

// ---- query_funding_rates (array); `start_time`/`end_time` are mandatory query params ----

#[derive(Debug, Clone, Deserialize)]
pub struct FundingRateEntry {
    pub id: u64,
    #[serde(deserialize_with = "de_decimal")]
    pub funding_rate: Decimal,
    /// RFC3339.
    pub time: String,
}

// ---- kline/history (columnar / UDF-style) ----

#[derive(Debug, Deserialize)]
pub struct KlineHistoryResponse {
    pub s: String,
    /// Unix seconds.
    pub t: Vec<i64>,
    pub o: Vec<f64>,
    pub h: Vec<f64>,
    pub l: Vec<f64>,
    pub c: Vec<f64>,
    pub v: Vec<f64>,
}
