use crate::qfex::conversions::*;
use crate::qfex::types::*;
use crate::qfex::ws_client::OrderbookManager;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::future::join_all;
use perps_core::{
    execute_with_retry, FundingRate, IPerps, Kline, Market, MarketStats, MultiResolutionOrderbook,
    OpenInterest, RateLimit, RateLimiter, RetryConfig, Ticker, Trade,
};
use reqwest::Client;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// Process-wide singleton `OrderbookManager` shared across all `QfexClient` instances.
/// This ensures only one WS connection is maintained regardless of how many clients are created.
static ORDERBOOK_MANAGER: OnceLock<Arc<OrderbookManager>> = OnceLock::new();

fn shared_orderbook_manager() -> Arc<OrderbookManager> {
    Arc::clone(ORDERBOOK_MANAGER.get_or_init(|| Arc::new(OrderbookManager::new(vec![0, 1, 2]))))
}

const BASE_URL: &str = "https://api.qfex.com";

/// Cached snapshot of `GET /symbols/metrics` with a wall-clock timestamp.
struct MetricsCache {
    data: Vec<SymbolMetrics>,
    fetched_at: Instant,
}

/// REST client for the QFEX perpetuals exchange.
///
/// All market data is served from `https://api.qfex.com` without authentication.
///
/// Symbol format: global `"NVDA"` ↔ QFEX `"NVDA-USD"` (conversion is idempotent).
///
/// Rate limit: 20 requests per second (conservative; QFEX does not publish a hard limit).
///
/// The `/symbols/metrics` endpoint returns **all** symbols at once; results are shared via
/// a 5-second TTL cache so the many methods that need metrics only issue one network request
/// per cycle.
pub struct QfexClient {
    http: Client,
    base_url: String,
    rate_limiter: Arc<RateLimiter>,
    /// Shared 5 s TTL cache for `GET /symbols/metrics` (returns ALL symbols at once).
    metrics_cache: Arc<RwLock<Option<MetricsCache>>>,
    orderbook_manager: Arc<OrderbookManager>,
}

impl QfexClient {
    /// Create a new `QfexClient` with default settings.
    ///
    /// Spawns a background task that fetches all available symbols and pre-subscribes
    /// the WebSocket orderbook manager to them, so orderbook snapshots are available
    /// before the first explicit `get_orderbook` call.
    pub fn new() -> Self {
        let is_first = ORDERBOOK_MANAGER.get().is_none();
        let this = Self {
            http: Client::new(),
            base_url: BASE_URL.to_string(),
            rate_limiter: Arc::new(RateLimiter::new(vec![RateLimit::per_second(20)])),
            metrics_cache: Arc::new(RwLock::new(None)),
            orderbook_manager: shared_orderbook_manager(),
        };

        // Only the first client to initialize triggers the auto-subscribe bootstrap.
        // Subsequent clients (e.g. short-lived validation clients) share the same WS
        // connection without triggering redundant subscriptions.
        if is_first {
            let bootstrap = QfexClient {
                http: this.http.clone(),
                base_url: this.base_url.clone(),
                rate_limiter: Arc::clone(&this.rate_limiter),
                metrics_cache: Arc::clone(&this.metrics_cache),
                orderbook_manager: Arc::clone(&this.orderbook_manager),
            };
            tokio::spawn(async move {
                match bootstrap.get_cached_metrics().await {
                    Ok(metrics) => {
                        let symbols: Vec<String> =
                            metrics.iter().map(|m| m.symbol.clone()).collect();
                        tracing::info!(
                            "qfex: auto-subscribing orderbook for {} symbols",
                            symbols.len()
                        );
                        bootstrap.orderbook_manager.subscribe_symbols(symbols).await;
                    }
                    Err(e) => tracing::warn!("qfex: auto-subscribe startup failed: {}", e),
                }
            });
        }

        this
    }

    // ---- HTTP helper --------------------------------------------------------

    /// Perform a rate-limited, retried GET request and deserialise the JSON response.
    async fn get<R>(&self, path: &str, query: &[(&str, &str)]) -> Result<R>
    where
        R: serde::de::DeserializeOwned + Send + 'static,
    {
        let config = RetryConfig::default();
        let url = format!("{}{}", self.base_url, path);
        let http = self.http.clone();
        let rate_limiter = self.rate_limiter.clone();
        let query: Vec<(String, String)> = query
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();

        execute_with_retry(&config, || {
            let url = url.clone();
            let http = http.clone();
            let rate_limiter = rate_limiter.clone();
            let query = query.clone();
            async move {
                rate_limiter
                    .execute(|| {
                        let url = url.clone();
                        let http = http.clone();
                        let query = query.clone();
                        async move {
                            tracing::trace!("QFEX GET {}", url);
                            let resp = http.get(&url).query(&query).send().await?;
                            if !resp.status().is_success() {
                                let status = resp.status();
                                let text = resp.text().await.unwrap_or_default();
                                return Err(anyhow!("HTTP {}: {}", status, text));
                            }
                            resp.json::<R>()
                                .await
                                .map_err(|e| anyhow!("JSON deserialize error: {}", e))
                        }
                    })
                    .await
            }
        })
        .await
    }

    // ---- Metrics cache -------------------------------------------------------

    /// Return cached metrics, fetching fresh data from `/symbols/metrics` on miss or expiry.
    ///
    /// The cache TTL is 5 seconds; all callers within the window share the same snapshot,
    /// which avoids redundant API calls when multiple methods are invoked in quick succession.
    async fn get_cached_metrics(&self) -> Result<Vec<SymbolMetrics>> {
        // Fast path: check under a read lock.
        {
            let guard = self.metrics_cache.read().await;
            if let Some(c) = guard.as_ref() {
                if c.fetched_at.elapsed() < Duration::from_secs(5) {
                    tracing::debug!("QFEX: metrics cache hit");
                    return Ok(c.data.clone());
                }
            }
        }

        // Cache miss or stale: fetch from the API.
        tracing::debug!("QFEX: fetching /symbols/metrics");
        let resp: SymbolMetricsResponse = self
            .get("/symbols/metrics", &[])
            .await
            .context("failed to fetch /symbols/metrics")?;
        let data = resp.data;

        {
            let mut guard = self.metrics_cache.write().await;
            *guard = Some(MetricsCache {
                data: data.clone(),
                fetched_at: Instant::now(),
            });
        }

        Ok(data)
    }
}

impl Default for QfexClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IPerps for QfexClient {
    /// Returns the canonical exchange name used in database records and CLI output.
    fn get_name(&self) -> &str {
        "qfex"
    }

    /// Convert a global symbol (e.g. `"NVDA"`) to QFEX format (e.g. `"NVDA-USD"`).
    ///
    /// The conversion is idempotent: passing `"NVDA-USD"` returns `"NVDA-USD"` unchanged.
    fn parse_symbol(&self, symbol: &str) -> String {
        to_qfex_symbol(crate::symbol_aliases::resolve_alias("qfex", symbol))
    }

    fn normalize_symbol(&self, exchange_symbol: &str) -> String {
        // "NVDA-USD" -> "NVDA", "GOLD-USD" -> "GOLD" -> unresolve -> "XAU"
        let upper = exchange_symbol.to_uppercase();
        let mut base = upper.strip_suffix("-USD").unwrap_or(&upper);
        base = base.strip_suffix("-KRW").unwrap_or(&base);
        crate::symbol_aliases::unresolve_alias("qfex", base).to_string()
    }

    /// Fetch all active markets from `GET /refdata`.
    ///
    /// Items whose `status` field is not `"ACTIVE"` are silently excluded.
    /// Individual items that fail to convert are warned and skipped rather than failing
    /// the entire call.
    async fn get_markets(&self) -> Result<Vec<Market>> {
        tracing::debug!("QFEX: fetching all markets");

        let resp: RefdataResponse = self
            .get("/refdata", &[])
            .await
            .context("failed to fetch /refdata")?;

        let markets: Vec<Market> = resp
            .data
            .iter()
            .filter(|item| item.status.as_deref() == Some("ACTIVE"))
            .filter_map(|item| match refdata_item_to_market(item) {
                Ok(mut m) => {
                    m.symbol = self.normalize_symbol(&item.symbol);
                    Some(m)
                }
                Err(e) => {
                    tracing::warn!("QFEX: skipping market {}: {}", item.symbol, e);
                    None
                }
            })
            .collect();

        tracing::debug!("QFEX: found {} active markets", markets.len());
        Ok(markets)
    }

    /// Fetch metadata for a single symbol via `GET /refdata?ticker={symbol}`.
    async fn get_market(&self, symbol: &str) -> Result<Market> {
        let qfex_sym = self.parse_symbol(symbol);
        tracing::debug!("QFEX: fetching market for {}", qfex_sym);

        let resp: RefdataResponse = self
            .get("/refdata", &[("ticker", qfex_sym.as_str())])
            .await
            .with_context(|| format!("failed to fetch /refdata for {}", qfex_sym))?;

        let item = resp
            .data
            .iter()
            .find(|item| item.symbol == qfex_sym)
            .ok_or_else(|| anyhow!("Market not found: {}", qfex_sym))?;

        let mut market = refdata_item_to_market(item)
            .with_context(|| format!("failed to convert market data for {}", qfex_sym))?;
        market.symbol = self.normalize_symbol(&qfex_sym);
        Ok(market)
    }

    /// Fetch the current ticker for a single symbol using the shared metrics cache.
    async fn get_ticker(&self, symbol: &str) -> Result<Ticker> {
        let qfex_sym = self.parse_symbol(symbol);
        tracing::debug!("QFEX: fetching ticker for {}", qfex_sym);

        let metrics = self.get_cached_metrics().await?;
        let m = metrics
            .iter()
            .find(|m| self.parse_symbol(&m.symbol) == qfex_sym)
            .ok_or_else(|| anyhow!("No metrics found for symbol: {}", qfex_sym))?;

        let ob = self.get_orderbook(&qfex_sym, 0).await?;

        let mut ticker = metrics_to_ticker(m, &ob)
            .with_context(|| format!("failed to convert ticker for {}", qfex_sym))?;
        ticker.symbol = self.normalize_symbol(&qfex_sym);
        Ok(ticker)
    }

    /// Fetch tickers for all symbols using the shared metrics cache.
    ///
    /// Symbols that fail to convert are warned and skipped.
    async fn get_all_tickers(&self) -> Result<Vec<Ticker>> {
        tracing::debug!("QFEX: fetching all tickers");

        let metrics = self.get_cached_metrics().await?;

        let futures: Vec<_> = metrics
            .iter()
            .map(|m| {
                let m = m.clone();
                async move {
                    match self.get_ticker(&m.symbol).await {
                        Ok(ticker) => Some(ticker),
                        Err(e) => {
                            tracing::warn!(
                                "QFEX: failed to convert ticker for {}: {}",
                                m.symbol,
                                e
                            );
                            None
                        }
                    }
                }
            })
            .collect();

        let results = join_all(futures).await;
        Ok(results.into_iter().flatten().collect())
    }

    /// Fetch the L2 orderbook for a symbol via the WebSocket orderbook manager.
    async fn get_orderbook(&self, symbol: &str, depth: u32) -> Result<MultiResolutionOrderbook> {
        let qfex_sym = self.parse_symbol(symbol);
        tracing::debug!("QFEX: fetching orderbook for {} depth={}", qfex_sym, depth);

        let mut ob = self
            .orderbook_manager
            .get_orderbook(&qfex_sym, depth as usize)
            .await
            .with_context(|| format!("failed to get orderbook for {}", qfex_sym))?;
        ob.symbol = self.normalize_symbol(&qfex_sym);
        for book in &mut ob.orderbooks {
            book.symbol = ob.symbol.clone();
        }
        Ok(ob)
    }

    /// Fetch the current funding rate for a symbol using the shared metrics cache.
    async fn get_funding_rate(&self, symbol: &str) -> Result<FundingRate> {
        let qfex_sym = self.parse_symbol(symbol);
        tracing::debug!("QFEX: fetching funding rate for {}", qfex_sym);

        let metrics = self.get_cached_metrics().await?;
        let m = metrics
            .iter()
            .find(|m| m.symbol == qfex_sym)
            .ok_or_else(|| anyhow!("No metrics found for symbol: {}", qfex_sym))?;

        let mut fr = metrics_to_funding_rate(m)
            .with_context(|| format!("failed to convert funding rate for {}", qfex_sym))?;
        fr.symbol = self.normalize_symbol(&qfex_sym);
        Ok(fr)
    }

    /// Fetch historical funding rates from `GET /funding/{symbol}`.
    ///
    /// Uses an 8-hour interval by default (`intervalMinutes=480`).
    /// Results are sorted by `funding_time` descending and truncated to `limit` if provided.
    async fn get_funding_rate_history(
        &self,
        symbol: &str,
        start_time: Option<DateTime<Utc>>,
        end_time: Option<DateTime<Utc>>,
        limit: Option<u32>,
    ) -> Result<Vec<FundingRate>> {
        let qfex_sym = self.parse_symbol(symbol);
        tracing::debug!("QFEX: fetching funding rate history for {}", qfex_sym);

        let path = format!("/funding/{}", qfex_sym);

        // Build owned strings before constructing the query slice to avoid dangling refs.
        let interval_minutes = "480".to_string();
        let from_iso = start_time.map(|t| t.to_rfc3339());
        let to_iso = end_time.map(|t| t.to_rfc3339());

        let mut query: Vec<(&str, &str)> = vec![("intervalMinutes", &interval_minutes)];
        if let Some(ref from) = from_iso {
            query.push(("fromISO", from.as_str()));
        }
        if let Some(ref to) = to_iso {
            query.push(("toISO", to.as_str()));
        }

        let resp: FundingHistoricResponse = self
            .get(&path, &query)
            .await
            .with_context(|| format!("failed to fetch funding history for {}", qfex_sym))?;

        let normalized_sym = self.normalize_symbol(&qfex_sym);
        let mut rates: Vec<FundingRate> = resp
            .data
            .iter()
            .filter_map(|p| match funding_point_to_funding_rate(p, &qfex_sym) {
                Ok(mut r) => {
                    r.symbol = normalized_sym.clone();
                    Some(r)
                }
                Err(e) => {
                    tracing::warn!("QFEX: skipping funding point for {}: {}", qfex_sym, e);
                    None
                }
            })
            .collect();

        // Most-recent first.
        rates.sort_by(|a, b| b.funding_time.cmp(&a.funding_time));

        if let Some(n) = limit {
            rates.truncate(n as usize);
        }

        Ok(rates)
    }

    /// Fetch the current open interest for a symbol using the shared metrics cache.
    async fn get_open_interest(&self, symbol: &str) -> Result<OpenInterest> {
        let qfex_sym = self.parse_symbol(symbol);
        tracing::debug!("QFEX: fetching open interest for {}", qfex_sym);

        let metrics = self.get_cached_metrics().await?;
        let m = metrics
            .iter()
            .find(|m| m.symbol == qfex_sym)
            .ok_or_else(|| anyhow!("No metrics found for symbol: {}", qfex_sym))?;

        let mut oi = metrics_to_open_interest(m)
            .with_context(|| format!("failed to convert open interest for {}", qfex_sym))?;
        oi.symbol = self.normalize_symbol(&qfex_sym);
        Ok(oi)
    }

    /// Fetch OHLCV candles from `GET /candles/{symbol}`.
    ///
    /// `interval` must be a value supported by QFEX (e.g. `"1m"`, `"1h"`); unsupported
    /// intervals are rejected immediately with an error.
    async fn get_klines(
        &self,
        symbol: &str,
        interval: &str,
        start_time: Option<DateTime<Utc>>,
        end_time: Option<DateTime<Utc>>,
        limit: Option<u32>,
    ) -> Result<Vec<Kline>> {
        let qfex_sym = self.parse_symbol(symbol);
        tracing::debug!(
            "QFEX: fetching klines for {} interval={}",
            qfex_sym,
            interval
        );

        let qfex_resolution = map_kline_interval(interval)
            .with_context(|| format!("unsupported kline interval: {}", interval))?;

        let path = format!("/candles/{}", qfex_sym);

        let resolution_str = qfex_resolution.to_string();
        let from_iso = start_time.map(|t| t.to_rfc3339());
        let to_iso = end_time.map(|t| t.to_rfc3339());

        let mut query: Vec<(&str, &str)> = vec![("resolution", &resolution_str)];
        if let Some(ref from) = from_iso {
            query.push(("fromISO", from.as_str()));
        }
        if let Some(ref to) = to_iso {
            query.push(("toISO", to.as_str()));
        }

        let resp: CandlesResponse = self
            .get(&path, &query)
            .await
            .with_context(|| format!("failed to fetch candles for {}", qfex_sym))?;

        let normalized_sym = self.normalize_symbol(&qfex_sym);
        let mut klines: Vec<Kline> = resp
            .candles
            .iter()
            .filter_map(|c| match candle_to_kline(c, &qfex_sym, interval) {
                Ok(mut k) => {
                    k.symbol = normalized_sym.clone();
                    Some(k)
                }
                Err(e) => {
                    tracing::warn!("QFEX: skipping candle for {}: {}", qfex_sym, e);
                    None
                }
            })
            .collect();

        if let Some(n) = limit {
            klines.truncate(n as usize);
        }

        Ok(klines)
    }

    /// QFEX does not expose a REST endpoint for recent trades.
    ///
    /// Returns an error directing callers to use WebSocket streaming instead.
    async fn get_recent_trades(&self, _symbol: &str, _limit: u32) -> Result<Vec<Trade>> {
        Err(anyhow!(
            "QFEX does not provide a REST trades endpoint; use WebSocket streaming instead"
        ))
    }

    /// Fetch aggregated market stats for a single symbol using the shared metrics cache.
    async fn get_market_stats(&self, symbol: &str) -> Result<MarketStats> {
        let qfex_sym = self.parse_symbol(symbol);
        tracing::debug!("QFEX: fetching market stats for {}", qfex_sym);

        let metrics = self.get_cached_metrics().await?;
        let m = metrics
            .iter()
            .find(|m| m.symbol == qfex_sym)
            .ok_or_else(|| anyhow!("No metrics found for symbol: {}", qfex_sym))?;

        let mut ms = metrics_to_market_stats(m, None)
            .with_context(|| format!("failed to convert market stats for {}", qfex_sym))?;
        ms.symbol = self.normalize_symbol(&qfex_sym);
        Ok(ms)
    }

    /// Fetch aggregated market stats for all symbols using the shared metrics cache.
    ///
    /// Symbols that fail to convert are warned and skipped.
    async fn get_all_market_stats(&self) -> Result<Vec<MarketStats>> {
        tracing::debug!("QFEX: fetching all market stats");

        let metrics = self.get_cached_metrics().await?;

        let futures: Vec<_> = metrics
            .iter()
            .map(|m| {
                let m = m.clone();
                let normalized = self.normalize_symbol(&m.symbol);
                async move {
                    match metrics_to_market_stats(&m, None) {
                        Ok(mut ms) => {
                            ms.symbol = normalized;
                            Some(ms)
                        }
                        Err(e) => {
                            tracing::warn!(
                                "QFEX: failed to convert market stats for {}: {}",
                                m.symbol,
                                e
                            );
                            None
                        }
                    }
                }
            })
            .collect();

        let results = join_all(futures).await;
        Ok(results.into_iter().flatten().collect())
    }

    async fn is_supported(&self, symbol: &str) -> Result<bool> {
        let markets = self.get_markets().await?;
        let normalized = self.normalize_symbol(symbol);
        Ok(markets.iter().any(|m| m.symbol == normalized))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_name() {
        let client = QfexClient::new();
        assert_eq!(client.get_name(), "qfex");
    }

    #[test]
    fn test_parse_symbol() {
        let client = QfexClient::new();
        // Global → QFEX format
        assert_eq!(client.parse_symbol("NVDA"), "NVDA-USD");
        assert_eq!(client.parse_symbol("BTC"), "BTC-USD");
        // Idempotent
        assert_eq!(client.parse_symbol("NVDA-USD"), "NVDA-USD");
    }
}
