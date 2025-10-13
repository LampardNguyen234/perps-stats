use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Represents market depth at a specific percentage level from mid-price
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepthLevel {
    /// Percentage from mid-price (e.g., 0.005 for 0.5%)
    pub percentage: Decimal,
    /// Cumulative quantity of bids within this percentage
    pub bid_volume: Decimal,
    /// Cumulative quantity of asks within this percentage
    pub ask_volume: Decimal,
}

/// Market depth analysis showing liquidity at different price levels
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketDepth {
    pub symbol: String,
    pub timestamp: DateTime<Utc>,
    pub levels: Vec<DepthLevel>,
}

/// Funding rate statistics calculated over a time period
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FundingRateStats {
    pub symbol: String,
    pub exchange: String,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    /// Average funding rate over the period
    pub average_rate: Decimal,
    /// Minimum funding rate in the period
    pub min_rate: Decimal,
    /// Maximum funding rate in the period
    pub max_rate: Decimal,
    /// Standard deviation of funding rates
    pub std_dev: Decimal,
    /// Number of funding rate observations
    pub count: usize,
    /// Trend: positive if increasing, negative if decreasing, zero if stable
    pub trend: Decimal,
}
