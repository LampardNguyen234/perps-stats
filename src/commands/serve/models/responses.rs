use chrono::{DateTime, Utc};
use serde::{Serialize, Serializer};
use serde_json::Value;

/// Custom serializer for DateTime<Utc> to UNIX timestamp in seconds
fn serialize_timestamp_as_unix<S>(dt: &DateTime<Utc>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_i64(dt.timestamp())
}

/// Custom serializer for Option<DateTime<Utc>> to UNIX timestamp in seconds
fn serialize_optional_timestamp_as_unix<S>(dt: &Option<DateTime<Utc>>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match dt {
        Some(dt) => serializer.serialize_i64(dt.timestamp()),
        None => serializer.serialize_none(),
    }
}

/// Standard paginated response wrapper
#[derive(Debug, Serialize)]
pub struct PaginatedResponse<T> {
    pub data: Vec<T>,
    pub pagination: PaginationMeta,
}

#[derive(Debug, Serialize)]
pub struct PaginationMeta {
    /// Total count (if available, can be expensive to compute)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<i64>,

    /// Limit used for this query
    pub limit: i64,

    /// Offset used for this query
    pub offset: i64,

    /// Actual number of items returned
    pub count: usize,
}

/// Error response structure
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
    pub message: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

/// Health check response
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub database: String,
    #[serde(serialize_with = "serialize_timestamp_as_unix")]
    pub timestamp: DateTime<Utc>,
    pub version: String,
}

/// Database statistics response
#[derive(Debug, Serialize)]
pub struct StatsResponse {
    pub total_tickers: i64,
    pub total_liquidity_depth: i64,
    pub total_orderbooks: i64,
    pub total_trades: i64,
    pub total_klines: i64,
    pub total_funding_rates: i64,
    #[serde(serialize_with = "serialize_optional_timestamp_as_unix")]
    pub oldest_data: Option<DateTime<Utc>>,
    #[serde(serialize_with = "serialize_optional_timestamp_as_unix")]
    pub newest_data: Option<DateTime<Utc>>,
}

/// Exchange information
#[derive(Debug, Serialize)]
pub struct ExchangeInfo {
    pub id: i32,
    pub name: String,
    pub maker_fee: Option<f64>,
    pub taker_fee: Option<f64>,
}

/// Wrapper for optional data that serializes None as empty object {}
#[derive(Debug)]
pub struct OptionalData<T>(pub Option<T>);

impl<T: Serialize> Serialize for OptionalData<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match &self.0 {
            Some(value) => value.serialize(serializer),
            None => {
                // Serialize None as an empty JSON object
                let empty: Value = serde_json::json!({});
                empty.serialize(serializer)
            }
        }
    }
}

/// Simple data wrapper for single item responses
#[derive(Debug, Serialize)]
pub struct DataResponse<T> {
    pub data: T,
    #[serde(serialize_with = "serialize_timestamp_as_unix")]
    pub timestamp: DateTime<Utc>,
}

/// Ticker with exchange information for multi-exchange responses
#[derive(Debug, Serialize)]
pub struct TickerWithExchange {
    pub exchange: String,
    #[serde(flatten)]
    pub ticker: perps_core::Ticker,
}

/// Orderbook with exchange information for multi-exchange responses
#[derive(Debug, Serialize)]
pub struct OrderbookWithExchange {
    pub exchange: String,
    #[serde(flatten)]
    pub orderbook: perps_core::Orderbook,
}
