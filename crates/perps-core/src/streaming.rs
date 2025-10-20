use crate::types::*;
use async_trait::async_trait;
use futures::stream::Stream;
use std::pin::Pin;

/// Type alias for a stream of market data events
pub type DataStream<T> = Pin<Box<dyn Stream<Item = anyhow::Result<T>> + Send>>;

/// Events that can be streamed from exchanges
#[derive(Debug, Clone)]
pub enum StreamEvent {
    Ticker(Ticker),
    Trade(Trade),
    Orderbook(Orderbook),
    FundingRate(FundingRate),
    Kline(Kline),
}

/// Configuration for streaming data from an exchange
#[derive(Debug, Clone)]
pub struct StreamConfig {
    /// Symbols to subscribe to
    pub symbols: Vec<String>,
    /// Types of data to stream
    pub data_types: Vec<StreamDataType>,
    /// Reconnect on disconnect
    pub auto_reconnect: bool,
    /// Kline interval (e.g., "1h", "5m") - only used when streaming klines
    pub kline_interval: Option<String>,
}

/// Types of data that can be streamed
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamDataType {
    Ticker,
    Trade,
    Orderbook,
    FundingRate,
    Kline,
}

/// Trait for exchanges that support real-time streaming via WebSocket
#[async_trait]
pub trait IPerpsStream: Send + Sync {
    /// Get the name of the exchange
    fn get_name(&self) -> &str;

    /// Subscribe to real-time ticker updates for specified symbols
    async fn stream_tickers(&self, symbols: Vec<String>) -> anyhow::Result<DataStream<Ticker>>;

    /// Subscribe to real-time trade updates for specified symbols
    async fn stream_trades(&self, symbols: Vec<String>) -> anyhow::Result<DataStream<Trade>>;

    /// Subscribe to real-time orderbook updates for specified symbols
    async fn stream_orderbooks(
        &self,
        symbols: Vec<String>,
    ) -> anyhow::Result<DataStream<Orderbook>>;

    /// Subscribe to multiple data types for specified symbols
    async fn stream_multi(&self, config: StreamConfig) -> anyhow::Result<DataStream<StreamEvent>>;
}

// ============================================================================
// OrderbookStreamer - Optimized streaming interface for orderbook management
// ============================================================================

/// Standardized depth update from WebSocket with update IDs for continuity checking
#[derive(Debug, Clone)]
pub struct DepthUpdate {
    pub symbol: String,
    pub first_update_id: u64,
    pub final_update_id: u64,
    pub previous_id: u64,
    pub bids: Vec<OrderbookLevel>,
    pub asks: Vec<OrderbookLevel>,
    /// Whether this is a snapshot (true) or delta update (false)
    pub is_snapshot: bool,
}

/// Type alias for depth update stream
pub type DepthUpdateStream = Pin<Box<dyn Stream<Item = anyhow::Result<DepthUpdate>> + Send>>;

/// Trait for exchanges supporting orderbook streaming with OrderbookManager integration
///
/// This trait provides a standardized interface for streaming raw depth updates
/// that can be consumed by OrderbookManager for local orderbook maintenance.
#[async_trait]
pub trait OrderbookStreamer: Send + Sync {
    /// Stream raw depth updates with update IDs for specified symbols
    ///
    /// Returns a stream of DepthUpdate events that include:
    /// - Update IDs for continuity checking
    /// - Previous update ID for gap detection (Binance/Aster `pu` field)
    /// - Bid and ask price levels
    async fn stream_depth_updates(&self, symbols: Vec<String>)
        -> anyhow::Result<DepthUpdateStream>;

    /// Whether this exchange uses incremental delta updates
    ///
    /// Returns:
    /// - `true`: Incremental delta mode - quantities represent changes to apply
    ///   (new_qty = existing_qty + delta_qty, remove if <= 0)
    /// - `false`: Full price mode - quantities are absolute values that replace existing levels
    ///   (new_qty = delta_qty, remove if == 0)
    fn is_incremental_delta(&self) -> bool;

    /// Exchange name (e.g., "aster", "binance", "extended")
    fn exchange_name(&self) -> &str;

    /// WebSocket base URL for this exchange
    fn ws_base_url(&self) -> &str;

    /// Optional: Connection configuration
    fn connection_config(&self) -> ConnectionConfig {
        ConnectionConfig::default()
    }
}

/// WebSocket connection configuration
#[derive(Debug, Clone)]
pub struct ConnectionConfig {
    /// How often to send ping messages
    pub ping_interval: std::time::Duration,

    /// How long to wait for pong response before considering connection dead
    pub pong_timeout: std::time::Duration,

    /// Delay before attempting to reconnect after disconnect
    pub reconnect_delay: std::time::Duration,

    /// Maximum number of reconnection attempts before giving up
    pub max_reconnect_attempts: usize,
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            ping_interval: std::time::Duration::from_secs(20),
            pong_timeout: std::time::Duration::from_secs(10),
            reconnect_delay: std::time::Duration::from_secs(5),
            max_reconnect_attempts: 10,
        }
    }
}
