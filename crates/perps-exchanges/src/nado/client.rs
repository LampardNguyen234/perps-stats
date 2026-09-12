use crate::cache::SymbolsCache;
use crate::nado::types::*;
use crate::nado::ws_client::NadoWsClient;
use crate::nado::GATEWAY_URL;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use perps_core::{
    execute_with_retry, DeltaOrderbookAdapter, FundingRate, IPerps, Kline, Market, MarketStats,
    MultiResolutionOrderbook, OpenInterest, Orderbook, OrderbookStreamer, RateLimiter, RetryConfig,
    Ticker, Trade, WsOrderbookConfig, WsOrderbookManager,
};
use reqwest::Client;
use std::sync::Arc;
use tracing;

const ARCHIVE_URL: &str = "https://archive.prod.nado.xyz/v2";

#[derive(Clone)]
pub struct NadoClient {
    client: Client,
    gateway_url: String,
    archive_url: String,
    symbols_cache: SymbolsCache,
    rate_limiter: Arc<RateLimiter>,
    stream_manager: Option<Arc<WsOrderbookManager>>,
}

impl NadoClient {
    pub fn new() -> Self {
        let stream_manager = if std::env::var("DATABASE_URL").is_ok()
            && std::env::var("ENABLE_ORDERBOOK_STREAMING")
                .map(|value| value.eq_ignore_ascii_case("true"))
                .unwrap_or(false)
        {
            tracing::info!("NadoClient: WebSocket orderbook streaming enabled");
            let ws: Arc<dyn OrderbookStreamer> = Arc::new(NadoWsClient::new());
            Some(Arc::new(WsOrderbookManager::new(
                Arc::new(DeltaOrderbookAdapter(ws)),
                WsOrderbookConfig {
                    with_delta: true,
                    reconnect_delay: std::time::Duration::from_secs(2),
                    ..Default::default()
                },
                vec![],
            )))
        } else {
            None
        };

        Self {
            client: Client::new(),
            gateway_url: GATEWAY_URL.to_string(),
            archive_url: ARCHIVE_URL.to_string(),
            symbols_cache: SymbolsCache::new(),
            rate_limiter: Arc::new(RateLimiter::nado()),
            stream_manager,
        }
    }

    /// Ensure the symbols cache is initialized
    async fn ensure_cache_initialized(&self) -> Result<()> {
        self.symbols_cache
            .get_or_init(|| async {
                let markets = self.get_markets().await?;
                Ok(markets
                    .into_iter()
                    .map(|m| self.parse_symbol(&m.symbol))
                    .collect())
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

    async fn fetch_orderbook_rest(&self, ticker_id: &str, depth: u32) -> Result<(Orderbook, u64)> {
        let url = format!(
            "{}/orderbook?ticker_id={}&depth={}",
            self.gateway_url, ticker_id, depth
        );
        tracing::debug!(%ticker_id, "fetching Nado orderbook");
        let response: OrderbookResponse = self.get(&url).await?;
        // REST timestamp is milliseconds (see conversions::orderbook_response_to_orderbook's
        // timestamp_millis_opt), but the WS book_depth stream's min/max/last_max_timestamp
        // (-> DepthUpdate::first_update_id/final_update_id/previous_id) are nanoseconds.
        // orderbook_manager's Rule 3/4 continuity checks compare this sequence directly against
        // those WS ids, so it must be scaled to nanoseconds or every WS update looks stale/gapped
        // and the stream reconnect-loops forever.
        let sequence = u64::try_from(response.timestamp)
            .context("Nado orderbook timestamp cannot be negative")?
            .checked_mul(1_000_000)
            .context("Nado orderbook timestamp overflowed converting ms to ns")?;
        let orderbook = super::conversions::orderbook_response_to_orderbook(&response)?;
        Ok((orderbook, sequence))
    }
}

impl Default for NadoClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IPerps for NadoClient {
    async fn prewarm_streams(&self, symbols: &[String]) -> Result<()> {
        if let Some(manager) = &self.stream_manager {
            manager
                .prewarm(
                    symbols
                        .iter()
                        .map(|symbol| self.parse_symbol(symbol))
                        .collect(),
                )
                .await?;
        }
        Ok(())
    }

    fn get_name(&self) -> &str {
        "nado"
    }

    fn normalize_symbol(&self, exchange_symbol: &str) -> String {
        // "BTC-PERP_USDT0" -> "BTC" -> unresolve alias
        let upper = exchange_symbol.to_uppercase();
        let base = upper.split("-PERP").next().unwrap_or(&upper);
        crate::symbol_aliases::unresolve_alias("nado", base).to_string()
    }

    fn parse_symbol(&self, symbol: &str) -> String {
        let upper = symbol.to_uppercase();
        if upper.ends_with("-PERP_USDT0") {
            return upper;
        }

        let base = ["-USDT0", "USDT0", "-USDT", "USDT", "-USD", "USD"]
            .into_iter()
            .find_map(|suffix| upper.strip_suffix(suffix))
            .unwrap_or(&upper);
        let base = crate::symbol_aliases::resolve_alias("nado", base);
        format!("{}-PERP_USDT0", base.to_uppercase())
    }

    async fn get_markets(&self) -> Result<Vec<Market>> {
        let url = format!("{}/pairs?market=perp", self.gateway_url);
        tracing::debug!("Fetching markets from Nano: {}", url);

        let pairs: Vec<Pair> = self.get(&url).await?;

        let markets = pairs
            .into_iter()
            .map(|pair| {
                let mut m = super::conversions::pair_to_market(&pair)?;
                m.symbol = self.normalize_symbol(&m.symbol);
                Ok(m)
            })
            .collect::<Result<Vec<Market>>>()?;

        Ok(markets)
    }

    async fn get_market(&self, symbol: &str) -> Result<Market> {
        let ticker_id = self.parse_symbol(symbol);
        let markets = self.get_markets().await?;

        let normalized = self.normalize_symbol(&ticker_id);
        markets
            .into_iter()
            .find(|m| m.symbol == normalized)
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
        let mut ticker = super::conversions::merge_ticker_contract_and_orderbook(
            ticker_data,
            contract_data,
            best_bid,
            best_ask,
        )?;
        ticker.symbol = self.normalize_symbol(&ticker.symbol);
        Ok(ticker)
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
                    Ok(mut ticker) => {
                        ticker.symbol = self.normalize_symbol(&ticker.symbol);
                        result.push(ticker);
                    }
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
        if let Some(manager) = &self.stream_manager {
            manager.subscribe(ticker_id.clone()).await?;
            let client = self.clone();
            let fallback_symbol = ticker_id.clone();
            match manager
                .get_orderbook(&ticker_id, depth, move || {
                    let client = client.clone();
                    async move { client.fetch_orderbook_rest(&fallback_symbol, depth).await }
                })
                .await
            {
                Ok(mut orderbook) => {
                    orderbook.symbol = self.normalize_symbol(&orderbook.symbol);
                    return Ok(MultiResolutionOrderbook::from_single(orderbook));
                }
                Err(error) => {
                    tracing::warn!(%error, "Nado WebSocket orderbook unavailable; falling back to REST");
                }
            }
        }

        let (mut orderbook, _) = self.fetch_orderbook_rest(&ticker_id, depth).await?;
        orderbook.symbol = self.normalize_symbol(&orderbook.symbol);

        Ok(MultiResolutionOrderbook::from_single(orderbook))
    }

    async fn get_funding_rate(&self, symbol: &str) -> Result<FundingRate> {
        let ticker_id = self.parse_symbol(symbol);
        tracing::debug!("Fetching funding rate for {} from Nano", ticker_id);

        let contracts = self.fetch_contracts().await?;
        let contract_data = contracts
            .get(&ticker_id)
            .ok_or_else(|| anyhow!("Contract {} not found", ticker_id))?;

        let mut fr = super::conversions::contract_to_funding_rate(contract_data)?;
        fr.symbol = self.normalize_symbol(&fr.symbol);
        Ok(fr)
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

        let mut oi = super::conversions::contract_to_open_interest(contract_data)?;
        oi.symbol = self.normalize_symbol(&oi.symbol);
        Ok(oi)
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

        let mut stats = super::conversions::contract_to_market_stats(contract_data)?;
        stats.symbol = self.normalize_symbol(&stats.symbol);
        Ok(stats)
    }

    async fn get_all_market_stats(&self) -> Result<Vec<MarketStats>> {
        tracing::debug!("Fetching all market stats from Nano");

        let contracts = self.fetch_contracts().await?;

        let mut result = Vec::new();
        for (ticker_id, contract_data) in contracts.iter() {
            match super::conversions::contract_to_market_stats(contract_data) {
                Ok(mut stats) => {
                    stats.symbol = self.normalize_symbol(&stats.symbol);
                    result.push(stats);
                }
                Err(e) => {
                    tracing::warn!("Failed to convert market stats for {}: {}", ticker_id, e);
                }
            }
        }

        Ok(result)
    }

    async fn is_supported(&self, symbol: &str) -> Result<bool> {
        self.ensure_cache_initialized().await?;
        Ok(self
            .symbols_cache
            .contains(&self.parse_symbol(symbol))
            .await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;

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

    #[tokio::test]
    #[ignore = "requires the live Nado API"]
    async fn streams_reconstructed_live_orderbook() {
        std::env::set_var("DATABASE_URL", "nado-live-test");
        std::env::set_var("ENABLE_ORDERBOOK_STREAMING", "true");
        let client = NadoClient::new();
        std::env::remove_var("DATABASE_URL");
        std::env::remove_var("ENABLE_ORDERBOOK_STREAMING");

        let books = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            client.get_orderbook("BTC", 10),
        )
        .await
        .expect("timed out waiting for reconstructed Nado orderbook")
        .expect("failed to read reconstructed Nado orderbook");
        let book = books.orderbooks.first().expect("missing Nado orderbook");

        assert_eq!(book.symbol, "BTC");
        assert!(!book.bids.is_empty());
        assert!(!book.asks.is_empty());
        assert!(book.bids[0].price > Decimal::ZERO);
        assert!(book.bids[0].price < book.asks[0].price);
    }
}
