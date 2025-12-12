use crate::types::*;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

/// IPerps is the core trait that all exchange implementations must implement.
/// It provides a unified interface for accessing perpetual futures market data
/// from different exchanges.
#[async_trait]
pub trait IPerps: Send + Sync {
    /// GetName returns the name of the exchange.
    fn get_name(&self) -> &str;

    /// Parses a global symbol into an exchange-specific symbol.
    /// For example, on Binance, "BTC" might become "BTCUSDT".
    fn parse_symbol(&self, symbol: &str) -> String;

    /// GetMarkets returns all available perpetual markets on the exchange.
    async fn get_markets(&self) -> anyhow::Result<Vec<Market>>;

    /// GetMarket returns detailed information about a specific market.
    async fn get_market(&self, symbol: &str) -> anyhow::Result<Market>;

    /// GetTicker returns the current ticker information for a market.
    async fn get_ticker(&self, symbol: &str) -> anyhow::Result<Ticker>;

    /// GetAllTickers returns ticker information for all markets.
    async fn get_all_tickers(&self) -> anyhow::Result<Vec<Ticker>>;

    /// GetOrderbook returns the order book for a market.
    /// For exchanges with aggregation control (e.g., Hyperliquid), returns multiple resolutions (fine/medium/coarse).
    /// For other exchanges, returns a single orderbook wrapped in MultiResolutionOrderbook (medium field).
    async fn get_orderbook(
        &self,
        symbol: &str,
        depth: u32,
    ) -> anyhow::Result<MultiResolutionOrderbook>;

    /// GetFundingRate returns the current and predicted funding rate for a market.
    async fn get_funding_rate(&self, symbol: &str) -> anyhow::Result<FundingRate>;

    /// GetFundingRateHistory returns historical funding rates for a market.
    async fn get_funding_rate_history(
        &self,
        symbol: &str,
        start_time: Option<DateTime<Utc>>,
        end_time: Option<DateTime<Utc>>,
        limit: Option<u32>,
    ) -> anyhow::Result<Vec<FundingRate>>;

    /// GetOpenInterest returns the current open interest for a market.
    async fn get_open_interest(&self, symbol: &str) -> anyhow::Result<OpenInterest>;

    /// GetKlines returns candlestick/kline data for a market.
    async fn get_klines(
        &self,
        symbol: &str,
        interval: &str,
        start_time: Option<DateTime<Utc>>,
        end_time: Option<DateTime<Utc>>,
        limit: Option<u32>,
    ) -> anyhow::Result<Vec<Kline>>;

    /// GetRecentTrades returns recent public trades for a market.
    async fn get_recent_trades(&self, symbol: &str, limit: u32) -> anyhow::Result<Vec<Trade>>;

    /// GetMarketStats returns aggregated market statistics.
    async fn get_market_stats(&self, symbol: &str) -> anyhow::Result<MarketStats>;

    /// GetAllMarketStats returns aggregated statistics for all markets.
    async fn get_all_market_stats(&self) -> anyhow::Result<Vec<MarketStats>>;

    /// IsSupported checks if a symbol is supported by the exchange.
    async fn is_supported(&self, symbol: &str) -> anyhow::Result<bool>;
}
