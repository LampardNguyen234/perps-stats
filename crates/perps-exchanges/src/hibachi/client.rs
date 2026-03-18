use crate::cache::{ContractCache, SymbolsCache};
use crate::hibachi::types::*;
use anyhow::{anyhow, Context, Result};
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

const DATA_BASE_URL: &str = "https://data-api.hibachi.xyz";
/// Responses are reused for this long before a fresh fetch is made.
const RESPONSE_CACHE_TTL: Duration = Duration::from_secs(5);

/// Client for the Hibachi exchange REST API.
///
/// Market data is served from `https://data-api.hibachi.xyz` (no authentication required).
///
/// Symbol format: global `"BTC"` → Hibachi `"BTC/USDT-P"`.
///
/// Rate limit: 300 requests per 10-second sliding window.
pub struct HibachiClient {
    http: Client,
    base_url: String,
    symbols_cache: SymbolsCache,
    /// Maps Hibachi symbol → granularities sorted finest→coarsest (e.g. ["0.1", "1", "10", "100"]).
    granularity_cache: ContractCache<Vec<String>>,
    rate_limiter: Arc<RateLimiter>,
    /// Short-lived response cache: key → (inserted_at, raw JSON body).
    /// Entries older than `RESPONSE_CACHE_TTL` are treated as stale and re-fetched.
    response_cache: Arc<RwLock<HashMap<String, (Instant, String)>>>,
}

impl HibachiClient {
    pub fn new() -> Self {
        Self {
            http: Client::new(),
            base_url: DATA_BASE_URL.to_string(),
            symbols_cache: SymbolsCache::new(),
            granularity_cache: ContractCache::new(),
            rate_limiter: Arc::new(RateLimiter::hibachi()),
            response_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    // ---- symbol conversion ----

    /// Convert a global symbol (e.g. `"BTC"`) to Hibachi format (e.g. `"BTC/USDT-P"`).
    fn to_hibachi_symbol(symbol: &str) -> String {
        let upper = symbol.to_uppercase();
        let base = upper
            .trim_end_matches("/USDT-P")
            .trim_end_matches("USDT")
            .trim_end_matches("-USDT")
            .trim_end_matches("USD")
            .trim_end_matches("-USD");
        format!("{}/USDT-P", base)
    }

    // ---- HTTP helpers ----

    /// Build a canonical cache key from path + sorted query params.
    fn cache_key(path: &str, query: &[(&str, &str)]) -> String {
        let mut pairs: Vec<(&str, &str)> = query.to_vec();
        pairs.sort_unstable_by_key(|(k, _)| *k);
        let qs: Vec<String> = pairs.iter().map(|(k, v)| format!("{}={}", k, v)).collect();
        format!("{}?{}", path, qs.join("&"))
    }

    /// Perform a GET request, return the raw JSON body string.
    /// Respects the rate limiter and retry policy.
    async fn fetch_raw(&self, path: &str, query: &[(&str, &str)]) -> Result<String> {
        let config = RetryConfig::default();
        let url = format!("{}{}", self.base_url, path);
        let http = self.http.clone();
        let rate_limiter = self.rate_limiter.clone();
        let query: Vec<(String, String)> =
            query.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();

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
                            tracing::trace!("Hibachi GET {}", url);
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

    /// GET with a 5-second response cache.
    ///
    /// Repeated calls for the same URL within `RESPONSE_CACHE_TTL` return the cached body
    /// without hitting the network or consuming rate-limit budget.
    async fn get<R>(&self, path: &str, query: &[(&str, &str)]) -> Result<R>
    where
        R: serde::de::DeserializeOwned + Send + 'static,
    {
        let key = Self::cache_key(path, query);

        // Fast path: return cached body if still fresh.
        {
            let cache = self.response_cache.read().await;
            if let Some((inserted_at, body)) = cache.get(&key) {
                if inserted_at.elapsed() < RESPONSE_CACHE_TTL {
                    tracing::trace!("Hibachi cache hit: {}", key);
                    return serde_json::from_str(body)
                        .with_context(|| format!("deserialize cached response for {}", key));
                }
            }
        }

        // Cache miss or stale: fetch and store.
        let body = self.fetch_raw(path, query).await?;
        {
            let mut cache = self.response_cache.write().await;
            cache.insert(key.clone(), (Instant::now(), body.clone()));
        }

        serde_json::from_str(&body)
            .with_context(|| format!("deserialize response for {}{}", self.base_url, path))
    }

    // ---- low-level fetchers ----

    /// Fetch all contracts from `/market/exchange-info` and populate the symbol cache.
    ///
    /// All contracts are cached (not filtered by `live`) so `is_supported` works correctly
    /// regardless of how the exchange sets that flag. Callers that need only live markets
    /// (e.g. `get_markets`) filter the returned vec themselves.
    async fn fetch_exchange_info(&self) -> Result<Vec<FutureContract>> {
        let resp: ExchangeInfoResponse = self.get("/market/exchange-info", &[]).await?;
        let contracts = resp.future_contracts;

        tracing::debug!(
            "Hibachi: fetched {} contracts from exchange-info",
            contracts.len()
        );

        let symbols: std::collections::HashSet<String> =
            contracts.iter().map(|c| c.symbol.clone()).collect();
        self.symbols_cache.initialize(symbols);

        // Cache granularities sorted finest → coarsest (ascending by parsed value).
        let gran_map: std::collections::HashMap<String, Vec<String>> = contracts
            .iter()
            .map(|c| {
                let mut grans = c.orderbook_granularities.clone();
                grans.sort_by(|a, b| {
                    let fa: f64 = a.parse().unwrap_or(f64::MAX);
                    let fb: f64 = b.parse().unwrap_or(f64::MAX);
                    fa.partial_cmp(&fb).unwrap_or(std::cmp::Ordering::Equal)
                });
                (c.symbol.clone(), grans)
            })
            .collect();
        self.granularity_cache.initialize(gran_map);

        Ok(contracts)
    }

    /// Fetch current prices for a Hibachi symbol (e.g. `"BTC/USDT-P"`).
    async fn fetch_prices(&self, hibachi_symbol: &str) -> Result<PricesResponse> {
        self.get("/market/data/prices", &[("symbol", hibachi_symbol)])
            .await
    }

    /// Fetch 24h stats for a Hibachi symbol.
    async fn fetch_stats(&self, hibachi_symbol: &str) -> Result<StatsResponse> {
        self.get("/market/data/stats", &[("symbol", hibachi_symbol)])
            .await
    }

    /// Fetch open interest for a Hibachi symbol.
    async fn fetch_oi(&self, hibachi_symbol: &str) -> Result<OIResponse> {
        self.get("/market/data/open-interest", &[("symbol", hibachi_symbol)])
            .await
    }

    /// Fetch prices, stats, and open interest in parallel (3 API calls).
    /// Used for `get_market_stats` where bid/ask quantities are not needed.
    async fn fetch_ticker_data(
        &self,
        hibachi_symbol: &str,
    ) -> Result<(PricesResponse, StatsResponse, OIResponse)> {
        let (prices, stats, oi) = tokio::try_join!(
            self.fetch_prices(hibachi_symbol),
            self.fetch_stats(hibachi_symbol),
            self.fetch_oi(hibachi_symbol),
        )?;
        Ok((prices, stats, oi))
    }

    /// Fetch prices, stats, open interest, and top-of-book in parallel (4 API calls).
    /// The extra orderbook call provides `best_bid_qty` / `best_ask_qty` for the ticker.
    async fn fetch_full_ticker_data(
        &self,
        hibachi_symbol: &str,
    ) -> Result<(PricesResponse, StatsResponse, OIResponse, OrderbookResponse)> {
        let ob_query = [("symbol", hibachi_symbol), ("depth", "1")];
        let (prices, stats, oi, ob) = tokio::try_join!(
            self.fetch_prices(hibachi_symbol),
            self.fetch_stats(hibachi_symbol),
            self.fetch_oi(hibachi_symbol),
            self.get::<OrderbookResponse>("/market/data/orderbook", &ob_query),
        )?;
        Ok((prices, stats, oi, ob))
    }
}

impl Default for HibachiClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IPerps for HibachiClient {
    fn get_name(&self) -> &str {
        "hibachi"
    }

    /// Convert global symbol to Hibachi format: `"BTC"` → `"BTC/USDT-P"`.
    fn parse_symbol(&self, symbol: &str) -> String {
        Self::to_hibachi_symbol(symbol)
    }

    async fn get_markets(&self) -> Result<Vec<Market>> {
        let contracts = self.fetch_exchange_info().await?;
        contracts
            .iter()
            .filter(|c| c.live)
            .map(super::conversions::future_contract_to_market)
            .collect()
    }

    async fn get_market(&self, symbol: &str) -> Result<Market> {
        let hibachi_symbol = self.parse_symbol(symbol);
        let contracts = self.fetch_exchange_info().await?;

        let contract = contracts
            .iter()
            .find(|c| c.symbol == hibachi_symbol)
            .ok_or_else(|| anyhow!("Market not found: {}", hibachi_symbol))?;

        super::conversions::future_contract_to_market(contract)
    }

    /// Fetches ticker using 4 parallel API calls:
    /// `/prices`, `/stats`, `/open-interest`, `/orderbook?depth=1` (for bid/ask qty).
    async fn get_ticker(&self, symbol: &str) -> Result<Ticker> {
        let hibachi_symbol = self.parse_symbol(symbol);
        tracing::debug!("Hibachi: fetching ticker for {}", hibachi_symbol);

        let (prices, stats, oi, ob) = self.fetch_full_ticker_data(&hibachi_symbol).await?;
        super::conversions::prices_stats_oi_ob_to_ticker(
            &prices,
            &stats,
            &oi,
            &ob,
            symbol.to_string(),
        )
    }

    /// Fetches tickers for all live symbols. Uses N×3 parallel calls.
    async fn get_all_tickers(&self) -> Result<Vec<Ticker>> {
        tracing::debug!("Hibachi: fetching all tickers");

        let contracts = self.fetch_exchange_info().await?;

        let futures: Vec<_> = contracts
            .iter()
            .filter(|c| c.live)
            .map(|c| {
                let hibachi_symbol = c.symbol.clone();
                let global_symbol = c.underlying_symbol.clone();
                async move {
                    match self.fetch_full_ticker_data(&hibachi_symbol).await {
                        Ok((prices, stats, oi, ob)) => {
                            match super::conversions::prices_stats_oi_ob_to_ticker(
                                &prices,
                                &stats,
                                &oi,
                                &ob,
                                global_symbol.clone(),
                            ) {
                                Ok(t) => Some(t),
                                Err(e) => {
                                    tracing::warn!(
                                        "Hibachi: failed to convert ticker {}: {}",
                                        hibachi_symbol,
                                        e
                                    );
                                    None
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                "Hibachi: failed to fetch ticker {}: {}",
                                hibachi_symbol,
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

    async fn get_orderbook(&self, symbol: &str, depth: u32) -> Result<MultiResolutionOrderbook> {
        let hibachi_symbol = self.parse_symbol(symbol);
        tracing::debug!("Hibachi: fetching orderbook for {}", hibachi_symbol);

        // Ensure exchange-info is loaded so granularity cache is populated.
        if !self.symbols_cache.is_initialized() {
            self.fetch_exchange_info().await?;
        }

        // Pick up to 2 finest granularities for this symbol (finest → coarsest order).
        let granularities: Vec<String> = self
            .granularity_cache
            .get(&hibachi_symbol)
            .await
            .unwrap_or_default()
            .into_iter()
            .take(2)
            .collect();

        // Hibachi API enforces depth in [1, 100].
        let depth_clamped = depth.clamp(1, 100);
        let depth_str = depth_clamped.to_string();
        let depth_usize = depth_clamped as usize;

        if granularities.len() >= 2 {
            // Bind query arrays before the join to avoid temporary lifetime issues.
            let query_fine = [
                ("symbol", hibachi_symbol.as_str()),
                ("depth", depth_str.as_str()),
                ("granularity", granularities[0].as_str()),
            ];
            let query_coarse = [
                ("symbol", hibachi_symbol.as_str()),
                ("depth", depth_str.as_str()),
                ("granularity", granularities[1].as_str()),
            ];
            // Fetch both granularities in parallel.
            let (ob_fine, ob_coarse) = tokio::try_join!(
                self.get::<OrderbookResponse>("/market/data/orderbook", &query_fine),
                self.get::<OrderbookResponse>("/market/data/orderbook", &query_coarse),
            )?;

            let fine =
                super::conversions::orderbook_to_orderbook_truncated(ob_fine, symbol, depth_usize)?;
            let coarse = super::conversions::orderbook_to_orderbook_truncated(
                ob_coarse,
                symbol,
                depth_usize,
            )?;

            tracing::debug!(
                "Hibachi: orderbook {} granularities [{}, {}]",
                hibachi_symbol,
                granularities[0],
                granularities[1]
            );

            Ok(MultiResolutionOrderbook::from_multiple(
                symbol.to_string(),
                Utc::now(),
                vec![fine, coarse],
            ))
        } else {
            // Single granularity (or none available) — fall back to no-granularity request.
            let gran_arg = granularities.first().map(|s| s.as_str()).unwrap_or("");
            let mut query = vec![("symbol", hibachi_symbol.as_str()), ("depth", &depth_str)];
            if !gran_arg.is_empty() {
                query.push(("granularity", gran_arg));
            }
            let ob: OrderbookResponse = self.get("/market/data/orderbook", &query).await?;
            let orderbook =
                super::conversions::orderbook_to_orderbook_truncated(ob, symbol, depth_usize)?;
            Ok(MultiResolutionOrderbook::from_single(orderbook))
        }
    }

    async fn get_funding_rate(&self, symbol: &str) -> Result<FundingRate> {
        let hibachi_symbol = self.parse_symbol(symbol);
        tracing::debug!("Hibachi: fetching funding rate for {}", hibachi_symbol);

        let prices = self.fetch_prices(&hibachi_symbol).await?;
        super::conversions::prices_to_funding_rate(&prices, symbol.to_string())
    }

    async fn get_funding_rate_history(
        &self,
        symbol: &str,
        start_time: Option<DateTime<Utc>>,
        end_time: Option<DateTime<Utc>>,
        limit: Option<u32>,
    ) -> Result<Vec<FundingRate>> {
        let hibachi_symbol = self.parse_symbol(symbol);
        tracing::debug!(
            "Hibachi: fetching funding rate history for {}",
            hibachi_symbol
        );

        let limit_str;
        let start_str;
        let end_str;

        let mut query: Vec<(&str, &str)> = vec![("symbol", &hibachi_symbol)];

        if let Some(lim) = limit {
            limit_str = lim.to_string();
            query.push(("limit", &limit_str));
        }
        if let Some(start) = start_time {
            start_str = start.timestamp().to_string();
            query.push(("startTime", &start_str));
        }
        if let Some(end) = end_time {
            end_str = end.timestamp().to_string();
            query.push(("endTime", &end_str));
        }

        let resp: FundingRatesResponse = self.get("/market/data/funding-rates", &query).await?;

        resp.data
            .iter()
            .map(|e| super::conversions::funding_rate_entry_to_funding_rate(e, symbol.to_string()))
            .collect()
    }

    async fn get_open_interest(&self, symbol: &str) -> Result<OpenInterest> {
        let hibachi_symbol = self.parse_symbol(symbol);
        tracing::debug!("Hibachi: fetching open interest for {}", hibachi_symbol);

        let (oi, prices) = tokio::try_join!(
            self.fetch_oi(&hibachi_symbol),
            self.fetch_prices(&hibachi_symbol),
        )?;

        let mark_price = prices
            .mark_price
            .as_deref()
            .and_then(|s| s.parse::<rust_decimal::Decimal>().ok())
            .unwrap_or(rust_decimal::Decimal::ZERO);

        super::conversions::oi_response_to_open_interest(&oi, mark_price, symbol.to_string())
    }

    async fn get_klines(
        &self,
        symbol: &str,
        interval: &str,
        start_time: Option<DateTime<Utc>>,
        end_time: Option<DateTime<Utc>>,
        limit: Option<u32>,
    ) -> Result<Vec<Kline>> {
        let hibachi_symbol = self.parse_symbol(symbol);
        tracing::debug!(
            "Hibachi: fetching klines for {} interval={}",
            hibachi_symbol,
            interval
        );

        let now = Utc::now();
        let from_ms = start_time
            .unwrap_or_else(|| now - chrono::Duration::hours(24))
            .timestamp_millis()
            .to_string();
        let to_ms = end_time.unwrap_or(now).timestamp_millis().to_string();

        let resp: KlinesResponse = self
            .get(
                "/market/data/klines",
                &[
                    ("symbol", hibachi_symbol.as_str()),
                    ("interval", interval),
                    ("fromMs", &from_ms),
                    ("toMs", &to_ms),
                ],
            )
            .await?;

        let interval_str = interval.to_string();
        let mut klines: Vec<Kline> = resp
            .klines
            .iter()
            .map(|k| {
                super::conversions::kline_to_kline(k, symbol.to_string(), interval_str.clone())
            })
            .collect::<Result<Vec<_>>>()?;

        if let Some(lim) = limit {
            let lim = lim as usize;
            if klines.len() > lim {
                klines = klines.into_iter().rev().take(lim).rev().collect();
            }
        }

        Ok(klines)
    }

    async fn get_recent_trades(&self, symbol: &str, limit: u32) -> Result<Vec<Trade>> {
        let hibachi_symbol = self.parse_symbol(symbol);
        tracing::debug!("Hibachi: fetching recent trades for {}", hibachi_symbol);

        let resp: TradesResponse = self
            .get("/market/data/trades", &[("symbol", &hibachi_symbol)])
            .await?;

        let mut trades: Vec<Trade> = resp
            .trades
            .iter()
            .map(|t| super::conversions::trade_entry_to_trade(t, symbol.to_string()))
            .collect::<Result<Vec<_>>>()?;

        trades.truncate(limit as usize);
        Ok(trades)
    }

    /// Fetches market stats using 3 parallel calls: `/prices`, `/stats`, `/open-interest`.
    async fn get_market_stats(&self, symbol: &str) -> Result<MarketStats> {
        let hibachi_symbol = self.parse_symbol(symbol);
        tracing::debug!("Hibachi: fetching market stats for {}", hibachi_symbol);

        let (prices, stats, oi) = self.fetch_ticker_data(&hibachi_symbol).await?;
        super::conversions::prices_stats_oi_to_market_stats(
            &prices,
            &stats,
            &oi,
            symbol.to_string(),
        )
    }

    /// Fetches market stats for all live symbols using N×3 parallel calls.
    async fn get_all_market_stats(&self) -> Result<Vec<MarketStats>> {
        tracing::debug!("Hibachi: fetching all market stats");

        let contracts = self.fetch_exchange_info().await?;

        let futures: Vec<_> = contracts
            .iter()
            .filter(|c| c.live)
            .map(|c| {
                let hibachi_symbol = c.symbol.clone();
                let global_symbol = c.underlying_symbol.clone();
                async move {
                    match self.fetch_ticker_data(&hibachi_symbol).await {
                        Ok((prices, stats, oi)) => {
                            match super::conversions::prices_stats_oi_to_market_stats(
                                &prices,
                                &stats,
                                &oi,
                                global_symbol.clone(),
                            ) {
                                Ok(ms) => Some(ms),
                                Err(e) => {
                                    tracing::warn!(
                                        "Hibachi: failed to convert market stats {}: {}",
                                        hibachi_symbol,
                                        e
                                    );
                                    None
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                "Hibachi: failed to fetch market stats {}: {}",
                                hibachi_symbol,
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
        let hibachi_symbol = self.parse_symbol(symbol);

        self.symbols_cache
            .get_or_init(|| async {
                let contracts = self.fetch_exchange_info().await?;
                Ok(contracts.iter().map(|c| c.symbol.clone()).collect())
            })
            .await?;

        Ok(self.symbols_cache.contains(&hibachi_symbol).await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_symbol() {
        let client = HibachiClient::new();
        assert_eq!(client.parse_symbol("BTC"), "BTC/USDT-P");
        assert_eq!(client.parse_symbol("ETH"), "ETH/USDT-P");
        assert_eq!(client.parse_symbol("btc"), "BTC/USDT-P");
        assert_eq!(client.parse_symbol("BTC/USDT-P"), "BTC/USDT-P");
        assert_eq!(client.parse_symbol("BTCUSDT"), "BTC/USDT-P");
    }

    #[test]
    fn test_get_name() {
        let client = HibachiClient::new();
        assert_eq!(client.get_name(), "hibachi");
    }
}
