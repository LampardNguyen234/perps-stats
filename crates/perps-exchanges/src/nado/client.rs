use crate::cache::SymbolsCache;
use crate::nado::types::*;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use perps_core::{
    execute_with_retry, FundingRate, IPerps, Kline, Market, MarketStats, MultiResolutionOrderbook,
    OpenInterest, RateLimiter, RetryConfig, Ticker, Trade,
};
use reqwest::Client;
use std::sync::Arc;
use tracing;

const GATEWAY_URL: &str = "https://gateway.prod.nado.xyz/v2";
const ARCHIVE_URL: &str = "https://archive.prod.nado.xyz/v2";

pub struct NadoClient {
    client: Client,
    gateway_url: String,
    archive_url: String,
    symbols_cache: SymbolsCache,
    rate_limiter: Arc<RateLimiter>,
}

impl NadoClient {
    pub fn new() -> Self {
        Self {
            client: Client::new(),
            gateway_url: GATEWAY_URL.to_string(),
            archive_url: ARCHIVE_URL.to_string(),
            symbols_cache: SymbolsCache::new(),
            rate_limiter: Arc::new(RateLimiter::nado()),
        }
    }

    /// Ensure the symbols cache is initialized
    async fn ensure_cache_initialized(&self) -> Result<()> {
        self.symbols_cache
            .get_or_init(|| async {
                let markets = self.get_markets().await?;
                Ok(markets.into_iter().map(|m| m.symbol).collect())
            })
            .await
    }

    /// Helper method to make rate-limited GET requests with retry
    async fn get<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        let config = RetryConfig::default();
        let url = url.to_string();
        let client = self.client.clone();
        let rate_limiter = self.rate_limiter.clone();

        execute_with_retry(&config, || {
            let url = url.clone();
            let client = client.clone();
            let rate_limiter = rate_limiter.clone();
            async move {
                rate_limiter
                    .execute(|| {
                        let url = url.clone();
                        let client = client.clone();
                        async move {
                            tracing::trace!("Requesting: {}", url);
                            let response = client.get(&url).send().await?;

                            // Check HTTP status first
                            if !response.status().is_success() {
                                let status = response.status();
                                let text = response
                                    .text()
                                    .await
                                    .unwrap_or_else(|_| "Unable to read response body".to_string());
                                return Err(anyhow!("HTTP {}: {}", status, text));
                            }

                            // Try to decode as the expected type
                            let data = response.json::<T>().await?;
                            Ok(data)
                        }
                    })
                    .await
            }
        })
        .await
    }

    /// Fetch tickers from Archive API
    async fn fetch_tickers(&self) -> Result<TickersResponse> {
        let url = format!("{}/tickers?market=perp&edge=true", self.archive_url);
        tracing::debug!("Fetching tickers from Nano: {}", url);
        self.get(&url).await
    }

    /// Fetch contracts from Archive API
    async fn fetch_contracts(&self) -> Result<ContractsResponse> {
        let url = format!("{}/contracts?edge=true", self.archive_url);
        tracing::debug!("Fetching contracts from Nano: {}", url);
        self.get(&url).await
    }
}

impl Default for NadoClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IPerps for NadoClient {
    fn get_name(&self) -> &str {
        "nano"
    }

    fn parse_symbol(&self, symbol: &str) -> String {
        // Nado uses ticker_id format: "BTC-PERP_USDT0"
        // If already in correct format, return as-is
        if symbol.contains("-PERP_USDT0") {
            return symbol.to_uppercase();
        }

        // Convert "BTC" → "BTC-PERP_USDT0"
        // Convert "BTCUSDT" → "BTC-PERP_USDT0"
        let base = symbol
            .to_uppercase()
            .trim_end_matches("USDT")
            .trim_end_matches("-USDT")
            .trim_end_matches("USD")
            .trim_end_matches("-USD")
            .to_string();
        format!("{}-PERP_USDT0", base)
    }

    async fn get_markets(&self) -> Result<Vec<Market>> {
        let url = format!("{}/pairs?market=perp", self.gateway_url);
        tracing::debug!("Fetching markets from Nano: {}", url);

        let pairs: Vec<Pair> = self.get(&url).await?;

        let markets = pairs
            .into_iter()
            .map(|pair| super::conversions::pair_to_market(&pair))
            .collect::<Result<Vec<Market>>>()?;

        Ok(markets)
    }

    async fn get_market(&self, symbol: &str) -> Result<Market> {
        let ticker_id = self.parse_symbol(symbol);
        let markets = self.get_markets().await?;

        markets
            .into_iter()
            .find(|m| m.symbol == ticker_id)
            .ok_or_else(|| anyhow!("Market {} not found", ticker_id))
    }

    async fn get_ticker(&self, symbol: &str) -> Result<Ticker> {
        let ticker_id = self.parse_symbol(symbol);
        tracing::debug!("Fetching ticker for {} from Nado", ticker_id);

        // Fetch tickers, contracts, and orderbook (depth 1 for best bid/ask)
        let (tickers, contracts, orderbook) = tokio::try_join!(
            self.fetch_tickers(),
            self.fetch_contracts(),
            self.get_orderbook(&ticker_id, 1)
        )?;

        // Find the ticker data
        let ticker_data = tickers
            .get(&ticker_id)
            .ok_or_else(|| anyhow!("Ticker {} not found", ticker_id))?;

        let contract_data = contracts
            .get(&ticker_id)
            .ok_or_else(|| anyhow!("Contract {} not found", ticker_id))?;

        // Extract best bid/ask from orderbook
        let best_bid = orderbook.orderbooks.first().and_then(|ob| ob.bids.first());
        let best_ask = orderbook.orderbooks.first().and_then(|ob| ob.asks.first());

        // Convert to perps_core::Ticker with bid/ask data
        super::conversions::merge_ticker_contract_and_orderbook(
            ticker_data,
            contract_data,
            best_bid,
            best_ask,
        )
    }

    async fn get_all_tickers(&self) -> Result<Vec<Ticker>> {
        tracing::debug!("Fetching all tickers from Nado");

        // Fetch both tickers and contracts data
        let (tickers, contracts) = tokio::try_join!(self.fetch_tickers(), self.fetch_contracts())?;

        // Merge data for all tickers (using legacy method without orderbook for bulk operations)
        // Note: For performance, bulk ticker fetches don't include best bid/ask
        // Use get_ticker() for individual symbols if bid/ask data is needed
        let mut result = Vec::new();
        for (ticker_id, ticker_data) in tickers.iter() {
            if let Some(contract_data) = contracts.get(ticker_id) {
                match super::conversions::merge_ticker_and_contract(ticker_data, contract_data) {
                    Ok(ticker) => result.push(ticker),
                    Err(e) => {
                        tracing::warn!("Failed to convert ticker {}: {}", ticker_id, e);
                    }
                }
            }
        }

        Ok(result)
    }

    async fn get_orderbook(&self, symbol: &str, depth: u32) -> Result<MultiResolutionOrderbook> {
        let ticker_id = self.parse_symbol(symbol);
        let url = format!(
            "{}/orderbook?ticker_id={}&depth={}",
            self.gateway_url, ticker_id, depth
        );
        tracing::debug!("Fetching orderbook for {} from Nano", ticker_id);

        let orderbook_response: OrderbookResponse = self.get(&url).await?;

        // Convert to perps_core::Orderbook
        let orderbook = super::conversions::orderbook_response_to_orderbook(&orderbook_response)?;

        Ok(MultiResolutionOrderbook::from_single(orderbook))
    }

    async fn get_funding_rate(&self, symbol: &str) -> Result<FundingRate> {
        let ticker_id = self.parse_symbol(symbol);
        tracing::debug!("Fetching funding rate for {} from Nano", ticker_id);

        let contracts = self.fetch_contracts().await?;
        let contract_data = contracts
            .get(&ticker_id)
            .ok_or_else(|| anyhow!("Contract {} not found", ticker_id))?;

        super::conversions::contract_to_funding_rate(contract_data)
    }

    async fn get_funding_rate_history(
        &self,
        _symbol: &str,
        _start_time: Option<DateTime<Utc>>,
        _end_time: Option<DateTime<Utc>>,
        _limit: Option<u32>,
    ) -> Result<Vec<FundingRate>> {
        // Not supported by Nano API yet
        Err(anyhow!(
            "get_funding_rate_history is not supported by Nano exchange"
        ))
    }

    async fn get_open_interest(&self, symbol: &str) -> Result<OpenInterest> {
        let ticker_id = self.parse_symbol(symbol);
        tracing::debug!("Fetching open interest for {} from Nano", ticker_id);

        let contracts = self.fetch_contracts().await?;
        let contract_data = contracts
            .get(&ticker_id)
            .ok_or_else(|| anyhow!("Contract {} not found", ticker_id))?;

        super::conversions::contract_to_open_interest(contract_data)
    }

    async fn get_klines(
        &self,
        _symbol: &str,
        _interval: &str,
        _start_time: Option<DateTime<Utc>>,
        _end_time: Option<DateTime<Utc>>,
        _limit: Option<u32>,
    ) -> Result<Vec<Kline>> {
        // Not supported by Nano API yet
        Err(anyhow!("get_klines is not supported by Nano exchange"))
    }

    async fn get_recent_trades(&self, _symbol: &str, _limit: u32) -> Result<Vec<Trade>> {
        // TODO: Implement using /trades endpoint
        Err(anyhow!(
            "get_recent_trades is not yet implemented for Nano exchange"
        ))
    }

    async fn get_market_stats(&self, symbol: &str) -> Result<MarketStats> {
        let ticker_id = self.parse_symbol(symbol);
        tracing::debug!("Fetching market stats for {} from Nano", ticker_id);

        let contracts = self.fetch_contracts().await?;
        let contract_data = contracts
            .get(&ticker_id)
            .ok_or_else(|| anyhow!("Contract {} not found", ticker_id))?;

        super::conversions::contract_to_market_stats(contract_data)
    }

    async fn get_all_market_stats(&self) -> Result<Vec<MarketStats>> {
        tracing::debug!("Fetching all market stats from Nano");

        let contracts = self.fetch_contracts().await?;

        let mut result = Vec::new();
        for (ticker_id, contract_data) in contracts.iter() {
            match super::conversions::contract_to_market_stats(contract_data) {
                Ok(stats) => result.push(stats),
                Err(e) => {
                    tracing::warn!("Failed to convert market stats for {}: {}", ticker_id, e);
                }
            }
        }

        Ok(result)
    }

    async fn is_supported(&self, symbol: &str) -> Result<bool> {
        self.ensure_cache_initialized().await?;
        let ticker_id = self.parse_symbol(symbol);
        Ok(self.symbols_cache.contains(&ticker_id).await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_symbol_standard() {
        let client = NadoClient::new();
        assert_eq!(client.parse_symbol("BTC"), "BTC-PERP_USDT0");
        assert_eq!(client.parse_symbol("BTCUSDT"), "BTC-PERP_USDT0");
        assert_eq!(client.parse_symbol("ETH"), "ETH-PERP_USDT0");
    }

    #[test]
    fn test_parse_symbol_already_formatted() {
        let client = NadoClient::new();
        assert_eq!(client.parse_symbol("BTC-PERP_USDT0"), "BTC-PERP_USDT0");
        assert_eq!(client.parse_symbol("ETH-PERP_USDT0"), "ETH-PERP_USDT0");
        assert_eq!(client.parse_symbol("sol-perp_usdt0"), "SOL-PERP_USDT0");
    }

    #[test]
    fn test_parse_symbol_with_hyphens() {
        let client = NadoClient::new();
        assert_eq!(client.parse_symbol("BTC-USDT"), "BTC-PERP_USDT0");
        assert_eq!(client.parse_symbol("BTC-USD"), "BTC-PERP_USDT0");
    }

    #[test]
    fn test_parse_symbol_lowercase() {
        let client = NadoClient::new();
        assert_eq!(client.parse_symbol("btc"), "BTC-PERP_USDT0");
        assert_eq!(client.parse_symbol("eth-usdt"), "ETH-PERP_USDT0");
    }

    #[test]
    fn test_get_name() {
        let client = NadoClient::new();
        assert_eq!(client.get_name(), "nado");
    }
}
