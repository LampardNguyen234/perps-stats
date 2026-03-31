use serde::Deserialize;

// ── GET /refdata ──────────────────────────────────────────────────────────────

/// Response from `GET /refdata` — all available symbols.
#[derive(Debug, Deserialize)]
pub struct RefdataResponse {
    pub data: Vec<RefDataItem>,
}

/// Metadata for a single QFEX tradeable symbol.
#[derive(Debug, Clone, Deserialize)]
pub struct RefDataItem {
    /// QFEX symbol, e.g. `"NVDA-USD"`.
    pub symbol: String,
    /// Base asset name, e.g. `"NVDA"`.
    pub base_asset: Option<String>,
    /// Latest underlier price (may be empty string in API response).
    pub underlier_price: Option<String>,
    /// 24-hour absolute price change (may be empty string in API response).
    pub price_change_24h: Option<String>,
    /// Minimum price increment, e.g. `"0.01"`.
    pub tick_size: Option<String>,
    /// Minimum order size, e.g. `"1"`.
    pub lot_size: Option<String>,
    /// Maximum allowed leverage.
    pub default_max_leverage: Option<i64>,
    /// Market status, e.g. `"ACTIVE"`.
    pub status: Option<String>,
    /// Product category, e.g. `"EQUITY"`.
    pub product_category: Option<String>,
}

// ── GET /symbols/metrics ─────────────────────────────────────────────────────

/// Response from `GET /symbols/metrics` — metrics for all symbols.
#[derive(Debug, Deserialize)]
pub struct SymbolMetricsResponse {
    pub data: Vec<SymbolMetrics>,
}

/// Metrics for a single QFEX symbol.
///
/// **Important field semantics (verified against live API):**
/// - `current_mark_price`: USD float, direct use.
/// - `volume_24h_usd_notional`: USD turnover (not base volume).
/// - `mark_price_change_24h_pct`: **percentage** (e.g. `0.3563` = +0.3563%) — **divide by 100** to get ratio.
/// - `open_interest`: in **contracts (shares)** — multiply by mark price for notional.
/// - `funding_rate_bps`: decimal ratio **despite the name** (e.g. `-0.0005` = -0.05%) — **use directly**.
///
/// Note: API returns snake_case field names (not camelCase).
#[derive(Debug, Clone, Deserialize)]
pub struct SymbolMetrics {
    /// QFEX symbol, e.g. `"NVDA-USD"`.
    pub symbol: String,
    /// Mark price in USD.
    pub current_mark_price: Option<f64>,
    /// 24h USD notional volume (= turnover_24h in core).
    pub volume_24h_usd_notional: Option<f64>,
    /// 24h price change as a **percentage** (divide by 100 → ratio).
    pub mark_price_change_24h_pct: Option<f64>,
    /// Open interest in **contracts** (shares).
    pub open_interest: Option<f64>,
    /// Funding rate as a **decimal ratio** (despite "bps" suffix in name).
    pub funding_rate_bps: Option<f64>,
}

// ── GET /candles/{symbol} ─────────────────────────────────────────────────────

/// Response from `GET /candles/{symbol}`.
#[derive(Debug, Deserialize)]
pub struct CandlesResponse {
    pub candles: Vec<CandleEntry>,
}

/// One OHLCV candle from QFEX.
///
/// Note: API returns camelCase field names (`startedAt`, `baseTokenVolume`, `usdVolume`).
#[derive(Debug, Clone, Deserialize)]
pub struct CandleEntry {
    /// ISO 8601 candle open time.
    #[serde(rename = "startedAt")]
    pub started_at: Option<String>,
    pub open: Option<String>,
    pub high: Option<String>,
    pub low: Option<String>,
    pub close: Option<String>,
    /// USD notional volume — maps to `turnover` in core.
    #[serde(rename = "usdVolume")]
    pub usd_volume: Option<f64>,
    /// Base-token (share) volume — maps to `volume` in core.
    #[serde(rename = "baseTokenVolume")]
    pub base_token_volume: Option<String>,
    /// Number of trades in interval.
    pub trades: Option<i64>,
}

// ── GET /funding/{symbol} ─────────────────────────────────────────────────────

/// Response from `GET /funding/{symbol}`.
#[derive(Debug, Deserialize)]
pub struct FundingHistoricResponse {
    pub data: Vec<FundingPoint>,
    pub count: Option<i64>,
}

/// One historical funding rate entry.
///
/// `rate` is a decimal ratio (same convention as `funding_rate_bps` in SymbolMetrics).
#[derive(Debug, Clone, Deserialize)]
pub struct FundingPoint {
    pub symbol: Option<String>,
    pub interval_minutes: Option<i64>,
    /// ISO 8601 window start time.
    pub window_start: Option<String>,
    /// Funding rate as a decimal ratio (use directly).
    pub rate: Option<f64>,
}
