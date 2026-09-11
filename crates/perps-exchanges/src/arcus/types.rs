use serde::Deserialize;

// ---- markets ----

/// Response from `GET /v1/markets` (unfiltered or `?market=`-filtered).
#[derive(Debug, Deserialize)]
pub struct ArcusMarketsResponse {
    pub markets: Vec<ArcusMarket>,
}

/// One entry from `GET /v1/markets`.
///
/// Field casing/units verified against a live capture (see `api_reference.md`):
/// - Decimal-valued fields (prices, sizes, rates, volumes) are JSON strings — parse with
///   `Decimal::from_str_exact`, never via `f64`.
/// - `next_funding_at` / `added_timestamp` are Unix **seconds** (unlike every other Arcus
///   timestamp, which is microseconds).
/// - `open_interest_cap_notional` is a configured cap, not the current OI notional — the
///   real notional must be derived as `open_interest * mark_price`.
/// - `regular_trading_hours` / trading-bound / expansion-zone fields are equities/indices-only
///   (null for `CRYPTO` category) and are not consumed by any `perps-core` conversion.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArcusMarket {
    pub market_display_name: String,
    pub full_asset_name: String,
    pub market_id: i64,
    /// `"ONLINE"` or `"OFFLINE"`.
    pub status: String,
    pub base_asset: String,
    pub quote_asset: String,
    pub tick_size: String,
    pub step_size: String,
    #[serde(default)]
    pub tick_tiers: Vec<TickTier>,
    pub min_order_notional: String,
    pub min_order_size: String,
    pub max_order_size: String,
    pub oracle_price: String,
    pub mark_price: String,
    /// Omitted entirely (not just null) on `OFFLINE` markets — required for `ONLINE` ones.
    pub last_trade_price: Option<String>,
    /// Decimal ratio, hourly (e.g. `"0.0000125"`).
    pub funding_rate: String,
    pub next_funding_rate: String,
    /// Unix seconds.
    pub next_funding_at: i64,
    /// Decimal ratio (e.g. `"-0.0185"` = -1.85%), never a percentage or bps value.
    pub price_change_24h: String,
    /// Base-asset quantity.
    pub volume_24h: String,
    /// USD notional.
    pub volume_24h_notional: String,
    /// Omitted entirely (not just null) on `OFFLINE` markets — required for `ONLINE` ones.
    pub high_24h: Option<String>,
    /// Omitted entirely (not just null) on `OFFLINE` markets — required for `ONLINE` ones.
    pub low_24h: Option<String>,
    pub trades_24h: i64,
    /// Base-asset quantity; there is no directly-provided notional counterpart.
    pub open_interest: String,
    /// A configured cap, not the current notional — do not use for `open_interest_notional`.
    pub open_interest_cap_notional: Option<String>,
    pub initial_margin_fraction: String,
    pub maintenance_margin_fraction: String,
    pub off_hours_initial_margin_fraction: Option<String>,
    pub regular_trading_hours: Option<RegularTradingHours>,
    pub is_outside_rth: Option<bool>,
    pub current_settlement_price: Option<String>,
    pub upper_trading_bound: Option<String>,
    pub lower_trading_bound: Option<String>,
    pub next_upper_trading_bound: Option<String>,
    pub next_lower_trading_bound: Option<String>,
    pub is_upper_in_expansion_zone: Option<bool>,
    pub is_lower_in_expansion_zone: Option<bool>,
    pub upper_zone_entered_at: Option<i64>,
    pub upper_expected_expansion_at: Option<i64>,
    pub lower_zone_entered_at: Option<i64>,
    pub lower_expected_expansion_at: Option<i64>,
    /// `"PERPETUAL"` for every market observed live; filter defensively rather than assuming.
    #[serde(rename = "type")]
    pub market_type: String,
    /// `"CRYPTO"`, `"EQUITIES"`, `"COMMODITIES"`, or `"INDICES"`.
    pub category: String,
    /// Unix seconds.
    pub added_timestamp: i64,
    pub asset_resolution: String,
    pub pyth_id: String,
}

/// One price-dependent tick-size tier. The final tier omits `up_to_price` (applies above
/// all listed thresholds). Not fully modeled by `Market.price_scale`, which uses tier 0 only.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TickTier {
    #[serde(default)]
    pub up_to_price: Option<String>,
    pub tick: String,
}

/// Equities/indices-only trading-hours window; always `None` for `CRYPTO` category.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegularTradingHours {
    pub start_seconds_of_day: i64,
    pub end_seconds_of_day: i64,
    pub timezone: String,
    pub is_overnight: bool,
}

// ---- orderbook ----

/// Response from `GET /v1/l2OrderBook/{market}`.
///
/// `bids`/`asks` are `[price, size]` string pairs, already sorted (bids desc, asks asc) live,
/// but callers must still defensively sort per project convention.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderbookSnapshot {
    pub bids: Vec<[String; 2]>,
    pub asks: Vec<[String; 2]>,
    pub last_sequence_id: i64,
    pub global_sequence_id: i64,
    /// Microseconds (unlike `ArcusMarket.next_funding_at`, which is seconds).
    pub timestamp: i64,
}

// ---- funding rate history ----

/// Response from `GET /v1/fundingRates`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FundingRatesResponse {
    pub funding_rates: Vec<ArcusFundingRate>,
}

/// One historical funding rate entry. Newest-first. `time` is microseconds.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArcusFundingRate {
    pub market_id: i64,
    pub market_display_name: String,
    pub funding_rate: String,
    pub time: i64,
}

// ---- candles ----

/// Response from `GET /v1/candles`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandlesResponse {
    pub candles: Vec<ArcusCandle>,
}

/// One OHLCV candle. Newest-first (first entry may be `is_final: false`, i.e. in-progress).
/// `volume` is base-asset quantity, `notional_volume` is USD turnover — both explicit, no
/// derivation needed. `open_time` is microseconds; there is no `closeTime` field.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArcusCandle {
    pub market_display_name: String,
    pub market_id: i64,
    pub timeframe: String,
    pub open_time: i64,
    pub open: String,
    pub high: String,
    pub low: String,
    pub close: String,
    pub volume: String,
    pub notional_volume: String,
    pub trade_count: i64,
    pub is_final: bool,
}

// ---- trades ----

/// Response from `GET /v1/trades`.
#[derive(Debug, Deserialize)]
pub struct TradesResponse {
    pub trades: Vec<ArcusTrade>,
}

/// One public trade. `side` is uppercase `"BUY"`/`"SELL"`. `timestamp` is microseconds.
/// Maker/taker order-id and address fields are not needed by `perps_core::Trade` and are
/// intentionally omitted here (serde ignores unknown JSON keys by default).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArcusTrade {
    pub market_display_name: String,
    pub market_id: i64,
    pub side: String,
    pub price: String,
    pub size: String,
    pub trade_id: String,
    pub timestamp: i64,
}
