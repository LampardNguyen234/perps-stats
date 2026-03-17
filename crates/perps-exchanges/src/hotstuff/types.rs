use serde::{Deserialize, Serialize};

/// Generic POST /info request wrapper
#[derive(Debug, Serialize)]
pub struct InfoRequest<P: Serialize> {
    pub method: &'static str,
    pub params: P,
}

// ---- instruments ----

#[derive(Debug, Clone, Serialize)]
pub struct InstrumentsParams {
    #[serde(rename = "type")]
    pub instrument_type: &'static str, // "perps"
}

#[derive(Debug, Deserialize)]
pub struct InstrumentsResponse {
    pub perps: Option<Vec<HotstuffInstrument>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HotstuffInstrument {
    pub id: i64,
    pub name: String,
    pub lot_size: Option<f64>,
    pub tick_size: Option<f64>,
    pub max_leverage: Option<i64>,
    #[serde(default)]
    pub delisted: bool,
    pub min_notional_usd: Option<f64>,
}

// ---- ticker ----

#[derive(Debug, Clone, Serialize)]
pub struct TickerParams {
    pub symbol: String,
}

/// Hotstuff ticker — all numeric fields come as strings.
#[derive(Debug, Clone, Deserialize)]
pub struct HotstuffTicker {
    pub symbol: String,
    pub mark_price: Option<String>,
    pub mid_price: Option<String>,
    pub index_price: Option<String>,
    pub best_bid_price: Option<String>,
    pub best_ask_price: Option<String>,
    pub best_bid_size: Option<String>,
    pub best_ask_size: Option<String>,
    pub funding_rate: Option<String>,
    pub open_interest: Option<String>,
    pub volume_24h: Option<String>,
    pub change_24h: Option<String>,
    pub max_trading_price: Option<String>, // 24h high
    pub min_trading_price: Option<String>, // 24h low
    pub last_price: Option<String>,
    pub last_updated: Option<i64>, // Unix ms
}

// ---- orderbook ----

#[derive(Debug, Clone, Serialize)]
pub struct OrderbookParams {
    pub symbol: String,
}

#[derive(Debug, Deserialize)]
pub struct HotstuffOrderbook {
    pub instrument_name: String,
    pub bids: Vec<HotstuffLevel>,
    pub asks: Vec<HotstuffLevel>,
    pub timestamp: Option<i64>, // Unix ms
}

#[derive(Debug, Clone, Deserialize)]
pub struct HotstuffLevel {
    pub price: f64,
    pub size: f64,
}

// ---- trades ----

#[derive(Debug, Clone, Serialize)]
pub struct TradesParams {
    pub symbol: String,
}

#[derive(Debug, Deserialize)]
pub struct HotstuffTrade {
    pub instrument: Option<String>,
    /// trade_id can exceed i64::MAX (~8.9e18), must use u64
    pub trade_id: Option<u64>,
    /// "b" = buy, "s" = sell
    pub side: Option<String>,
    pub price: Option<String>,
    pub size: Option<String>,
    /// ISO 8601 timestamp e.g. "2026-02-03T17:00:57.558Z"
    pub timestamp: Option<String>,
}

// ---- chart ----

#[derive(Debug, Clone, Serialize)]
pub struct ChartParams {
    /// Numeric instrument id as string, e.g. "1"
    pub symbol: String,
    pub chart_type: String,
    pub resolution: String,
    /// Unix seconds
    pub from: i64,
    /// Unix milliseconds
    pub to: i64,
}

#[derive(Debug, Deserialize)]
pub struct HotstuffKline {
    pub open: Option<f64>,
    pub high: Option<f64>,
    pub low: Option<f64>,
    pub close: Option<f64>,
    pub volume: Option<f64>,
    /// Unix ms
    pub time: Option<i64>,
}
