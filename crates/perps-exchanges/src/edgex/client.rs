use crate::edgex::conversions::{
    contract_to_market, depth_record_to_orderbook, funding_record_to_funding_rate,
    kline_record_to_kline, ticker_record_to_market_stats, ticker_record_to_open_interest,
    ticker_record_to_ticker, KLINE_INTERVALS,
};
use crate::edgex::types::{
    ContractMeta, DepthRecord, EdgexResponse, FundingRateRecord, MetadataPayload,
    PageDataFundingRate, PageDataKline, TickerRecord,
};
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::future::join_all;
use perps_core::{
    execute_with_retry, FundingRate, IPerps, IPerpsStream, Kline, Market, MarketStats,
    MultiResolutionOrderbook, OpenInterest, RateLimit, RateLimiter, RetryConfig, Ticker, Trade,
    WsOrderbookManager,
};
use reqwest::Client;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};

const BASE_URL: &str = "https://edgex-prod-v2.edgex.exchange";
const METADATA_TTL: Duration = Duration::from_secs(60);
const PRICE_CACHE_TTL: Duration = Duration::from_secs(3);

/// Convert a global symbol (e.g. `"BTC"`) or an already-formatted EdgeX `contractName`
/// (e.g. `"BTCUSDC"`) to EdgeX format. Idempotent: uppercase -> strip a trailing `"USDC"`
/// if present -> resolve alias -> append `"USDC"`.
pub(crate) fn to_edgex_symbol(symbol: &str) -> String {
    let upper = symbol.to_uppercase();
    let base = upper.strip_suffix("USDC").unwrap_or(&upper);
    let resolved = crate::symbol_aliases::resolve_alias("edgex", base);
    format!("{resolved}USDC")
}

/// Convert an EdgeX `contractName` (e.g. `"BTCUSDC"`) back to the global symbol.
pub(crate) fn to_global_symbol(exchange_symbol: &str) -> String {
    let upper = exchange_symbol.to_uppercase();
    let base = upper.strip_suffix("USDC").unwrap_or(&upper);
    crate::symbol_aliases::unresolve_alias("edgex", base).to_string()
}

/// Cached, indexed snapshot of `GET /api/v2/public/meta/getMetaData`.
struct MetadataCache {
    contracts: Vec<ContractMeta>,
    by_name: HashMap<String, ContractMeta>,
    fetched_at: Instant,
}

struct CachedTicker {
    record: TickerRecord,
    fetched_at: Instant,
}

struct CachedDepth {
    record: DepthRecord,
    /// Envelope `responseTime`, milliseconds, used as `Orderbook.timestamp`.
    response_time_ms: i64,
    fetched_at: Instant,
}

/// Process-wide state shared by every `EdgexClient::new()` instance, so the separate
/// clients the `start`/`liquidity` commands create for ticker vs. liquidity tasks enforce
/// one rate limit and share one metadata/price cache rather than each cold-starting alone.
struct EdgexSharedState {
    http: Client,
    base_url: String,
    rate_limiter: Arc<RateLimiter>,
    metadata: RwLock<Option<MetadataCache>>,
    /// Held across a metadata refill so concurrent misses issue one upstream request.
    metadata_refresh: Mutex<()>,
    ticker_cache: RwLock<HashMap<String, CachedTicker>>,
    depth_cache: RwLock<HashMap<(String, i32), CachedDepth>>,
    /// Per-key single-flight locks for ticker/depth cache refills, keyed by a
    /// `"ticker:<contractId>"` / `"depth:<contractId>:<level>"` string.
    refresh_guards: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

static SHARED_STATE: OnceLock<Arc<EdgexSharedState>> = OnceLock::new();

fn shared_state() -> Arc<EdgexSharedState> {
    Arc::clone(SHARED_STATE.get_or_init(|| {
        Arc::new(EdgexSharedState {
            http: Client::new(),
            base_url: BASE_URL.to_string(),
            // EdgeX documents tiered rate limiting but publishes no concrete number for
            // public endpoints; conservative flat limit, mirrors QfexClient's posture.
            rate_limiter: Arc::new(RateLimiter::new(vec![RateLimit::per_second(10)])),
            metadata: RwLock::new(None),
            metadata_refresh: Mutex::new(()),
            ticker_cache: RwLock::new(HashMap::new()),
            depth_cache: RwLock::new(HashMap::new()),
            refresh_guards: Mutex::new(HashMap::new()),
        })
    }))
}

/// Process-wide push-based orderbook cache (see `perps_core::WsOrderbookManager`), enabled
/// only when `DATABASE_URL` is set and `ENABLE_ORDERBOOK_STREAMING=true` - same convention as
/// every other client that offers this (`binance`, `aster`, `kucoin`, `extended`). `None`
/// otherwise, in which case `get_orderbook` always uses the plain REST path below.
///
/// A separate `OnceLock` from `SHARED_STATE`: the initializer here constructs an
/// `EdgexWsClient`, which itself owns an `EdgexClient` (`resolve_contract_id`'s home) - if
/// this lived inside `shared_state()`'s own closure, that inner `EdgexClient::new()` call
/// would re-enter `SHARED_STATE.get_or_init` before the outer call had returned. Calling this
/// function only from `get_orderbook` (never during `EdgexClient::new()`/`shared_state()`
/// construction) means `shared_state()` has always already returned by the time this runs.
static ORDERBOOK_CACHE: OnceLock<Option<Arc<WsOrderbookManager>>> = OnceLock::new();

fn orderbook_cache() -> Option<Arc<WsOrderbookManager>> {
    ORDERBOOK_CACHE
        .get_or_init(|| {
            let enabled = std::env::var("DATABASE_URL").is_ok()
                && std::env::var("ENABLE_ORDERBOOK_STREAMING")
                    .map(|v| v.eq_ignore_ascii_case("true"))
                    .unwrap_or(false);
            if !enabled {
                tracing::debug!("EdgeX: orderbook push-cache disabled, using REST-only mode");
                return None;
            }
            tracing::info!("EdgeX: orderbook push-cache enabled (WebSocket-backed)");
            let ws: Arc<dyn IPerpsStream> = Arc::new(super::ws_client::EdgexWsClient::new());
            Some(Arc::new(WsOrderbookManager::new(
                Arc::new(perps_core::FullOrderbookAdapter(ws)),
                perps_core::WsOrderbookConfig::default(),
                vec![],
            )))
        })
        .clone()
}

/// REST client for the EdgeX perpetuals exchange (`https://edgex-prod-v2.edgex.exchange`).
///
/// All endpoints used here are public; no authentication required.
///
/// Symbol format: global `"BTC"` <-> EdgeX `"BTCUSDC"` (conversion is idempotent; aliasing
/// via `symbol_aliases.toml`'s `[edgex]` section for the four symbols whose EdgeX base name
/// differs from the project's global name).
///
/// EdgeX's price/depth/funding endpoints key on a numeric `contractId`, not the human
/// `contractName` — every such call resolves `contractName -> contractId` via the cached
/// `getMetaData` response first (a pure in-memory lookup, not a second network call).
///
/// EdgeX has no REST trades endpoint, so `get_recent_trades` always returns `Err`.
///
/// `Clone` is cheap (an `Arc` bump) - `EdgexWsClient` holds an owned `EdgexClient` and
/// derives `Clone` itself to match every other WS client's `#[derive(Clone)]` convention.
#[derive(Clone)]
pub struct EdgexClient {
    state: Arc<EdgexSharedState>,
}

impl EdgexClient {
    pub fn new() -> Self {
        Self {
            state: shared_state(),
        }
    }

    /// Perform a rate-limited, retried GET request and unwrap the response envelope,
    /// erroring on a non-`"SUCCESS"` `code` or an absent `data` payload. Returns the
    /// envelope's `responseTime` alongside the payload, since some payloads (`getDepth`)
    /// carry no timestamp of their own.
    async fn get<R>(&self, path: &str, query: &[(&str, &str)]) -> Result<(R, String)>
    where
        R: serde::de::DeserializeOwned + Send + 'static,
    {
        let config = RetryConfig::default();
        let url = format!("{}{}", self.state.base_url, path);
        let http = self.state.http.clone();
        let rate_limiter = self.state.rate_limiter.clone();
        let query: Vec<(String, String)> = query
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();

        let resp: EdgexResponse<R> = execute_with_retry(&config, || {
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
                            tracing::trace!("EdgeX GET {}", url);
                            let resp = http.get(&url).query(&query).send().await?;
                            if !resp.status().is_success() {
                                let status = resp.status();
                                let text = resp.text().await.unwrap_or_default();
                                return Err(anyhow!("HTTP {}: {}", status, text));
                            }
                            resp.json::<EdgexResponse<R>>()
                                .await
                                .map_err(|e| anyhow!("JSON deserialize error: {}", e))
                        }
                    })
                    .await
            }
        })
        .await?;

        if resp.code != "SUCCESS" {
            return Err(anyhow!(
                "EdgeX API error for {}: code={} msg={:?} errorParam={:?}",
                path,
                resp.code,
                resp.msg,
                resp.error_param
            ));
        }
        let data = resp
            .data
            .ok_or_else(|| anyhow!("EdgeX API returned success with no data for {}", path))?;
        Ok((data, resp.response_time))
    }

    // ---- Metadata cache --------------------------------------------------------

    /// Ensure `self.state.metadata` holds a snapshot no older than `METADATA_TTL`,
    /// refreshing from `getMetaData` on first use or expiry.
    async fn ensure_metadata(&self) -> Result<()> {
        {
            let guard = self.state.metadata.read().await;
            if let Some(c) = guard.as_ref() {
                if c.fetched_at.elapsed() < METADATA_TTL {
                    return Ok(());
                }
            }
        }

        // Single-flight: only the first caller past this point refetches; everyone else
        // waits for the lock, then re-checks and finds a fresh cache already in place.
        let _permit = self.state.metadata_refresh.lock().await;
        {
            let guard = self.state.metadata.read().await;
            if let Some(c) = guard.as_ref() {
                if c.fetched_at.elapsed() < METADATA_TTL {
                    return Ok(());
                }
            }
        }

        tracing::debug!("EdgeX: fetching /api/v2/public/meta/getMetaData");
        let (payload, _rt): (MetadataPayload, String) = self
            .get("/api/v2/public/meta/getMetaData", &[])
            .await
            .context("failed to fetch getMetaData")?;

        let mut by_name = HashMap::with_capacity(payload.contract_list.len());
        for c in &payload.contract_list {
            by_name.insert(c.contract_name.clone(), c.clone());
        }

        let mut guard = self.state.metadata.write().await;
        *guard = Some(MetadataCache {
            contracts: payload.contract_list,
            by_name,
            fetched_at: Instant::now(),
        });
        tracing::debug!(
            "EdgeX: cached {} contracts",
            guard.as_ref().map(|c| c.contracts.len()).unwrap_or(0)
        );
        Ok(())
    }

    /// Look up a contract by its `contractName` (e.g. `"BTCUSDC"`). `pub(crate)` so
    /// `EdgexWsClient` can resolve `ContractMeta` (for `funding_rate_interval_min`/
    /// `funding_min_rate`/`funding_max_rate`) the same way REST's funding-rate path does,
    /// without a second cache.
    pub(crate) async fn find_contract_by_name(&self, contract_name: &str) -> Result<ContractMeta> {
        self.ensure_metadata().await?;
        let guard = self.state.metadata.read().await;
        guard
            .as_ref()
            .and_then(|c| c.by_name.get(contract_name))
            .cloned()
            .ok_or_else(|| anyhow!("EdgeX: unsupported or unknown symbol ({})", contract_name))
    }

    /// Resolve a global or EdgeX-formatted symbol to its numeric `contractId`, the key
    /// every price/depth/funding/kline endpoint requires. `pub(crate)` so `EdgexWsClient`
    /// (which holds an owned `EdgexClient` purely for this lookup, see `ws_client.rs`) can
    /// reuse the same cached metadata resolution REST already performs, rather than
    /// duplicating it.
    pub(crate) async fn resolve_contract_id(&self, symbol: &str) -> Result<String> {
        let contract_name = to_edgex_symbol(symbol);
        Ok(self
            .find_contract_by_name(&contract_name)
            .await?
            .contract_id)
    }

    /// Return every contract with `enableTrade && enableDisplay`, refreshing metadata first.
    async fn fetch_active_markets(&self) -> Result<Vec<ContractMeta>> {
        self.ensure_metadata().await?;
        let guard = self.state.metadata.read().await;
        Ok(guard
            .as_ref()
            .map(|c| c.contracts.clone())
            .unwrap_or_default()
            .into_iter()
            .filter(|c| c.enable_trade && c.enable_display)
            .collect())
    }

    /// Convert active contracts into `Market`s, warning and skipping any single conversion
    /// failure rather than failing the whole call (matches every other client's
    /// `get_markets` precedent — one malformed entry must not take down the active-market
    /// list, especially at EdgeX's ~170-contract scale).
    async fn active_markets(&self) -> Result<Vec<Market>> {
        let contracts = self.fetch_active_markets().await?;
        Ok(contracts
            .iter()
            .filter_map(|c| match contract_to_market(c) {
                Ok(mut m) => {
                    m.symbol = to_global_symbol(&c.contract_name);
                    Some(m)
                }
                Err(e) => {
                    tracing::warn!("EdgeX: skipping market {}: {}", c.contract_name, e);
                    None
                }
            })
            .collect())
    }

    // ---- Single-flight helper ---------------------------------------------------

    /// Fetch (or lazily create) the per-key single-flight lock used to de-duplicate
    /// concurrent ticker/depth cache refills for the same key.
    async fn key_guard(&self, key: String) -> Arc<Mutex<()>> {
        let mut guards = self.state.refresh_guards.lock().await;
        Arc::clone(
            guards
                .entry(key)
                .or_insert_with(|| Arc::new(Mutex::new(()))),
        )
    }

    // ---- Ticker (cached 3s) -----------------------------------------------------

    async fn ticker_cache_get(&self, contract_id: &str) -> Option<TickerRecord> {
        let guard = self.state.ticker_cache.read().await;
        guard.get(contract_id).and_then(|c| {
            if c.fetched_at.elapsed() < PRICE_CACHE_TTL {
                Some(c.record.clone())
            } else {
                None
            }
        })
    }

    /// Fetch `getTicker` for one `contractId`, reusing a live cache entry when available.
    async fn get_ticker_record(&self, contract_id: &str) -> Result<TickerRecord> {
        if let Some(rec) = self.ticker_cache_get(contract_id).await {
            return Ok(rec);
        }
        let guard = self.key_guard(format!("ticker:{contract_id}")).await;
        let _permit = guard.lock().await;
        if let Some(rec) = self.ticker_cache_get(contract_id).await {
            return Ok(rec);
        }

        let (records, _rt): (Vec<TickerRecord>, String) = self
            .get(
                "/api/v2/public/quote/getTicker",
                &[("contractId", contract_id)],
            )
            .await
            .with_context(|| format!("failed to fetch getTicker for contract {contract_id}"))?;
        let record = records
            .into_iter()
            .find(|r| r.contract_id == contract_id)
            .ok_or_else(|| anyhow!("EdgeX getTicker: no record for contractId {contract_id}"))?;

        self.state.ticker_cache.write().await.insert(
            contract_id.to_string(),
            CachedTicker {
                record: record.clone(),
                fetched_at: Instant::now(),
            },
        );
        Ok(record)
    }

    // ---- Depth (cached 3s, level=200 satisfies a level=15 request) -------------

    async fn depth_cache_get(&self, contract_id: &str, level: i32) -> Option<(DepthRecord, i64)> {
        let guard = self.state.depth_cache.read().await;
        guard.get(&(contract_id.to_string(), level)).and_then(|c| {
            if c.fetched_at.elapsed() < PRICE_CACHE_TTL {
                Some((c.record.clone(), c.response_time_ms))
            } else {
                None
            }
        })
    }

    /// Fetch `getDepth` at the given API `level` (`15` or `200`), reusing a live cache
    /// entry when available. A `level=15` request may be served from a live `level=200`
    /// entry (a strict superset); the reverse is never done.
    async fn get_depth_record(&self, contract_id: &str, level: i32) -> Result<(DepthRecord, i64)> {
        if let Some(hit) = self.depth_cache_get(contract_id, level).await {
            return Ok(hit);
        }
        if level == 15 {
            if let Some(hit) = self.depth_cache_get(contract_id, 200).await {
                return Ok(hit);
            }
        }

        let guard = self.key_guard(format!("depth:{contract_id}:{level}")).await;
        let _permit = guard.lock().await;
        if let Some(hit) = self.depth_cache_get(contract_id, level).await {
            return Ok(hit);
        }
        if level == 15 {
            if let Some(hit) = self.depth_cache_get(contract_id, 200).await {
                return Ok(hit);
            }
        }

        let level_str = level.to_string();
        let (records, response_time): (Vec<DepthRecord>, String) = self
            .get(
                "/api/v2/public/quote/getDepth",
                &[("contractId", contract_id), ("level", &level_str)],
            )
            .await
            .with_context(|| format!("failed to fetch getDepth for contract {contract_id}"))?;
        let record = records
            .into_iter()
            .find(|r| r.contract_id == contract_id)
            .ok_or_else(|| anyhow!("EdgeX getDepth: no record for contractId {contract_id}"))?;
        let response_time_ms: i64 = response_time
            .parse()
            .with_context(|| format!("EdgeX getDepth: invalid responseTime {response_time:?}"))?;

        self.state.depth_cache.write().await.insert(
            (contract_id.to_string(), level),
            CachedDepth {
                record: record.clone(),
                response_time_ms,
                fetched_at: Instant::now(),
            },
        );
        Ok((record, response_time_ms))
    }

    /// API `level` for a requested orderbook depth: `15` covers `depth <= 15` (including
    /// `depth == 0`, meaning "return the default/complete response"), otherwise `200`
    /// (EdgeX's `getDepth.level` only accepts these two values).
    fn depth_to_level(depth: u32) -> i32 {
        if depth == 0 || depth <= 15 {
            15
        } else {
            200
        }
    }
}

impl Default for EdgexClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IPerps for EdgexClient {
    async fn prewarm_streams(&self, symbols: &[String]) -> Result<()> {
        if let Some(cache) = orderbook_cache() {
            cache
                .prewarm(symbols.iter().map(|s| self.normalize_symbol(s)).collect())
                .await?;
        }
        Ok(())
    }

    fn get_name(&self) -> &str {
        "edgex"
    }

    fn parse_symbol(&self, symbol: &str) -> String {
        to_edgex_symbol(symbol)
    }

    fn normalize_symbol(&self, exchange_symbol: &str) -> String {
        to_global_symbol(exchange_symbol)
    }

    /// Returns every active (`enableTrade && enableDisplay`) EdgeX contract.
    async fn get_markets(&self) -> Result<Vec<Market>> {
        tracing::debug!("EdgeX: fetching all markets");
        let markets = self.active_markets().await?;
        tracing::debug!("EdgeX: found {} active markets", markets.len());
        Ok(markets)
    }

    async fn get_market(&self, symbol: &str) -> Result<Market> {
        let contract_name = self.parse_symbol(symbol);
        let meta = self.find_contract_by_name(&contract_name).await?;
        if !(meta.enable_trade && meta.enable_display) {
            return Err(anyhow!("EdgeX: market {contract_name} is not active"));
        }
        let mut market = contract_to_market(&meta)
            .with_context(|| format!("failed to convert market data for {contract_name}"))?;
        market.symbol = self.normalize_symbol(&contract_name);
        Ok(market)
    }

    /// Merges `getTicker` (no bid/ask) with `getDepth(level=15)`'s first level for
    /// top-of-book completeness.
    async fn get_ticker(&self, symbol: &str) -> Result<Ticker> {
        let contract_id = self.resolve_contract_id(symbol).await?;
        tracing::debug!("EdgeX: fetching ticker for {}", contract_id);

        let (ticker, (depth, _rt)) = tokio::try_join!(
            self.get_ticker_record(&contract_id),
            self.get_depth_record(&contract_id, 15)
        )?;

        let mut result = ticker_record_to_ticker(&ticker, &depth)
            .with_context(|| format!("failed to convert ticker for contract {contract_id}"))?;
        result.symbol = self.normalize_symbol(&ticker.contract_name);
        Ok(result)
    }

    /// Fans out `get_ticker` over every active market, bounded by the shared rate limiter.
    /// Individual failures are logged and skipped rather than failing the whole call.
    async fn get_all_tickers(&self) -> Result<Vec<Ticker>> {
        tracing::debug!("EdgeX: fetching all tickers");
        let markets = self.active_markets().await?;

        let futures: Vec<_> = markets
            .iter()
            .map(|m| {
                let symbol = m.symbol.clone();
                async move {
                    match self.get_ticker(&symbol).await {
                        Ok(t) => Some(t),
                        Err(e) => {
                            tracing::warn!("EdgeX: failed to fetch ticker for {}: {}", symbol, e);
                            None
                        }
                    }
                }
            })
            .collect();

        let results = join_all(futures).await;
        Ok(results.into_iter().flatten().collect())
    }

    /// `getDepth`'s `level` only accepts `15`/`200`; `depth` is clamped to the nearest
    /// supported level and the result truncated client-side to the caller's request.
    ///
    /// When the WS-backed orderbook push-cache is enabled (`ENABLE_ORDERBOOK_STREAMING=true`
    /// + `DATABASE_URL` set), this serves from it instead: subscribes the symbol (idempotent,
    /// auto-starts a background WS connection on first use) and returns the cached book on a
    /// hit, falling back to the REST path below on a miss (not yet subscribed, or no frame has
    /// arrived yet - e.g. immediately after process start).
    async fn get_orderbook(&self, symbol: &str, depth: u32) -> Result<MultiResolutionOrderbook> {
        if let Some(cache) = orderbook_cache() {
            let normalized = self.normalize_symbol(symbol);
            cache.subscribe(normalized.clone()).await?;
            if let Some(orderbook) = cache.get(&normalized, depth).await {
                return Ok(MultiResolutionOrderbook::from_single(orderbook));
            }
            tracing::debug!(
                "EdgeX: orderbook push-cache miss for {}, falling back to REST",
                normalized
            );
        }

        let contract_id = self.resolve_contract_id(symbol).await?;
        let level = Self::depth_to_level(depth);
        tracing::debug!(
            "EdgeX: fetching orderbook for {} depth={} level={}",
            contract_id,
            depth,
            level
        );

        let (record, response_time_ms) = self.get_depth_record(&contract_id, level).await?;
        let truncate_to = if depth == 0 {
            None
        } else {
            Some(depth as usize)
        };
        let mut orderbook = depth_record_to_orderbook(&record, response_time_ms, truncate_to)
            .with_context(|| format!("failed to convert orderbook for contract {contract_id}"))?;
        orderbook.symbol = self.normalize_symbol(symbol);
        Ok(MultiResolutionOrderbook::from_single(orderbook))
    }

    /// Maps `getLatestFundingRate`; falls back to the settled `fundingRate` when
    /// `forecastFundingRate` is an empty string (settlement rows never carry a forecast).
    async fn get_funding_rate(&self, symbol: &str) -> Result<FundingRate> {
        let contract_id = self.resolve_contract_id(symbol).await?;
        tracing::debug!("EdgeX: fetching funding rate for {}", contract_id);

        let (records, _rt): (Vec<FundingRateRecord>, String) = self
            .get(
                "/api/v2/public/funding/getLatestFundingRate",
                &[("contractId", contract_id.as_str())],
            )
            .await
            .with_context(|| format!("failed to fetch funding rate for {contract_id}"))?;
        let record = records
            .into_iter()
            .find(|r| r.contract_id == contract_id)
            .ok_or_else(|| anyhow!("EdgeX: no funding rate record for contract {contract_id}"))?;

        let meta = self.find_contract_by_name(&to_edgex_symbol(symbol)).await?;
        // The record carries its own interval; fall back to contract metadata only if absent.
        let interval_min = record
            .funding_rate_interval_min
            .clone()
            .unwrap_or_else(|| meta.funding_rate_interval_min.clone());

        let mut fr = funding_record_to_funding_rate(&record, &interval_min, &meta)
            .with_context(|| format!("failed to convert funding rate for {contract_id}"))?;
        fr.symbol = self.normalize_symbol(symbol);
        Ok(fr)
    }

    /// Always sends `filterSettlementFundingRate=true` — without it EdgeX returns
    /// minute-level non-settlement calculations, not real funding events. Pages through
    /// `nextPageOffsetData` until `limit` unique settlement records are collected.
    async fn get_funding_rate_history(
        &self,
        symbol: &str,
        start_time: Option<DateTime<Utc>>,
        end_time: Option<DateTime<Utc>>,
        limit: Option<u32>,
    ) -> Result<Vec<FundingRate>> {
        let contract_id = self.resolve_contract_id(symbol).await?;
        let want = limit.unwrap_or(100) as usize;
        if want == 0 {
            return Ok(Vec::new());
        }
        if let (Some(s), Some(e)) = (start_time, end_time) {
            if s > e {
                return Err(anyhow!("start_time must not be after end_time"));
            }
        }
        tracing::debug!(
            "EdgeX: fetching funding rate history for {} limit={}",
            contract_id,
            want
        );

        let begin_str = start_time.map(|t| t.timestamp_millis().to_string());
        let end_str = end_time.map(|t| t.timestamp_millis().to_string());
        let normalized_sym = self.normalize_symbol(symbol);

        let mut collected: Vec<FundingRateRecord> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut offset = String::new();
        loop {
            let page_size = 100usize.min(want.saturating_sub(collected.len()).max(1));
            let size_str = page_size.to_string();
            let mut query: Vec<(&str, &str)> = vec![
                ("contractId", contract_id.as_str()),
                ("size", size_str.as_str()),
                ("filterSettlementFundingRate", "true"),
            ];
            if let Some(ref b) = begin_str {
                query.push(("filterBeginTimeInclusive", b.as_str()));
            }
            if let Some(ref e) = end_str {
                query.push(("filterEndTimeExclusive", e.as_str()));
            }
            if !offset.is_empty() {
                query.push(("offsetData", offset.as_str()));
            }

            let (page, _rt): (PageDataFundingRate, String) = self
                .get("/api/v2/public/funding/getFundingRatePage", &query)
                .await
                .with_context(|| format!("failed to fetch funding history for {contract_id}"))?;

            let mut progressed = false;
            for row in page.data_list {
                let dedup_key = format!("{}:{}", row.contract_id, row.funding_time);
                if seen.insert(dedup_key) {
                    collected.push(row);
                    progressed = true;
                }
            }

            if page.next_page_offset_data.is_empty() || collected.len() >= want {
                break;
            }
            if !progressed && page.next_page_offset_data == offset {
                return Err(anyhow!(
                    "EdgeX getFundingRatePage: pagination cursor did not advance for {contract_id}"
                ));
            }
            offset = page.next_page_offset_data;
        }

        // Newest-first.
        collected.sort_by(|a, b| b.funding_time.cmp(&a.funding_time));
        collected.truncate(want);

        let meta = self.find_contract_by_name(&to_edgex_symbol(symbol)).await?;
        let mut rates: Vec<FundingRate> = Vec::with_capacity(collected.len());
        for row in &collected {
            let interval_min = row
                .funding_rate_interval_min
                .clone()
                .unwrap_or_else(|| meta.funding_rate_interval_min.clone());
            let mut fr =
                funding_record_to_funding_rate(row, &interval_min, &meta).with_context(|| {
                    format!("failed to convert funding history row for {contract_id}")
                })?;
            fr.symbol = normalized_sym.clone();
            rates.push(fr);
        }
        Ok(rates)
    }

    /// Derives `open_interest_notional`-equivalent (`OpenInterest.open_value`) as
    /// `open_interest * mark_price`, since EdgeX exposes no notional field directly.
    async fn get_open_interest(&self, symbol: &str) -> Result<OpenInterest> {
        let contract_id = self.resolve_contract_id(symbol).await?;
        tracing::debug!("EdgeX: fetching open interest for {}", contract_id);

        let ticker = self.get_ticker_record(&contract_id).await?;
        let mut oi = ticker_record_to_open_interest(&ticker)
            .with_context(|| format!("failed to convert open interest for {contract_id}"))?;
        oi.symbol = self.normalize_symbol(&ticker.contract_name);
        Ok(oi)
    }

    /// Maps the project's standard interval strings to EdgeX's `klineType` enum; rejects
    /// anything else. Always `priceType=LAST_PRICE`. EdgeX has no close timestamp, so
    /// `close_time` is derived from `klineTime + interval - 1ms`; results are returned
    /// oldest-first for the repository's backfill loop.
    async fn get_klines(
        &self,
        symbol: &str,
        interval: &str,
        start_time: Option<DateTime<Utc>>,
        end_time: Option<DateTime<Utc>>,
        limit: Option<u32>,
    ) -> Result<Vec<Kline>> {
        let contract_id = self.resolve_contract_id(symbol).await?;
        let kline_type = KLINE_INTERVALS
            .iter()
            .find(|(k, _, _)| *k == interval)
            .map(|(_, v, _)| *v)
            .ok_or_else(|| anyhow!("EdgeX: unsupported kline interval: {interval}"))?;
        tracing::debug!(
            "EdgeX: fetching klines for {} interval={}",
            contract_id,
            interval
        );

        let size = limit.unwrap_or(1000).min(1000);
        if size == 0 {
            return Ok(Vec::new());
        }
        let size_str = size.to_string();
        let begin_str = start_time.map(|t| t.timestamp_millis().to_string());
        let end_str = end_time.map(|t| t.timestamp_millis().to_string());

        let mut query: Vec<(&str, &str)> = vec![
            ("contractId", contract_id.as_str()),
            ("priceType", "LAST_PRICE"),
            ("klineType", kline_type),
            ("size", size_str.as_str()),
        ];
        if let Some(ref b) = begin_str {
            query.push(("filterBeginKlineTimeInclusive", b.as_str()));
        }
        if let Some(ref e) = end_str {
            query.push(("filterEndKlineTimeExclusive", e.as_str()));
        }

        let (page, _rt): (PageDataKline, String) = self
            .get("/api/v2/public/quote/getKline", &query)
            .await
            .with_context(|| format!("failed to fetch klines for {contract_id}"))?;

        let normalized_sym = self.normalize_symbol(symbol);
        let mut klines: Vec<Kline> = page
            .data_list
            .iter()
            .filter_map(|k| match kline_record_to_kline(k, interval) {
                Ok(mut kl) => {
                    kl.symbol = normalized_sym.clone();
                    Some(kl)
                }
                Err(e) => {
                    tracing::warn!("EdgeX: skipping kline for {}: {}", contract_id, e);
                    None
                }
            })
            .collect();

        // EdgeX returns newest-first; the repository's backfill loop expects oldest-first.
        klines.sort_by(|a, b| a.open_time.cmp(&b.open_time));
        klines.truncate(size as usize);
        Ok(klines)
    }

    /// EdgeX has no REST trades endpoint — only aggregate `trades` counts are exposed via
    /// ticker/kline data, insufficient to construct a `Trade`. Mirrors the identical
    /// precedent at `crates/perps-exchanges/src/qfex/client.rs`.
    async fn get_recent_trades(&self, _symbol: &str, _limit: u32) -> Result<Vec<Trade>> {
        Err(anyhow!(
            "EdgeX does not provide a REST trades endpoint; only aggregate trade counts are exposed via ticker/kline data"
        ))
    }

    async fn get_market_stats(&self, symbol: &str) -> Result<MarketStats> {
        let contract_id = self.resolve_contract_id(symbol).await?;
        tracing::debug!("EdgeX: fetching market stats for {}", contract_id);

        let ticker = self.get_ticker_record(&contract_id).await?;
        let mut ms = ticker_record_to_market_stats(&ticker)
            .with_context(|| format!("failed to convert market stats for {contract_id}"))?;
        ms.symbol = self.normalize_symbol(&ticker.contract_name);
        Ok(ms)
    }

    /// Fans out over every active market via the shared ticker cache, same pattern as
    /// `get_all_tickers`.
    async fn get_all_market_stats(&self) -> Result<Vec<MarketStats>> {
        tracing::debug!("EdgeX: fetching all market stats");
        let markets = self.active_markets().await?;

        let futures: Vec<_> = markets
            .iter()
            .map(|m| {
                let symbol = m.symbol.clone();
                async move {
                    match self.get_market_stats(&symbol).await {
                        Ok(ms) => Some(ms),
                        Err(e) => {
                            tracing::warn!(
                                "EdgeX: failed to fetch market stats for {}: {}",
                                symbol,
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
        let markets = self.active_markets().await?;
        let normalized = self.normalize_symbol(symbol);
        Ok(markets.iter().any(|m| m.symbol == normalized))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_edgex_symbol_direct() {
        assert_eq!(to_edgex_symbol("BTC"), "BTCUSDC");
        assert_eq!(to_edgex_symbol("btc"), "BTCUSDC");
    }

    #[test]
    fn test_to_edgex_symbol_idempotent() {
        assert_eq!(to_edgex_symbol("BTCUSDC"), to_edgex_symbol("BTC"));
    }

    #[test]
    fn test_to_global_symbol_direct() {
        assert_eq!(to_global_symbol("BTCUSDC"), "BTC");
    }

    #[test]
    fn test_depth_to_level() {
        assert_eq!(EdgexClient::depth_to_level(0), 15);
        assert_eq!(EdgexClient::depth_to_level(15), 15);
        assert_eq!(EdgexClient::depth_to_level(16), 200);
        assert_eq!(EdgexClient::depth_to_level(1000), 200);
    }

    #[test]
    fn test_get_name() {
        let client = EdgexClient::new();
        assert_eq!(client.get_name(), "edgex");
    }
}
