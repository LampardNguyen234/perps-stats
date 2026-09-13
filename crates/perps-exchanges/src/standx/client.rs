use super::conversions::*;
use super::types::*;
use super::ws_client::StandxWsClient;
use crate::cache::SymbolsCache;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::future::join_all;
use perps_core::{
    execute_with_retry, FullOrderbookAdapter, FundingRate, IPerps, Kline, Market, MarketStats,
    MultiResolutionOrderbook, OpenInterest, RateLimiter, RetryConfig, Ticker, Trade,
    WsOrderbookConfig, WsOrderbookManager,
};
use reqwest::Client;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

const BASE_URL: &str = "https://perps.standx.com";
/// Responses are reused for this long before a fresh fetch is made.
const RESPONSE_CACHE_TTL: Duration = Duration::from_secs(5);

/// Client for the StandX perpetuals REST API.
///
/// All endpoints used here are public (no authentication) — trade/user endpoints requiring
/// JWT signing are out of scope for this market-data client.
///
/// Symbol format: global `"BTC"` <-> StandX `"BTC-USD"` (quote asset is always `DUSD`).
pub struct StandxClient {
    http: Client,
    base_url: String,
    symbols_cache: SymbolsCache,
    rate_limiter: Arc<RateLimiter>,
    /// Short-lived response cache: key -> (inserted_at, raw JSON body).
    response_cache: Arc<RwLock<HashMap<String, (Instant, String)>>>,
    /// Present when `ENABLE_ORDERBOOK_STREAMING=true` (and `DATABASE_URL` is set). StandX's
    /// `depth_book` WS channel sends a full snapshot every message, so this uses
    /// `FullOrderbookAdapter` (the Gravity pattern), not the delta/continuity machinery.
    orderbook_manager: Option<Arc<WsOrderbookManager>>,
}

impl StandxClient {
    pub fn new() -> Self {
        let orderbook_manager = if std::env::var("DATABASE_URL").is_ok()
            && std::env::var("ENABLE_ORDERBOOK_STREAMING")
                .map(|v| v.to_lowercase() == "true")
                .unwrap_or(false)
        {
            tracing::info!("StandxClient: WebSocket orderbook streaming enabled");
            Some(Arc::new(WsOrderbookManager::new(
                Arc::new(FullOrderbookAdapter(Arc::new(StandxWsClient::new()))),
                WsOrderbookConfig {
                    staleness_threshold: Duration::from_secs(30),
                    wait_timeout: Duration::from_secs(30),
                    reconnect_delay: Duration::from_secs(2),
                    ..Default::default()
                },
                vec![],
            )))
        } else {
            None
        };
        Self {
            http: Client::new(),
            base_url: BASE_URL.to_string(),
            symbols_cache: SymbolsCache::new(),
            rate_limiter: Arc::new(RateLimiter::standx()),
            response_cache: Arc::new(RwLock::new(HashMap::new())),
            orderbook_manager,
        }
    }

    async fn ensure_cache_initialized(&self) -> Result<()> {
        self.symbols_cache
            .get_or_init(|| async {
                let markets = self.get_markets().await?;
                Ok(markets
                    .iter()
                    .map(|m| self.parse_symbol(&m.symbol))
                    .collect())
            })
            .await
    }

    // ---- HTTP helpers ----

    fn cache_key(path: &str, query: &[(&str, &str)]) -> String {
        let mut pairs: Vec<(&str, &str)> = query.to_vec();
        pairs.sort_unstable_by_key(|(k, _)| *k);
        let qs: Vec<String> = pairs.iter().map(|(k, v)| format!("{}={}", k, v)).collect();
        format!("{}?{}", path, qs.join("&"))
    }

    async fn fetch_raw(&self, path: &str, query: &[(&str, &str)]) -> Result<String> {
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
                            tracing::trace!("StandX GET {}", url);
                            let resp = http.get(&url).query(&query).send().await?;

                            if !resp.status().is_success() {
                                let status = resp.status();
                                let text = resp.text().await.unwrap_or_default();
                                return Err(anyhow!("HTTP {}: {}", status, text));
                            }

                            Ok(resp.text().await?)
                        }
                    })
                    .await
            }
        })
        .await
    }

    /// GET with a 5-second response cache, keyed by path + sorted query params.
    async fn get<R>(&self, path: &str, query: &[(&str, &str)]) -> Result<R>
    where
        R: serde::de::DeserializeOwned + Send + 'static,
    {
        let key = Self::cache_key(path, query);

        {
            let cache = self.response_cache.read().await;
            if let Some((inserted_at, body)) = cache.get(&key) {
                if inserted_at.elapsed() < RESPONSE_CACHE_TTL {
                    tracing::trace!("StandX cache hit: {}", key);
                    return serde_json::from_str(body)
                        .with_context(|| format!("deserialize cached response for {}", key));
                }
            }
        }

        let body = self.fetch_raw(path, query).await?;
        {
            let mut cache = self.response_cache.write().await;
            cache.insert(key.clone(), (Instant::now(), body.clone()));
        }

        serde_json::from_str(&body)
            .with_context(|| format!("deserialize response for {}{}", self.base_url, path))
    }

    // ---- low-level fetchers ----

    async fn fetch_market_overview(&self) -> Result<MarketOverviewResponse> {
        self.get("/api/query_market_overview", &[]).await
    }

    async fn fetch_symbol_info(&self, standx_symbol: &str) -> Result<Vec<SymbolInfo>> {
        self.get("/api/query_symbol_info", &[("symbol", standx_symbol)])
            .await
    }

    async fn fetch_symbol_market(&self, standx_symbol: &str) -> Result<SymbolMarket> {
        self.get("/api/query_symbol_market", &[("symbol", standx_symbol)])
            .await
    }

    async fn fetch_depth_book(&self, standx_symbol: &str) -> Result<DepthBookResponse> {
        self.get("/api/query_depth_book", &[("symbol", standx_symbol)])
            .await
    }

    async fn fetch_recent_trades(&self, standx_symbol: &str) -> Result<Vec<RecentTrade>> {
        self.get("/api/query_recent_trades", &[("symbol", standx_symbol)])
            .await
    }

    /// `start_time`/`end_time` are mandatory query params (verified live 2026-09-13 — the
    /// API returns a 400 "missing field `start_time`" without them), given in milliseconds.
    async fn fetch_funding_rates(
        &self,
        standx_symbol: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<FundingRateEntry>> {
        let start_str = start_ms.to_string();
        let end_str = end_ms.to_string();
        self.get(
            "/api/query_funding_rates",
            &[
                ("symbol", standx_symbol),
                ("start_time", &start_str),
                ("end_time", &end_str),
            ],
        )
        .await
    }

    /// `from`/`to` are unix seconds (verified live 2026-09-13).
    async fn fetch_klines(
        &self,
        standx_symbol: &str,
        resolution: &str,
        from_sec: i64,
        to_sec: i64,
    ) -> Result<KlineHistoryResponse> {
        let from_str = from_sec.to_string();
        let to_str = to_sec.to_string();
        self.get(
            "/api/kline/history",
            &[
                ("symbol", standx_symbol),
                ("from", &from_str),
                ("to", &to_str),
                ("resolution", resolution),
            ],
        )
        .await
    }
}

impl Default for StandxClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IPerps for StandxClient {
    async fn prewarm_streams(&self, symbols: &[String]) -> Result<()> {
        if let Some(manager) = &self.orderbook_manager {
            manager
                .prewarm(symbols.iter().map(|s| self.normalize_symbol(s)).collect())
                .await?;
        }
        Ok(())
    }

    fn get_name(&self) -> &str {
        "standx"
    }

    fn parse_symbol(&self, symbol: &str) -> String {
        standx_parse_symbol(symbol)
    }

    fn normalize_symbol(&self, exchange_symbol: &str) -> String {
        standx_normalize_symbol(exchange_symbol)
    }

    /// StandX has no dedicated "list markets" endpoint. Fetch the symbol list from
    /// `query_market_overview` (one call), then `query_symbol_info` per symbol (join_all) for
    /// sizing fields. StandX's small symbol universe (currently ~13, live-verified 2026-09-13 —
    /// more than the docs' 4-symbol reference set) makes this cheap; failures for individual
    /// symbols are logged and skipped rather than failing the whole call.
    async fn get_markets(&self) -> Result<Vec<Market>> {
        let overview = self.fetch_market_overview().await?;

        let futures = overview.symbols.iter().map(|s| {
            let standx_symbol = s.symbol.clone();
            let last_price = s.last_price;
            let normalized = self.normalize_symbol(&standx_symbol);
            async move {
                let infos = self.fetch_symbol_info(&standx_symbol).await?;
                let info = infos.first().ok_or_else(|| {
                    anyhow!("empty query_symbol_info response for {standx_symbol}")
                })?;
                symbol_info_to_market(info, last_price, normalized)
            }
        });

        let mut markets = Vec::new();
        for result in join_all(futures).await {
            match result {
                Ok(m) => markets.push(m),
                Err(e) => tracing::warn!("StandX: failed to build market: {}", e),
            }
        }

        let symbols: std::collections::HashSet<String> = markets
            .iter()
            .map(|m| self.parse_symbol(&m.symbol))
            .collect();
        self.symbols_cache.initialize(symbols);

        Ok(markets)
    }

    async fn get_market(&self, symbol: &str) -> Result<Market> {
        let standx_symbol = self.parse_symbol(symbol);
        let (overview, infos) = tokio::try_join!(
            self.fetch_market_overview(),
            self.fetch_symbol_info(&standx_symbol),
        )?;
        let last_price = overview
            .symbols
            .iter()
            .find(|s| s.symbol == standx_symbol)
            .map(|s| s.last_price)
            .unwrap_or(rust_decimal::Decimal::ZERO);
        let info = infos
            .first()
            .ok_or_else(|| anyhow!("Market not found: {}", standx_symbol))?;
        symbol_info_to_market(info, last_price, self.normalize_symbol(symbol))
    }

    /// Merges `query_symbol_market` (price/volume/OI/funding) with `query_depth_book`
    /// (top-of-book quantity, which `query_symbol_market` doesn't provide) — 2 calls.
    async fn get_ticker(&self, symbol: &str) -> Result<Ticker> {
        let standx_symbol = self.parse_symbol(symbol);
        tracing::debug!("StandX: fetching ticker for {}", standx_symbol);

        let (market, depth) = tokio::try_join!(
            self.fetch_symbol_market(&standx_symbol),
            self.fetch_depth_book(&standx_symbol),
        )?;
        symbol_market_depth_to_ticker(&market, depth, self.normalize_symbol(symbol))
    }

    async fn get_all_tickers(&self) -> Result<Vec<Ticker>> {
        tracing::debug!("StandX: fetching all tickers");
        let overview = self.fetch_market_overview().await?;

        let futures = overview.symbols.iter().map(|s| {
            let standx_symbol = s.symbol.clone();
            let normalized = self.normalize_symbol(&standx_symbol);
            async move {
                match tokio::try_join!(
                    self.fetch_symbol_market(&standx_symbol),
                    self.fetch_depth_book(&standx_symbol),
                ) {
                    Ok((market, depth)) => {
                        match symbol_market_depth_to_ticker(&market, depth, normalized.clone()) {
                            Ok(t) => Some(t),
                            Err(e) => {
                                tracing::warn!(
                                    "StandX: failed to convert ticker {}: {}",
                                    standx_symbol,
                                    e
                                );
                                None
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("StandX: failed to fetch ticker {}: {}", standx_symbol, e);
                        None
                    }
                }
            }
        });

        Ok(join_all(futures).await.into_iter().flatten().collect())
    }

    async fn get_orderbook(&self, symbol: &str, depth: u32) -> Result<MultiResolutionOrderbook> {
        let normalized = self.normalize_symbol(symbol);
        let depth = depth.max(1);

        if let Some(mgr) = &self.orderbook_manager {
            let client_symbol = self.parse_symbol(symbol);
            let fallback_symbol = normalized.clone();
            let ob = mgr
                .get_orderbook(&normalized, depth, || async move {
                    let raw = self.fetch_depth_book(&client_symbol).await?;
                    let ob = depth_book_to_orderbook(raw, fallback_symbol, depth as usize)?;
                    Ok((ob, 0))
                })
                .await?;
            return Ok(MultiResolutionOrderbook::from_single(ob));
        }

        let standx_symbol = self.parse_symbol(symbol);
        let raw = self.fetch_depth_book(&standx_symbol).await?;
        let orderbook = depth_book_to_orderbook(raw, normalized, depth as usize)?;
        Ok(MultiResolutionOrderbook::from_single(orderbook))
    }

    /// Uses `query_symbol_market` (already fetched for the ticker in the common path) plus
    /// `query_symbol_info`'s `funding_rate_cap` for the cap/floor bound.
    async fn get_funding_rate(&self, symbol: &str) -> Result<FundingRate> {
        let standx_symbol = self.parse_symbol(symbol);
        tracing::debug!("StandX: fetching funding rate for {}", standx_symbol);

        let (market, infos) = tokio::try_join!(
            self.fetch_symbol_market(&standx_symbol),
            self.fetch_symbol_info(&standx_symbol),
        )?;
        let info = infos
            .first()
            .ok_or_else(|| anyhow!("empty query_symbol_info response for {standx_symbol}"))?;
        symbol_market_info_to_funding_rate(&market, info, self.normalize_symbol(symbol))
    }

    async fn get_funding_rate_history(
        &self,
        symbol: &str,
        start_time: Option<DateTime<Utc>>,
        end_time: Option<DateTime<Utc>>,
        limit: Option<u32>,
    ) -> Result<Vec<FundingRate>> {
        let standx_symbol = self.parse_symbol(symbol);
        tracing::debug!(
            "StandX: fetching funding rate history for {}",
            standx_symbol
        );

        let now = Utc::now();
        let start_ms = start_time
            .unwrap_or_else(|| now - chrono::Duration::hours(24))
            .timestamp_millis();
        let end_ms = end_time.unwrap_or(now).timestamp_millis();

        let entries = self
            .fetch_funding_rates(&standx_symbol, start_ms, end_ms)
            .await?;

        let normalized = self.normalize_symbol(symbol);
        let mut rates: Vec<FundingRate> = entries
            .iter()
            .map(|e| funding_rate_entry_to_funding_rate(e, normalized.clone()))
            .collect::<Result<Vec<_>>>()?;

        if let Some(lim) = limit {
            let lim = lim as usize;
            if rates.len() > lim {
                rates = rates.into_iter().rev().take(lim).rev().collect();
            }
        }

        Ok(rates)
    }

    async fn get_open_interest(&self, symbol: &str) -> Result<OpenInterest> {
        let standx_symbol = self.parse_symbol(symbol);
        tracing::debug!("StandX: fetching open interest for {}", standx_symbol);

        let market = self.fetch_symbol_market(&standx_symbol).await?;
        symbol_market_to_open_interest(&market, self.normalize_symbol(symbol))
    }

    async fn get_klines(
        &self,
        symbol: &str,
        interval: &str,
        start_time: Option<DateTime<Utc>>,
        end_time: Option<DateTime<Utc>>,
        limit: Option<u32>,
    ) -> Result<Vec<Kline>> {
        let resolution = interval_to_resolution(interval)?;
        let standx_symbol = self.parse_symbol(symbol);
        tracing::debug!(
            "StandX: fetching klines for {} interval={}",
            standx_symbol,
            interval
        );

        let now = Utc::now();
        let from_sec = start_time
            .unwrap_or_else(|| now - chrono::Duration::hours(24))
            .timestamp();
        let to_sec = end_time.unwrap_or(now).timestamp();

        let resp = self
            .fetch_klines(&standx_symbol, resolution, from_sec, to_sec)
            .await?;

        let mut klines =
            kline_history_to_klines(resp, self.normalize_symbol(symbol), interval.to_string())?;

        if let Some(lim) = limit {
            let lim = lim as usize;
            if klines.len() > lim {
                klines = klines.into_iter().rev().take(lim).rev().collect();
            }
        }

        Ok(klines)
    }

    async fn get_recent_trades(&self, symbol: &str, limit: u32) -> Result<Vec<Trade>> {
        let standx_symbol = self.parse_symbol(symbol);
        tracing::debug!("StandX: fetching recent trades for {}", standx_symbol);

        let entries = self.fetch_recent_trades(&standx_symbol).await?;
        let normalized = self.normalize_symbol(symbol);
        let mut trades: Vec<Trade> = entries
            .iter()
            .map(|t| recent_trade_to_trade(t, normalized.clone()))
            .collect::<Result<Vec<_>>>()?;

        trades.truncate(limit as usize);
        Ok(trades)
    }

    async fn get_market_stats(&self, symbol: &str) -> Result<MarketStats> {
        let standx_symbol = self.parse_symbol(symbol);
        tracing::debug!("StandX: fetching market stats for {}", standx_symbol);

        let market = self.fetch_symbol_market(&standx_symbol).await?;
        symbol_market_to_market_stats(&market, self.normalize_symbol(symbol))
    }

    async fn get_all_market_stats(&self) -> Result<Vec<MarketStats>> {
        tracing::debug!("StandX: fetching all market stats");
        let overview = self.fetch_market_overview().await?;

        let futures = overview.symbols.iter().map(|s| {
            let standx_symbol = s.symbol.clone();
            let normalized = self.normalize_symbol(&standx_symbol);
            async move {
                match self.fetch_symbol_market(&standx_symbol).await {
                    Ok(market) => {
                        match symbol_market_to_market_stats(&market, normalized.clone()) {
                            Ok(ms) => Some(ms),
                            Err(e) => {
                                tracing::warn!(
                                    "StandX: failed to convert market stats {}: {}",
                                    standx_symbol,
                                    e
                                );
                                None
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "StandX: failed to fetch market stats {}: {}",
                            standx_symbol,
                            e
                        );
                        None
                    }
                }
            }
        });

        Ok(join_all(futures).await.into_iter().flatten().collect())
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

    #[test]
    fn test_parse_symbol() {
        let client = StandxClient::new();
        assert_eq!(client.parse_symbol("BTC"), "BTC-USD");
        assert_eq!(client.parse_symbol("XAU"), "XAU-USD");
        assert_eq!(client.parse_symbol("BTC-USD"), "BTC-USD");
    }

    #[test]
    fn test_normalize_symbol() {
        let client = StandxClient::new();
        assert_eq!(client.normalize_symbol("BTC-USD"), "BTC");
    }

    #[test]
    fn test_get_name() {
        let client = StandxClient::new();
        assert_eq!(client.get_name(), "standx");
    }
}
