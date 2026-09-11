use crate::arcus::conversions;
use crate::arcus::types::*;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::future::join_all;
use once_cell::sync::OnceCell;
use perps_core::{
    execute_with_retry, FundingRate, IPerps, Kline, Market, MarketStats, MultiResolutionOrderbook,
    OpenInterest, RateLimit, RateLimiter, RetryConfig, Ticker, Trade,
};
use reqwest::Client;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex as AsyncMutex;

const BASE_URL: &str = "https://api.arcus.xyz";
/// TTL for the shared, unfiltered `/v1/markets` response — this single endpoint backs
/// `get_markets`, `get_ticker`, `get_all_tickers`, `get_market_stats`, `get_all_market_stats`,
/// and `is_supported`.
const MARKETS_CACHE_TTL: Duration = Duration::from_secs(4);

/// Snapshot of the last successful `/v1/markets` fetch.
struct MarketsCacheEntry {
    markets: Vec<ArcusMarket>,
    /// Wall-clock time this snapshot was captured; reused as the `timestamp` for every domain
    /// object derived from it, rather than calling `Utc::now()` per converted item.
    captured_at: DateTime<Utc>,
    fetched_at: Instant,
}

/// State shared by every `ArcusClient` created via `ArcusClient::new()` (process-wide, since
/// Arcus's rate limit is per-IP, not per-client-instance).
pub struct ArcusSharedState {
    rate_limiter: RateLimiter,
    /// Guarded by an async mutex (not `RwLock`) so a cache refill holds the lock across the
    /// network call: concurrent cold-cache callers queue on the lock and each re-check the TTL
    /// after acquiring it, so exactly one upstream `/v1/markets` request is made per refill.
    markets_cache: AsyncMutex<Option<MarketsCacheEntry>>,
}

impl ArcusSharedState {
    fn new() -> Self {
        Self {
            // Arcus documents a per-IP weight budget (~1,500 weight/min); this stays under
            // that with headroom for weight-2 endpoints. See `01_overview.md` for the
            // derivation. Re-confirm endpoint weights before raising these.
            rate_limiter: RateLimiter::new(vec![
                RateLimit::per_second(10),
                RateLimit::per_minute(600),
            ]),
            markets_cache: AsyncMutex::new(None),
        }
    }
}

static SHARED_STATE: OnceCell<Arc<ArcusSharedState>> = OnceCell::new();

fn shared_state() -> Arc<ArcusSharedState> {
    SHARED_STATE
        .get_or_init(|| Arc::new(ArcusSharedState::new()))
        .clone()
}

/// REST client for the Arcus perpetuals exchange (crypto + equities/commodities/indices).
///
/// All market data is served from `https://api.arcus.xyz` without authentication.
///
/// Symbol format: global `"BTC"` <-> Arcus `"BTC-USD"`; aliasing (e.g. `"SKHYNIX"` -> `"SKHY"`)
/// goes through `symbol_aliases.toml`, never hardcoded here.
///
/// `/v1/markets` does not expose bid/ask, so `get_ticker`/`get_all_tickers` always make an
/// additional top-of-book orderbook request per symbol.
pub struct ArcusClient {
    http: Client,
    base_url: String,
    shared: Arc<ArcusSharedState>,
}

impl ArcusClient {
    pub fn new() -> Self {
        Self {
            http: Client::new(),
            base_url: BASE_URL.to_string(),
            shared: shared_state(),
        }
    }

    /// Test-only constructor pointing at an injected base URL (e.g. a local mock server) with
    /// isolated shared state — does not participate in the process-wide cache/rate limiter.
    pub fn new_for_test(base_url: impl Into<String>) -> Self {
        Self {
            http: Client::new(),
            base_url: base_url.into(),
            shared: Arc::new(ArcusSharedState::new()),
        }
    }

    /// Test-only constructor sharing an already-constructed `ArcusSharedState` with another
    /// test client, for asserting cache/rate-limit coalescing across "independently created"
    /// clients.
    pub fn new_for_test_with_shared(
        base_url: impl Into<String>,
        shared: Arc<ArcusSharedState>,
    ) -> Self {
        Self {
            http: Client::new(),
            base_url: base_url.into(),
            shared,
        }
    }

    /// Convert a global symbol (e.g. `"BTC"`, `"SKHYNIX"`) to Arcus format (e.g. `"BTC-USD"`,
    /// `"SKHY-USD"`). Idempotent: `"BTC-USD"` and `"SKHYNIX-USD"` both pass through unchanged
    /// modulo alias resolution, because the suffix is stripped before the alias lookup.
    fn to_arcus_symbol(symbol: &str) -> String {
        let upper = symbol.to_uppercase();
        let base = upper.strip_suffix("-USD").unwrap_or(&upper);
        let aliased = crate::symbol_aliases::resolve_alias("arcus", base);
        format!("{aliased}-USD")
    }

    /// Convert an Arcus symbol (or an already-global one) back to global format.
    fn to_global_symbol(exchange_symbol: &str) -> String {
        let upper = exchange_symbol.to_uppercase();
        let base = upper.strip_suffix("-USD").unwrap_or(&upper);
        crate::symbol_aliases::unresolve_alias("arcus", base).to_string()
    }

    // ---- HTTP helpers ----

    /// Rate-limited, retried GET against an arbitrary Arcus path. Not cached — callers that
    /// need the shared `/v1/markets` snapshot should use `cached_markets` instead.
    async fn get<R>(&self, path: &str, query: &[(&str, &str)]) -> Result<R>
    where
        R: serde::de::DeserializeOwned + Send + 'static,
    {
        let config = RetryConfig::default();
        let url = format!("{}{}", self.base_url, path);
        let http = self.http.clone();
        let rate_limiter = self.shared.rate_limiter.clone();
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
                            tracing::trace!("Arcus GET {}", url);
                            let resp = http.get(&url).query(&query).send().await?;

                            if !resp.status().is_success() {
                                let status = resp.status();
                                let text = resp.text().await.unwrap_or_default();
                                let excerpt: String = text.chars().take(500).collect();
                                return Err(anyhow!("HTTP {}: {}", status, excerpt));
                            }

                            resp.json::<R>()
                                .await
                                .map_err(|e| anyhow!("JSON deserialize error for {}: {}", url, e))
                        }
                    })
                    .await
            }
        })
        .await
    }

    /// Return the cached unfiltered `/v1/markets` snapshot, refilling on miss or expiry.
    ///
    /// Holds the mutex across the network call on a miss so concurrent callers coalesce into
    /// exactly one upstream request per refill window.
    async fn cached_markets(&self) -> Result<(Vec<ArcusMarket>, DateTime<Utc>)> {
        let mut guard = self.shared.markets_cache.lock().await;

        if let Some(entry) = guard.as_ref() {
            if entry.fetched_at.elapsed() < MARKETS_CACHE_TTL {
                return Ok((entry.markets.clone(), entry.captured_at));
            }
        }

        tracing::debug!("Arcus: refilling /v1/markets cache");
        let resp: ArcusMarketsResponse = self.get("/v1/markets", &[]).await?;
        let captured_at = Utc::now();
        *guard = Some(MarketsCacheEntry {
            markets: resp.markets.clone(),
            captured_at,
            fetched_at: Instant::now(),
        });

        Ok((resp.markets, captured_at))
    }

    fn find_market<'a>(markets: &'a [ArcusMarket], arcus_symbol: &str) -> Result<&'a ArcusMarket> {
        markets
            .iter()
            .find(|m| m.market_display_name == arcus_symbol)
            .ok_or_else(|| anyhow!("Market not found: {}", arcus_symbol))
    }

    async fn fetch_orderbook_raw(
        &self,
        arcus_symbol: &str,
        n_levels: u32,
    ) -> Result<OrderbookSnapshot> {
        let n = n_levels.clamp(1, 100).to_string();
        let path = format!("/v1/l2OrderBook/{arcus_symbol}");
        self.get(&path, &[("nLevels", n.as_str())]).await
    }
}

impl Default for ArcusClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IPerps for ArcusClient {
    fn get_name(&self) -> &str {
        "arcus"
    }

    fn parse_symbol(&self, symbol: &str) -> String {
        Self::to_arcus_symbol(symbol)
    }

    fn normalize_symbol(&self, exchange_symbol: &str) -> String {
        Self::to_global_symbol(exchange_symbol)
    }

    async fn get_markets(&self) -> Result<Vec<Market>> {
        let (markets, _captured_at) = self.cached_markets().await?;

        Ok(markets
            .iter()
            .filter(|m| m.status == "ONLINE" && m.market_type == "PERPETUAL")
            .filter_map(|m| match conversions::to_market(m) {
                Ok(mut market) => {
                    market.symbol = self.normalize_symbol(&m.market_display_name);
                    Some(market)
                }
                Err(e) => {
                    tracing::warn!("Arcus: skipping market {}: {}", m.market_display_name, e);
                    None
                }
            })
            .collect())
    }

    async fn get_market(&self, symbol: &str) -> Result<Market> {
        let arcus_symbol = self.parse_symbol(symbol);
        let resp: ArcusMarketsResponse = self
            .get("/v1/markets", &[("market", arcus_symbol.as_str())])
            .await?;
        let m = resp
            .markets
            .first()
            .ok_or_else(|| anyhow!("Market not found: {}", arcus_symbol))?;

        let mut market = conversions::to_market(m)?;
        market.symbol = self.normalize_symbol(&m.market_display_name);
        Ok(market)
    }

    /// Fetches the cached markets snapshot plus a fresh `nLevels=1` orderbook request, so the
    /// returned `Ticker` always has non-zero top-of-book data.
    async fn get_ticker(&self, symbol: &str) -> Result<Ticker> {
        let arcus_symbol = self.parse_symbol(symbol);
        let (markets, captured_at) = self.cached_markets().await?;
        let m = Self::find_market(&markets, &arcus_symbol)?;

        let ob = self.fetch_orderbook_raw(&arcus_symbol, 1).await?;
        let mut ticker = conversions::to_ticker(m, &ob, captured_at)?;
        ticker.symbol = self.normalize_symbol(&arcus_symbol);
        Ok(ticker)
    }

    /// Fetches the cached markets snapshot once, then fans out one top-of-book orderbook
    /// request per active market. Entries that fail to fetch/convert, or that would otherwise
    /// come back `Ticker::is_empty()`, are warned and skipped rather than failing the batch.
    async fn get_all_tickers(&self) -> Result<Vec<Ticker>> {
        let (markets, captured_at) = self.cached_markets().await?;
        let active: Vec<&ArcusMarket> = markets
            .iter()
            .filter(|m| m.status == "ONLINE" && m.market_type == "PERPETUAL")
            .collect();

        let futures = active.into_iter().map(|m| async move {
            let ob = match self.fetch_orderbook_raw(&m.market_display_name, 1).await {
                Ok(ob) => ob,
                Err(e) => {
                    tracing::warn!(
                        "Arcus: failed to fetch orderbook for {}: {}",
                        m.market_display_name,
                        e
                    );
                    return None;
                }
            };

            match conversions::to_ticker(m, &ob, captured_at) {
                Ok(mut ticker) => {
                    ticker.symbol = self.normalize_symbol(&m.market_display_name);
                    if ticker.is_empty() {
                        tracing::warn!(
                            "Arcus: skipping empty ticker for {}",
                            m.market_display_name
                        );
                        None
                    } else {
                        Some(ticker)
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "Arcus: failed to convert ticker for {}: {}",
                        m.market_display_name,
                        e
                    );
                    None
                }
            }
        });

        Ok(join_all(futures).await.into_iter().flatten().collect())
    }

    async fn get_orderbook(&self, symbol: &str, depth: u32) -> Result<MultiResolutionOrderbook> {
        let arcus_symbol = self.parse_symbol(symbol);
        let raw = self.fetch_orderbook_raw(&arcus_symbol, depth).await?;
        let normalized = self.normalize_symbol(&arcus_symbol);
        let orderbook = conversions::orderbook_snapshot_to_orderbook(&raw, normalized)?;
        Ok(MultiResolutionOrderbook::from_single(orderbook))
    }

    async fn get_funding_rate(&self, symbol: &str) -> Result<FundingRate> {
        let arcus_symbol = self.parse_symbol(symbol);
        let (markets, _captured_at) = self.cached_markets().await?;
        let m = Self::find_market(&markets, &arcus_symbol)?;

        let mut fr = conversions::to_funding_rate(m)?;
        fr.symbol = self.normalize_symbol(&arcus_symbol);
        Ok(fr)
    }

    async fn get_funding_rate_history(
        &self,
        symbol: &str,
        start_time: Option<DateTime<Utc>>,
        end_time: Option<DateTime<Utc>>,
        limit: Option<u32>,
    ) -> Result<Vec<FundingRate>> {
        let arcus_symbol = self.parse_symbol(symbol);

        let from_str;
        let to_str;
        let limit_str;
        let mut query: Vec<(&str, &str)> = vec![("market", arcus_symbol.as_str())];

        if let Some(start) = start_time {
            from_str = start.timestamp_micros().to_string();
            query.push(("from", &from_str));
        }
        if let Some(end) = end_time {
            to_str = end.timestamp_micros().to_string();
            query.push(("to", &to_str));
        }
        if let Some(lim) = limit {
            limit_str = lim.to_string();
            query.push(("limit", &limit_str));
        }

        let resp: FundingRatesResponse = self.get("/v1/fundingRates", &query).await?;
        let normalized = self.normalize_symbol(&arcus_symbol);

        resp.funding_rates
            .iter()
            .map(|e| conversions::funding_rate_entry_to_funding_rate(e, normalized.clone()))
            .collect()
    }

    async fn get_open_interest(&self, symbol: &str) -> Result<OpenInterest> {
        let arcus_symbol = self.parse_symbol(symbol);
        let (markets, captured_at) = self.cached_markets().await?;
        let m = Self::find_market(&markets, &arcus_symbol)?;

        let mut oi = conversions::to_open_interest(m, captured_at)?;
        oi.symbol = self.normalize_symbol(&arcus_symbol);
        Ok(oi)
    }

    async fn get_klines(
        &self,
        symbol: &str,
        interval: &str,
        start_time: Option<DateTime<Utc>>,
        end_time: Option<DateTime<Utc>>,
        limit: Option<u32>,
    ) -> Result<Vec<Kline>> {
        if !conversions::SUPPORTED_TIMEFRAMES.contains(&interval) {
            anyhow::bail!(
                "Arcus does not support kline interval '{}'. Supported: {}",
                interval,
                conversions::SUPPORTED_TIMEFRAMES.join(", ")
            );
        }

        let arcus_symbol = self.parse_symbol(symbol);
        let to = end_time.unwrap_or_else(Utc::now);
        let to_str = to.timestamp_micros().to_string();
        let mut query: Vec<(&str, &str)> = vec![
            ("market", arcus_symbol.as_str()),
            ("timeframe", interval),
            ("to", &to_str),
        ];

        let from_str;
        let countback_str;
        if let Some(start) = start_time {
            from_str = start.timestamp_micros().to_string();
            query.push(("from", &from_str));
        } else {
            countback_str = limit.unwrap_or(100).to_string();
            query.push(("countback", &countback_str));
        }

        let resp: CandlesResponse = self.get("/v1/candles", &query).await?;
        let normalized = self.normalize_symbol(&arcus_symbol);
        let interval_owned = interval.to_string();

        let mut klines: Vec<Kline> = resp
            .candles
            .iter()
            .map(|c| conversions::candle_to_kline(c, normalized.clone(), interval_owned.clone()))
            .collect::<Result<Vec<_>>>()?;

        if let Some(lim) = limit {
            klines.truncate(lim as usize);
        }

        Ok(klines)
    }

    async fn get_recent_trades(&self, symbol: &str, limit: u32) -> Result<Vec<Trade>> {
        let arcus_symbol = self.parse_symbol(symbol);
        let limit_str = limit.to_string();
        let resp: TradesResponse = self
            .get(
                "/v1/trades",
                &[
                    ("market", arcus_symbol.as_str()),
                    ("limit", limit_str.as_str()),
                ],
            )
            .await?;

        let normalized = self.normalize_symbol(&arcus_symbol);
        Ok(resp
            .trades
            .iter()
            .filter_map(
                |t| match conversions::trade_to_trade(t, normalized.clone()) {
                    Ok(trade) => Some(trade),
                    Err(e) => {
                        tracing::warn!("Arcus: skipping trade for {}: {}", arcus_symbol, e);
                        None
                    }
                },
            )
            .collect())
    }

    async fn get_market_stats(&self, symbol: &str) -> Result<MarketStats> {
        let arcus_symbol = self.parse_symbol(symbol);
        let (markets, captured_at) = self.cached_markets().await?;
        let m = Self::find_market(&markets, &arcus_symbol)?;

        let mut stats = conversions::to_market_stats(m, captured_at)?;
        stats.symbol = self.normalize_symbol(&arcus_symbol);
        Ok(stats)
    }

    /// Maps directly from the cached markets response — unlike `get_all_tickers`, no
    /// per-symbol orderbook request is needed since `MarketStats` has no bid/ask fields.
    async fn get_all_market_stats(&self) -> Result<Vec<MarketStats>> {
        let (markets, captured_at) = self.cached_markets().await?;

        Ok(markets
            .iter()
            .filter(|m| m.status == "ONLINE" && m.market_type == "PERPETUAL")
            .filter_map(|m| match conversions::to_market_stats(m, captured_at) {
                Ok(mut stats) => {
                    stats.symbol = self.normalize_symbol(&m.market_display_name);
                    Some(stats)
                }
                Err(e) => {
                    tracing::warn!(
                        "Arcus: skipping market stats for {}: {}",
                        m.market_display_name,
                        e
                    );
                    None
                }
            })
            .collect())
    }

    async fn is_supported(&self, symbol: &str) -> Result<bool> {
        let normalized = self.normalize_symbol(symbol);
        let markets = self.get_markets().await?;
        Ok(markets
            .iter()
            .any(|m| m.symbol.eq_ignore_ascii_case(&normalized)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_name() {
        assert_eq!(ArcusClient::new().get_name(), "arcus");
    }

    #[test]
    fn test_parse_symbol_global_and_idempotent() {
        let client = ArcusClient::new();
        assert_eq!(client.parse_symbol("BTC"), "BTC-USD");
        assert_eq!(client.parse_symbol("btc"), "BTC-USD");
        assert_eq!(client.parse_symbol("BTC-USD"), "BTC-USD");
    }

    #[test]
    fn test_normalize_symbol_inverse_of_parse() {
        let client = ArcusClient::new();
        assert_eq!(client.normalize_symbol("BTC-USD"), "BTC");
        assert_eq!(client.normalize_symbol(&client.parse_symbol("BTC")), "BTC");
    }

    #[test]
    fn test_non_proxy_commodity_symbols_pass_through_unaliased() {
        // XAU/XAG/XCU/CL are distinct ETF instruments on Arcus, not naming aliases — they
        // must round-trip as themselves, never rewritten to GLD/SLV/CPER/USO.
        let client = ArcusClient::new();
        assert_eq!(client.parse_symbol("XAU"), "XAU-USD");
        assert_eq!(client.parse_symbol("XAG"), "XAG-USD");
        assert_eq!(client.parse_symbol("XCU"), "XCU-USD");
        assert_eq!(client.parse_symbol("CL"), "CL-USD");
    }
}
