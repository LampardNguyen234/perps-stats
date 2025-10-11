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
}

/// Types of data that can be streamed
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamDataType {
    Ticker,
    Trade,
    Orderbook,
    FundingRate,
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
    async fn stream_orderbooks(&self, symbols: Vec<String>) -> anyhow::Result<DataStream<Orderbook>>;

    /// Subscribe to multiple data types for specified symbols
    async fn stream_multi(&self, config: StreamConfig) -> anyhow::Result<DataStream<StreamEvent>>;
}
