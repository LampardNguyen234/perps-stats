use crate::cache::{ContractCache, SymbolsCache};
use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::future::join_all;
use perps_core::{
    execute_with_retry, FundingRate, IPerps, Kline, Market, MarketStats, MultiResolutionOrderbook,
    OpenInterest, RateLimiter, RetryConfig, Ticker, Trade,
};
use reqwest::Client;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

use super::conversions::*;
use super::types::*;

const BASE_URL: &str = "https://api.rise.trade";
/// Responses reused for this long before a fresh fetch is made.
const RESPONSE_CACHE_TTL: Duration = Duration::from_secs(5);
/// RISEx documents at most 250 levels per side.
const ORDERBOOK_MAX_DEPTH: u32 = 250;

/// Client for the RISEx perpetuals DEX REST API.
///
/// Symbol format: global `"BTC"` → RISEx `"BTC/USDC"`.
///
/// Most market-summary data (ticker, funding, OI, stats) is read from a single
/// `GET /v1/markets` call cached with a 5-second TTL.  Bid/ask prices require
/// a separate `GET /v1/orderbook` per symbol, also cached at the same TTL.
///
/// Rate limit: conservative 20 req/s (undocumented by API).
#[derive(Clone)]
pub struct RiseXClient {
    http: Client,
    base_url: String,
    symbols_cache: SymbolsCache,
    /// Permanent: global symbol → integer market_id.
    /// RISEx detail endpoints (orderbook, klines, trades, funding history) need this id.
    market_id_cache: ContractCache<u64>,
    /// Short-lived response cache: canonical URL key → (inserted_at, raw JSON body).
    response_cache: Arc<RwLock<HashMap<String, (Instant, String)>>>,
    rate_limiter: Arc<RateLimiter>,
}

impl RiseXClient {
    pub fn new() -> Self {
        Self {
            http: Client::new(),
            base_url: BASE_URL.to_string(),
            symbols_cache: SymbolsCache::new(),
            market_id_cache: ContractCache::new(),
            response_cache: Arc::new(RwLock::new(HashMap::new())),
            rate_limiter: Arc::new(RateLimiter::risex()),
        }
    }

    /// Canonical cache key: path + sorted query params joined with `|`.
    fn cache_key(path: &str, query: &[(&str, &str)]) -> String {
        let mut params: Vec<_> = query.to_vec();
        params.sort_by_key(|(k, _)| *k);
        let qs = params
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect::<Vec<_>>()
            .join("|");
        if qs.is_empty() {
            path.to_string()
        } else {
            format!("{}|{}", path, qs)
        }
    }

    /// GET path with query params, rate-limited and retried on 429.  Returns raw body.
    async fn fetch_raw(&self, path: &str, query: &[(&str, &str)]) -> Result<String> {
        let url = format!("{}{}", self.base_url, path);
        let http = self.http.clone();
        let rate_limiter = self.rate_limiter.clone();
        let query_owned: Vec<(String, String)> = query
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();

        execute_with_retry(&RetryConfig::default(), || {
            let url = url.clone();
            let http = http.clone();
            let rate_limiter = rate_limiter.clone();
            let query = query_owned.clone();
            async move {
                rate_limiter
                    .execute(|| async {
                        let resp = http
                            .get(&url)
                            .query(&query)
                            .send()
                            .await
                            .with_context(|| format!("GET {}", url))?;
                        if !resp.status().is_success() {
                            anyhow::bail!(
                                "HTTP {}: {}",
                                resp.status(),
                                resp.text().await.unwrap_or_default()
                            );
                        }
                        resp.text()
                            .await
                            .with_context(|| format!("read body for {}", url))
                    })
                    .await
            }
        })
        .await
    }

    fn decode_envelope<T: serde::de::DeserializeOwned>(body: &str, context: &str) -> Result<T> {
        let envelope: ApiEnvelope<T> = serde_json::from_str(body)
            .with_context(|| format!("deserialize RISEx envelope for {}", context))?;
        Ok(envelope.data)
    }

    /// Fetch with 5-second TTL response cache.
    async fn get_cached<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T> {
        let key = Self::cache_key(path, query);

        {
            let cache = self.response_cache.read().await;
            if let Some((inserted_at, body)) = cache.get(&key) {
                if inserted_at.elapsed() < RESPONSE_CACHE_TTL {
                    return Self::decode_envelope(body, &key);
                }
            }
        }

        let body = self.fetch_raw(path, query).await?;
        self.response_cache
            .write()
            .await
            .insert(key.clone(), (Instant::now(), body.clone()));
        Self::decode_envelope(&body, &key)
    }

    /// Fetch without caching — used for history/live endpoints where TTL would cause stale results.
    async fn get_uncached<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T> {
        let body = self.fetch_raw(path, query).await?;
        Self::decode_envelope(&body, path)
    }

    /// Initialize symbols_cache and market_id_cache from /v1/markets.  Idempotent.
    async fn ensure_cache_initialized(&self) -> Result<()> {
        if self.symbols_cache.is_initialized() {
            return Ok(());
        }
        let resp: ApiGetMarketsResponse = self.get_cached("/v1/markets", &[]).await?;
        let symbols: std::collections::HashSet<String> = resp
            .markets
            .iter()
            .filter(|m| m.active && m.visible.unwrap_or(true))
            .map(|m| global_symbol_from_risex_symbol(&m.base_asset_symbol))
            .collect();
        let ids: HashMap<String, u64> = resp
            .markets
            .iter()
            .filter(|m| m.active && m.visible.unwrap_or(true))
            .filter_map(|m| {
                let id = m.market_id.parse::<u64>().ok()?;
                Some((global_symbol_from_risex_symbol(&m.base_asset_symbol), id))
            })
            .collect();
        self.symbols_cache.initialize(symbols);
        self.market_id_cache.initialize(ids);
        Ok(())
    }

    /// Resolve a global symbol (e.g. "BTC") to its integer market_id.
    async fn market_id_for(&self, symbol: &str) -> Result<u64> {
        self.ensure_cache_initialized().await?;
        let global = global_symbol_from_risex_symbol(&risex_symbol_from_input(symbol));
        self.market_id_cache
            .get(&global)
            .await
            .ok_or_else(|| anyhow::anyhow!("RISEx: unknown symbol '{}'", symbol))
    }

    /// GET /v1/markets — cached, returns all market entries.
    async fn fetch_markets(&self) -> Result<Vec<ApiMarketInfo>> {
        let resp: ApiGetMarketsResponse = self.get_cached("/v1/markets", &[]).await?;
        Ok(resp.markets)
    }

    /// GET /v1/orderbook — cached at full depth (ORDERBOOK_MAX_DEPTH).
    async fn fetch_orderbook(&self, market_id: u64) -> Result<ApiGetOrderbookResponse> {
        let id_str = market_id.to_string();
        let depth_str = ORDERBOOK_MAX_DEPTH.to_string();
        self.get_cached(
            "/v1/orderbook",
            &[("market_id", &id_str), ("limit", &depth_str)],
        )
        .await
    }
}

impl Default for RiseXClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IPerps for RiseXClient {
    fn get_name(&self) -> &str {
        "risex"
    }

    fn parse_symbol(&self, symbol: &str) -> String {
        risex_symbol_from_input(symbol)
    }

    fn normalize_symbol(&self, exchange_symbol: &str) -> String {
        global_symbol_from_risex_symbol(exchange_symbol)
    }

    async fn get_markets(&self) -> Result<Vec<Market>> {
        let markets = self.fetch_markets().await?;
        Ok(markets
            .iter()
            .filter(|m| m.active && m.visible.unwrap_or(true))
            .map(to_market)
            .collect())
    }

    async fn get_market(&self, symbol: &str) -> Result<Market> {
        let sym = self.normalize_symbol(symbol);
        let markets = self.fetch_markets().await?;
        markets
            .iter()
            .find(|m| self.normalize_symbol(&m.base_asset_symbol) == sym)
            .map(to_market)
            .ok_or_else(|| anyhow::anyhow!("RISEx: market not found for '{}'", symbol))
    }

    async fn get_ticker(&self, symbol: &str) -> Result<Ticker> {
        let sym = self.normalize_symbol(symbol);
        let market_id = self.market_id_for(&sym).await?;
        let markets = self.fetch_markets().await?;
        let market = markets
            .iter()
            .find(|m| self.normalize_symbol(&m.base_asset_symbol) == sym)
            .ok_or_else(|| anyhow::anyhow!("RISEx: ticker not found for '{}'", symbol))?;
        let ob = self.fetch_orderbook(market_id).await?;
        Ok(to_ticker(market, &ob))
    }

    async fn get_all_tickers(&self) -> Result<Vec<Ticker>> {
        self.ensure_cache_initialized().await?;
        let markets = self.fetch_markets().await?;
        let active: Vec<_> = markets
            .iter()
            .filter(|m| m.active && m.visible.unwrap_or(true))
            .collect();

        let futures: Vec<_> = active
            .iter()
            .map(|m| async move {
                let sym = self.normalize_symbol(&m.base_asset_symbol);
                let market_id = match self.market_id_for(&sym).await {
                    Ok(id) => id,
                    Err(e) => {
                        tracing::warn!(
                            "RISEx get_all_tickers: market_id for {} failed: {}",
                            sym,
                            e
                        );
                        return None;
                    }
                };
                match self.fetch_orderbook(market_id).await {
                    Ok(ob) => Some(to_ticker(m, &ob)),
                    Err(e) => {
                        tracing::warn!(
                            "RISEx get_all_tickers: orderbook for {} failed: {}",
                            sym,
                            e
                        );
                        None
                    }
                }
            })
            .collect();

        let results = join_all(futures).await;
        Ok(results.into_iter().flatten().collect())
    }

    async fn get_orderbook(&self, symbol: &str, depth: u32) -> Result<MultiResolutionOrderbook> {
        let sym = self.normalize_symbol(symbol);
        let market_id = self.market_id_for(&sym).await?;
        let depth = depth.clamp(1, ORDERBOOK_MAX_DEPTH) as usize;
        let ob_resp = self.fetch_orderbook(market_id).await?;
        let ob = to_orderbook(&ob_resp, &sym, depth);
        Ok(MultiResolutionOrderbook::from_single(ob))
    }

    async fn get_funding_rate(&self, symbol: &str) -> Result<FundingRate> {
        let sym = self.normalize_symbol(symbol);
        let markets = self.fetch_markets().await?;
        markets
            .iter()
            .find(|m| self.normalize_symbol(&m.base_asset_symbol) == sym)
            .map(to_funding_rate)
            .ok_or_else(|| anyhow::anyhow!("RISEx: funding rate not found for '{}'", symbol))
    }

    async fn get_funding_rate_history(
        &self,
        symbol: &str,
        start_time: Option<DateTime<Utc>>,
        end_time: Option<DateTime<Utc>>,
        limit: Option<u32>,
    ) -> Result<Vec<FundingRate>> {
        let sym = self.normalize_symbol(symbol);
        let market_id = self.market_id_for(&sym).await?;
        let path = format!("/v1/markets/id/{}/funding-rate-history", market_id);
        let target = limit.unwrap_or(100).max(1) as usize;
        let mut page = 1u32;
        let mut out: Vec<FundingRate> = Vec::with_capacity(target);

        while out.len() < target {
            let remaining = target - out.len();
            let page_str = page.to_string();
            let lim_str = remaining.min(100).to_string();
            let start_ns = start_time
                .and_then(|t| t.timestamp_nanos_opt())
                .map(|n| n.to_string());
            let end_ns = end_time
                .and_then(|t| t.timestamp_nanos_opt())
                .map(|n| n.to_string());

            let mut query: Vec<(&str, String)> = vec![("limit", lim_str), ("page", page_str)];
            if let Some(ref s) = start_ns {
                query.push(("start_time", s.clone()));
            }
            if let Some(ref e) = end_ns {
                query.push(("end_time", e.clone()));
            }

            let q_refs: Vec<(&str, &str)> = query.iter().map(|(k, v)| (*k, v.as_str())).collect();
            let resp: ApiGetFundingHistoryResponse = self.get_uncached(&path, &q_refs).await?;

            out.extend(
                resp.records
                    .iter()
                    .map(|r| to_funding_rate_from_record(&sym, r)),
            );
            if !resp.has_next_page.unwrap_or(false) || resp.records.is_empty() {
                break;
            }
            page += 1;
        }

        out.truncate(target);
        Ok(out)
    }

    async fn get_open_interest(&self, symbol: &str) -> Result<OpenInterest> {
        let sym = self.normalize_symbol(symbol);
        let markets = self.fetch_markets().await?;
        markets
            .iter()
            .find(|m| self.normalize_symbol(&m.base_asset_symbol) == sym)
            .map(to_open_interest)
            .ok_or_else(|| anyhow::anyhow!("RISEx: open interest not found for '{}'", symbol))
    }

    async fn get_klines(
        &self,
        symbol: &str,
        interval: &str,
        start_time: Option<DateTime<Utc>>,
        end_time: Option<DateTime<Utc>>,
        limit: Option<u32>,
    ) -> Result<Vec<Kline>> {
        let sym = self.normalize_symbol(symbol);
        let market_id = self.market_id_for(&sym).await?;
        let interval_ns = interval_to_nanos(interval)?.to_string();
        let path = format!("/v1/markets/id/{}/trading-view-data", market_id);

        let from_str = start_time
            .and_then(|t| t.timestamp_nanos_opt())
            .map(|n| n.to_string());
        let to_str = end_time
            .and_then(|t| t.timestamp_nanos_opt())
            .map(|n| n.to_string());

        let mut query = vec![("interval", interval_ns.as_str())];
        if let Some(ref f) = from_str {
            query.push(("from", f));
        }
        if let Some(ref t) = to_str {
            query.push(("to", t));
        }

        let resp: ApiGetKlinesResponse = self.get_uncached(&path, &query).await?;
        let mut klines: Vec<Kline> = resp
            .data
            .iter()
            .filter_map(|bar| {
                to_kline(&sym, bar, interval)
                    .map_err(|e| tracing::warn!("RISEx kline conversion failed: {}", e))
                    .ok()
            })
            .collect();

        if let Some(lim) = limit {
            let lim = lim as usize;
            if klines.len() > lim {
                klines = klines.into_iter().rev().take(lim).rev().collect();
            }
        }
        Ok(klines)
    }

    async fn get_recent_trades(&self, symbol: &str, limit: u32) -> Result<Vec<Trade>> {
        let sym = self.normalize_symbol(symbol);
        let market_id = self.market_id_for(&sym).await?;
        let path = format!("/v1/markets/id/{}/trade-history", market_id);
        let lim_str = limit.to_string();
        let resp: ApiGetTradeHistoryResponse =
            self.get_uncached(&path, &[("limit", &lim_str)]).await?;
        Ok(resp
            .trades
            .iter()
            .filter_map(|t| {
                to_trade(&sym, t)
                    .map_err(|e| tracing::warn!("RISEx trade conversion failed: {}", e))
                    .ok()
            })
            .collect())
    }

    async fn get_market_stats(&self, symbol: &str) -> Result<MarketStats> {
        let sym = self.normalize_symbol(symbol);
        let markets = self.fetch_markets().await?;
        markets
            .iter()
            .find(|m| self.normalize_symbol(&m.base_asset_symbol) == sym)
            .map(to_market_stats)
            .ok_or_else(|| anyhow::anyhow!("RISEx: market stats not found for '{}'", symbol))
    }

    async fn get_all_market_stats(&self) -> Result<Vec<MarketStats>> {
        let markets = self.fetch_markets().await?;
        Ok(markets
            .iter()
            .filter(|m| m.active && m.visible.unwrap_or(true))
            .map(to_market_stats)
            .collect())
    }

    async fn is_supported(&self, symbol: &str) -> Result<bool> {
        let markets = self.get_markets().await?;
        let sym = self.normalize_symbol(symbol);
        Ok(markets.iter().any(|m| m.symbol == sym))
    }
}
