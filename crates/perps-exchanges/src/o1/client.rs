use super::conversions::*;
use super::types::*;
use crate::cache::SymbolsCache;
use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use perps_core::{
    execute_with_retry, FundingRate, IPerps, Kline, Market, MarketStats, MultiResolutionOrderbook,
    OpenInterest, RateLimiter, RetryConfig, Ticker, Trade,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

const BASE_URL: &str = "https://zo-mainnet.n1.xyz";
const STATS_CACHE_TTL: Duration = Duration::from_secs(5);
const ORDERBOOK_CACHE_TTL: Duration = Duration::from_secs(5);
const MARKETS_CACHE_TTL: Duration = Duration::from_secs(300);

// ---------------------------------------------------------------------------
// ResponseCache<T> — generic TTL cache keyed by market_id (u32)
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct CacheEntry<T: Clone> {
    data: T,
    fetched_at: Instant,
}

#[derive(Debug)]
struct ResponseCache<T: Clone> {
    entries: RwLock<HashMap<u32, CacheEntry<T>>>,
    ttl: Duration,
}

impl<T: Clone> ResponseCache<T> {
    fn new(ttl: Duration) -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            ttl,
        }
    }

    async fn get(&self, key: u32) -> Option<T> {
        let entries = self.entries.read().await;
        entries.get(&key).and_then(|entry| {
            if entry.fetched_at.elapsed() < self.ttl {
                Some(entry.data.clone())
            } else {
                None
            }
        })
    }

    async fn set(&self, key: u32, data: T) {
        let mut entries = self.entries.write().await;
        entries.insert(
            key,
            CacheEntry {
                data,
                fetched_at: Instant::now(),
            },
        );
    }
}

// ---------------------------------------------------------------------------
// MarketEntry — lightweight cached market metadata
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct MarketEntry {
    market_id: u32,
    symbol: String,        // API symbol e.g. "BTCUSD"
    global_symbol: String, // Global symbol e.g. "BTC"
}

// ---------------------------------------------------------------------------
// MarketsData — full cached market data with TTL
// ---------------------------------------------------------------------------

struct MarketsData {
    entries: HashMap<String, MarketEntry>, // keyed by API symbol "BTCUSD"
    raw_markets: Vec<NordMarketInfo>,      // raw market info for conversion
    fetched_at: Instant,
}

// ---------------------------------------------------------------------------
// O1Client — 01.xyz (Nord) exchange REST client
// ---------------------------------------------------------------------------

/// 01.xyz (Nord) exchange REST client for perpetual futures market data.
///
/// Implements the [`IPerps`] trait for the Nord perpetual futures DEX on Solana.
/// All endpoints use GET requests against the Nord mainnet API.
///
/// Rate limit: 20 requests per second (conservative estimate).
pub struct O1Client {
    http: reqwest::Client,
    base_url: String,
    symbols_cache: SymbolsCache,
    markets: Arc<RwLock<Option<MarketsData>>>,
    stats_cache: Arc<ResponseCache<NordMarketStats>>,
    orderbook_cache: Arc<ResponseCache<NordOrderbookInfo>>,
    rate_limiter: Arc<RateLimiter>,
}

impl O1Client {
    /// Create a new O1Client with default configuration.
    ///
    /// # Returns
    /// A new O1Client instance ready to make API requests.
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: BASE_URL.to_string(),
            symbols_cache: SymbolsCache::new(),
            markets: Arc::new(RwLock::new(None)),
            stats_cache: Arc::new(ResponseCache::new(STATS_CACHE_TTL)),
            orderbook_cache: Arc::new(ResponseCache::new(ORDERBOOK_CACHE_TTL)),
            rate_limiter: Arc::new(RateLimiter::o1()),
        }
    }

    /// Convert a global symbol to the Nord API symbol format (idempotent).
    ///
    /// - `"BTC"` -> `"BTCUSD"`
    /// - `"BTCUSD"` -> `"BTCUSD"` (already in correct format)
    fn to_api_symbol(&self, symbol: &str) -> String {
        let upper = symbol.to_uppercase();
        if upper.ends_with("USD") {
            upper
        } else {
            format!("{}USD", upper)
        }
    }

    /// Convert a Nord API symbol back to a global symbol.
    ///
    /// - `"BTCUSD"` -> `"BTC"`
    /// - `"ETHUSD"` -> `"ETH"`
    fn denormalize_symbol(api_symbol: &str) -> String {
        api_symbol
            .strip_suffix("USD")
            .unwrap_or(api_symbol)
            .to_string()
    }

    /// Make a rate-limited GET request with retry logic.
    async fn get_request<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let config = RetryConfig::default();
        let url = format!("{}{}", self.base_url, path);
        let client = self.http.clone();
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
                            tracing::trace!("01 GET request: {}", url);
                            let response = client.get(&url).send().await?;

                            if !response.status().is_success() {
                                let status = response.status();
                                let text = response
                                    .text()
                                    .await
                                    .unwrap_or_else(|_| "Unable to read response body".to_string());
                                return Err(anyhow::anyhow!("HTTP {}: {}", status, text));
                            }

                            let data = response.json::<T>().await?;
                            Ok(data)
                        }
                    })
                    .await
            }
        })
        .await
    }

    /// Ensure markets data is loaded and fresh. If stale or absent, re-fetch from API.
    async fn ensure_markets(&self) -> Result<()> {
        {
            let guard = self.markets.read().await;
            if let Some(ref data) = *guard {
                if data.fetched_at.elapsed() < MARKETS_CACHE_TTL {
                    return Ok(());
                }
            }
        }

        tracing::debug!("Fetching 01 markets info");
        let info: NordMarketsInfo = self.get_request("/info").await?;

        let mut entries = HashMap::new();
        let mut symbol_set = std::collections::HashSet::new();

        for m in &info.markets {
            let global_symbol = Self::denormalize_symbol(&m.symbol);
            let entry = MarketEntry {
                market_id: m.market_id,
                symbol: m.symbol.clone(),
                global_symbol: global_symbol.clone(),
            };
            entries.insert(m.symbol.clone(), entry);
            symbol_set.insert(m.symbol.clone());
        }

        self.symbols_cache.initialize(symbol_set);

        let mut guard = self.markets.write().await;
        *guard = Some(MarketsData {
            entries,
            raw_markets: info.markets,
            fetched_at: Instant::now(),
        });

        Ok(())
    }

    /// Resolve a symbol (global or API format) to its market_id.
    async fn resolve_market_id(&self, symbol: &str) -> Result<u32> {
        self.ensure_markets().await?;
        let api_symbol = self.to_api_symbol(symbol);

        let guard = self.markets.read().await;
        let data = guard.as_ref().context("markets not initialized")?;

        data.entries
            .get(&api_symbol)
            .map(|e| e.market_id)
            .ok_or_else(|| {
                anyhow::anyhow!("Market not found for symbol: {} ({})", symbol, api_symbol)
            })
    }

    /// Fetch market stats for a given market_id, using the TTL cache.
    async fn fetch_market_stats_cached(&self, market_id: u32) -> Result<NordMarketStats> {
        if let Some(cached) = self.stats_cache.get(market_id).await {
            return Ok(cached);
        }

        let path = format!("/market/{}/stats", market_id);
        let stats: NordMarketStats = self.get_request(&path).await?;
        self.stats_cache.set(market_id, stats.clone()).await;
        Ok(stats)
    }

    /// Fetch orderbook for a given market_id, using the TTL cache.
    async fn fetch_orderbook_cached(&self, market_id: u32) -> Result<NordOrderbookInfo> {
        if let Some(cached) = self.orderbook_cache.get(market_id).await {
            return Ok(cached);
        }

        let path = format!("/market/{}/orderbook", market_id);
        let ob: NordOrderbookInfo = self.get_request(&path).await?;
        self.orderbook_cache.set(market_id, ob.clone()).await;
        Ok(ob)
    }
}

impl Default for O1Client {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IPerps for O1Client {
    fn get_name(&self) -> &str {
        "01"
    }

    fn parse_symbol(&self, symbol: &str) -> String {
        self.to_api_symbol(symbol)
    }

    async fn get_markets(&self) -> Result<Vec<Market>> {
        self.ensure_markets().await?;
        let guard = self.markets.read().await;
        let data = guard.as_ref().context("markets not initialized")?;

        Ok(data.raw_markets.iter().map(nord_market_to_market).collect())
    }

    async fn get_market(&self, symbol: &str) -> Result<Market> {
        self.ensure_markets().await?;
        let api_symbol = self.to_api_symbol(symbol);

        let guard = self.markets.read().await;
        let data = guard.as_ref().context("markets not initialized")?;

        data.raw_markets
            .iter()
            .find(|m| m.symbol == api_symbol)
            .map(nord_market_to_market)
            .ok_or_else(|| anyhow::anyhow!("Market not found: {}", api_symbol))
    }

    async fn get_ticker(&self, symbol: &str) -> Result<Ticker> {
        let market_id = self.resolve_market_id(symbol).await?;
        let global_symbol = Self::denormalize_symbol(&self.to_api_symbol(symbol));

        let stats = self.fetch_market_stats_cached(market_id).await?;
        let ob = self.fetch_orderbook_cached(market_id).await?;

        nord_stats_and_orderbook_to_ticker(&stats, &ob, global_symbol)
    }

    async fn get_all_tickers(&self) -> Result<Vec<Ticker>> {
        self.ensure_markets().await?;

        let market_entries: Vec<(u32, String)> = {
            let guard = self.markets.read().await;
            let data = guard.as_ref().context("markets not initialized")?;
            data.entries
                .values()
                .map(|e| (e.market_id, e.global_symbol.clone()))
                .collect()
        };

        let futures: Vec<_> = market_entries
            .into_iter()
            .map(|(market_id, global_symbol)| {
                let stats_cache = self.stats_cache.clone();
                let ob_cache = self.orderbook_cache.clone();
                let rate_limiter = self.rate_limiter.clone();
                let http = self.http.clone();
                let base_url = self.base_url.clone();

                async move {
                    // Fetch stats
                    let stats = if let Some(cached) = stats_cache.get(market_id).await {
                        cached
                    } else {
                        let url = format!("{}/market/{}/stats", base_url, market_id);
                        let config = RetryConfig::default();
                        let client = http.clone();
                        let rl = rate_limiter.clone();

                        let stats: NordMarketStats = execute_with_retry(&config, || {
                            let url = url.clone();
                            let client = client.clone();
                            let rl = rl.clone();
                            async move {
                                rl.execute(|| {
                                    let url = url.clone();
                                    let client = client.clone();
                                    async move {
                                        let resp = client.get(&url).send().await?;
                                        if !resp.status().is_success() {
                                            let status = resp.status();
                                            let text = resp.text().await.unwrap_or_default();
                                            return Err(anyhow::anyhow!(
                                                "HTTP {}: {}",
                                                status,
                                                text
                                            ));
                                        }
                                        Ok(resp.json().await?)
                                    }
                                })
                                .await
                            }
                        })
                        .await?;
                        stats_cache.set(market_id, stats.clone()).await;
                        stats
                    };

                    // Fetch orderbook
                    let ob = if let Some(cached) = ob_cache.get(market_id).await {
                        cached
                    } else {
                        let url = format!("{}/market/{}/orderbook", base_url, market_id);
                        let config = RetryConfig::default();
                        let client = http.clone();
                        let rl = rate_limiter.clone();

                        let ob: NordOrderbookInfo = execute_with_retry(&config, || {
                            let url = url.clone();
                            let client = client.clone();
                            let rl = rl.clone();
                            async move {
                                rl.execute(|| {
                                    let url = url.clone();
                                    let client = client.clone();
                                    async move {
                                        let resp = client.get(&url).send().await?;
                                        if !resp.status().is_success() {
                                            let status = resp.status();
                                            let text = resp.text().await.unwrap_or_default();
                                            return Err(anyhow::anyhow!(
                                                "HTTP {}: {}",
                                                status,
                                                text
                                            ));
                                        }
                                        Ok(resp.json().await?)
                                    }
                                })
                                .await
                            }
                        })
                        .await?;
                        ob_cache.set(market_id, ob.clone()).await;
                        ob
                    };

                    nord_stats_and_orderbook_to_ticker(&stats, &ob, global_symbol)
                }
            })
            .collect();

        let results = futures::future::join_all(futures).await;

        let mut tickers = Vec::new();
        for result in results {
            match result {
                Ok(ticker) => tickers.push(ticker),
                Err(e) => {
                    tracing::warn!("Failed to fetch ticker for 01 market: {}", e);
                }
            }
        }

        Ok(tickers)
    }

    async fn get_orderbook(&self, symbol: &str, _depth: u32) -> Result<MultiResolutionOrderbook> {
        let market_id = self.resolve_market_id(symbol).await?;
        let global_symbol = Self::denormalize_symbol(&self.to_api_symbol(symbol));

        let ob = self.fetch_orderbook_cached(market_id).await?;
        let orderbook = nord_orderbook_to_orderbook(&ob, global_symbol.clone());
        let timestamp = orderbook.timestamp;

        Ok(MultiResolutionOrderbook {
            symbol: global_symbol,
            timestamp,
            orderbooks: vec![orderbook],
        })
    }

    async fn get_funding_rate(&self, symbol: &str) -> Result<FundingRate> {
        let market_id = self.resolve_market_id(symbol).await?;
        let global_symbol = Self::denormalize_symbol(&self.to_api_symbol(symbol));

        let stats = self.fetch_market_stats_cached(market_id).await?;
        nord_stats_to_funding_rate(&stats, global_symbol)
    }

    async fn get_funding_rate_history(
        &self,
        symbol: &str,
        _start_time: Option<DateTime<Utc>>,
        _end_time: Option<DateTime<Utc>>,
        _limit: Option<u32>,
    ) -> Result<Vec<FundingRate>> {
        // Nord API does not support historical funding rate queries.
        // Return the current funding rate as a single-element vector for compatibility.
        let funding_rate = self.get_funding_rate(symbol).await?;
        Ok(vec![funding_rate])
    }

    async fn get_open_interest(&self, symbol: &str) -> Result<OpenInterest> {
        let market_id = self.resolve_market_id(symbol).await?;
        let global_symbol = Self::denormalize_symbol(&self.to_api_symbol(symbol));

        let stats = self.fetch_market_stats_cached(market_id).await?;
        nord_stats_to_open_interest(&stats, global_symbol)
    }

    async fn get_klines(
        &self,
        symbol: &str,
        interval: &str,
        _start_time: Option<DateTime<Utc>>,
        _end_time: Option<DateTime<Utc>>,
        limit: Option<u32>,
    ) -> Result<Vec<Kline>> {
        // Nord only supports 1h klines via /market/{id}/history/PT1H
        if interval != "1h" {
            anyhow::bail!(
                "Unsupported kline interval for 01: {}. Only \"1h\" is supported.",
                interval
            );
        }

        let market_id = self.resolve_market_id(symbol).await?;
        let global_symbol = Self::denormalize_symbol(&self.to_api_symbol(symbol));

        let page_size = limit.unwrap_or(24);
        let path = format!("/market/{}/history/PT1H?pageSize={}", market_id, page_size);
        let page_result: NordPageResult<NordMarketHistoryInfo> =
            self.get_request(&path).await?;

        page_result
            .items
            .iter()
            .map(|h| nord_history_to_kline(h, global_symbol.clone()))
            .collect()
    }

    async fn get_recent_trades(&self, symbol: &str, limit: u32) -> Result<Vec<Trade>> {
        let market_id = self.resolve_market_id(symbol).await?;
        let global_symbol = Self::denormalize_symbol(&self.to_api_symbol(symbol));

        let effective_limit = std::cmp::min(limit, 50);
        let path = format!(
            "/trades?marketId={}&pageSize={}",
            market_id, effective_limit
        );
        let page_result: NordPageResult<NordTrade> = self.get_request(&path).await?;

        page_result
            .items
            .iter()
            .map(|t| nord_trade_to_trade(t, global_symbol.clone()))
            .collect()
    }

    async fn get_market_stats(&self, symbol: &str) -> Result<MarketStats> {
        let market_id = self.resolve_market_id(symbol).await?;
        let global_symbol = Self::denormalize_symbol(&self.to_api_symbol(symbol));

        let stats = self.fetch_market_stats_cached(market_id).await?;
        nord_stats_to_market_stats(&stats, global_symbol)
    }

    async fn get_all_market_stats(&self) -> Result<Vec<MarketStats>> {
        self.ensure_markets().await?;

        let market_entries: Vec<(u32, String)> = {
            let guard = self.markets.read().await;
            let data = guard.as_ref().context("markets not initialized")?;
            data.entries
                .values()
                .map(|e| (e.market_id, e.global_symbol.clone()))
                .collect()
        };

        let futures: Vec<_> = market_entries
            .into_iter()
            .map(|(market_id, global_symbol)| {
                let stats_cache = self.stats_cache.clone();
                let rate_limiter = self.rate_limiter.clone();
                let http = self.http.clone();
                let base_url = self.base_url.clone();

                async move {
                    let stats = if let Some(cached) = stats_cache.get(market_id).await {
                        cached
                    } else {
                        let url = format!("{}/market/{}/stats", base_url, market_id);
                        let config = RetryConfig::default();
                        let client = http.clone();
                        let rl = rate_limiter.clone();

                        let stats: NordMarketStats = execute_with_retry(&config, || {
                            let url = url.clone();
                            let client = client.clone();
                            let rl = rl.clone();
                            async move {
                                rl.execute(|| {
                                    let url = url.clone();
                                    let client = client.clone();
                                    async move {
                                        let resp = client.get(&url).send().await?;
                                        if !resp.status().is_success() {
                                            let status = resp.status();
                                            let text = resp.text().await.unwrap_or_default();
                                            return Err(anyhow::anyhow!(
                                                "HTTP {}: {}",
                                                status,
                                                text
                                            ));
                                        }
                                        Ok(resp.json().await?)
                                    }
                                })
                                .await
                            }
                        })
                        .await?;
                        stats_cache.set(market_id, stats.clone()).await;
                        stats
                    };

                    nord_stats_to_market_stats(&stats, global_symbol)
                }
            })
            .collect();

        let results = futures::future::join_all(futures).await;

        let mut all_stats = Vec::new();
        for result in results {
            match result {
                Ok(ms) => all_stats.push(ms),
                Err(e) => {
                    tracing::warn!("Failed to fetch market stats for 01 market: {}", e);
                }
            }
        }

        Ok(all_stats)
    }

    async fn is_supported(&self, symbol: &str) -> Result<bool> {
        let api_symbol = self.to_api_symbol(symbol);
        tracing::debug!(
            "Checking if symbol {} (normalized to {}) is supported on 01",
            symbol,
            api_symbol
        );

        self.symbols_cache
            .get_or_init(|| async {
                self.ensure_markets().await?;
                let guard = self.markets.read().await;
                let data = guard
                    .as_ref()
                    .context("markets not initialized after ensure_markets")?;
                let symbols: std::collections::HashSet<String> =
                    data.entries.keys().cloned().collect();
                tracing::debug!("Cached {} 01 instruments", symbols.len());
                Ok(symbols)
            })
            .await?;

        let is_cached = self.symbols_cache.contains(&api_symbol).await;
        if is_cached {
            tracing::debug!("Symbol {} ({}) is supported on 01", symbol, api_symbol);
        } else {
            tracing::warn!("Symbol {} ({}) not found on 01", symbol, api_symbol);
        }
        Ok(is_cached)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_symbol() {
        let client = O1Client::new();
        assert_eq!(client.parse_symbol("BTC"), "BTCUSD");
        assert_eq!(client.parse_symbol("eth"), "ETHUSD");
        assert_eq!(client.parse_symbol("BTCUSD"), "BTCUSD");
    }

    #[test]
    fn test_denormalize_symbol() {
        assert_eq!(O1Client::denormalize_symbol("BTCUSD"), "BTC");
        assert_eq!(O1Client::denormalize_symbol("ETHUSD"), "ETH");
        assert_eq!(O1Client::denormalize_symbol("SOLUSD"), "SOL");
    }

    #[test]
    fn test_get_name() {
        let client = O1Client::new();
        assert_eq!(client.get_name(), "01");
    }

    #[tokio::test]
    async fn test_response_cache_fresh() {
        let cache = ResponseCache::new(Duration::from_secs(60));
        cache.set(1, "hello".to_string()).await;

        let result = cache.get(1).await;
        assert_eq!(result, Some("hello".to_string()));
    }

    #[tokio::test]
    async fn test_response_cache_expired() {
        let cache = ResponseCache::new(Duration::from_millis(50));
        cache.set(1, "hello".to_string()).await;

        tokio::time::sleep(Duration::from_millis(60)).await;

        let result = cache.get(1).await;
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn test_response_cache_empty() {
        let cache: ResponseCache<String> = ResponseCache::new(Duration::from_secs(60));
        let result = cache.get(999).await;
        assert_eq!(result, None);
    }
}
