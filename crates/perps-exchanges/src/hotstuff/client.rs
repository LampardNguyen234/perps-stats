use crate::cache::SymbolsCache;
use crate::hotstuff::types::*;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use perps_core::{
    execute_with_retry, FundingRate, IPerps, Kline, Market, MarketStats, MultiResolutionOrderbook,
    OpenInterest, RateLimiter, RetryConfig, Ticker, Trade,
};
use reqwest::Client;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

const BASE_URL: &str = "https://api.hotstuff.trade";

/// Client for the Hotstuff exchange REST API.
///
/// All market data is accessed via a single POST endpoint:
/// `POST https://api.hotstuff.trade/info`
/// with body `{ "method": "<method>", "params": { ... } }`.
///
/// Rate limit: ~5000 req/min. Client enforces 80 req/s (safe margin).
pub struct HotstuffClient {
    http: Client,
    base_url: String,
    symbols_cache: SymbolsCache,
    /// Maps Hotstuff symbol (e.g. "BTC-PERP") → numeric instrument id.
    /// Required for the `chart` (klines) endpoint.
    instrument_id_cache: Arc<RwLock<HashMap<String, i64>>>,
    rate_limiter: Arc<RateLimiter>,
}

impl HotstuffClient {
    pub fn new() -> Self {
        Self {
            http: Client::new(),
            base_url: BASE_URL.to_string(),
            symbols_cache: SymbolsCache::new(),
            instrument_id_cache: Arc::new(RwLock::new(HashMap::new())),
            rate_limiter: Arc::new(RateLimiter::hotstuff()),
        }
    }

    // ---- symbol conversion ----

    /// Convert a global symbol (e.g. "BTC") to Hotstuff format (e.g. "BTC-PERP").
    fn to_hotstuff_symbol(symbol: &str) -> String {
        let upper = symbol.to_uppercase();
        // Normalise: strip common quote suffixes, then append -PERP
        let base = upper
            .trim_end_matches("-PERP")
            .trim_end_matches("USDT")
            .trim_end_matches("-USDT")
            .trim_end_matches("USD")
            .trim_end_matches("-USD");
        format!("{}-PERP", base)
    }

    // ---- HTTP helper ----

    /// POST to /info with `{ "method": ..., "params": ... }` and deserialise the response.
    async fn post_info<P, R>(&self, method: &'static str, params: P) -> Result<R>
    where
        P: serde::Serialize + Clone + Send + Sync + 'static,
        R: serde::de::DeserializeOwned + Send + 'static,
    {
        let config = RetryConfig::default();
        let url = format!("{}/info", self.base_url);
        let http = self.http.clone();
        let rate_limiter = self.rate_limiter.clone();

        execute_with_retry(&config, || {
            let url = url.clone();
            let http = http.clone();
            let rate_limiter = rate_limiter.clone();
            let params = params.clone();
            async move {
                rate_limiter
                    .execute(|| {
                        let url = url.clone();
                        let http = http.clone();
                        let params = params.clone();
                        async move {
                            let body = InfoRequest { method, params };
                            tracing::trace!("Hotstuff POST /info method={}", method);
                            let resp = http.post(&url).json(&body).send().await?;

                            if !resp.status().is_success() {
                                let status = resp.status();
                                let text = resp.text().await.unwrap_or_default();
                                return Err(anyhow!("HTTP {}: {}", status, text));
                            }

                            Ok(resp.json::<R>().await?)
                        }
                    })
                    .await
            }
        })
        .await
    }

    // ---- low-level fetchers ----

    /// Fetch all perp instruments and populate caches.
    async fn fetch_instruments(&self) -> Result<Vec<HotstuffInstrument>> {
        let resp: InstrumentsResponse = self
            .post_info(
                "instruments",
                InstrumentsParams {
                    instrument_type: "perps",
                },
            )
            .await?;

        let instruments = resp.perps.unwrap_or_default();

        // Populate both caches from the fresh list
        let symbols: std::collections::HashSet<String> =
            instruments.iter().map(|i| i.name.clone()).collect();
        self.symbols_cache.initialize(symbols);

        let mut id_map = self.instrument_id_cache.write().await;
        for inst in &instruments {
            id_map.insert(inst.name.clone(), inst.id);
        }

        Ok(instruments)
    }

    /// Ensure the instrument caches are populated (no-op if already done).
    async fn ensure_instruments_cached(&self) -> Result<()> {
        if self.symbols_cache.is_initialized() {
            return Ok(());
        }
        self.fetch_instruments().await?;
        Ok(())
    }

    /// Fetch the numeric instrument id for a Hotstuff symbol (e.g. "BTC-PERP").
    async fn get_instrument_id(&self, hotstuff_symbol: &str) -> Result<i64> {
        self.ensure_instruments_cached().await?;
        let id_map = self.instrument_id_cache.read().await;
        id_map
            .get(hotstuff_symbol)
            .copied()
            .ok_or_else(|| anyhow!("Instrument id not found for symbol: {}", hotstuff_symbol))
    }

    /// Fetch ticker for a single symbol ("all" returns all tickers as a Vec).
    async fn fetch_ticker(&self, hotstuff_symbol: &str) -> Result<HotstuffTicker> {
        let tickers: Vec<HotstuffTicker> = self
            .post_info(
                "ticker",
                TickerParams {
                    symbol: hotstuff_symbol.to_string(),
                },
            )
            .await?;

        tickers
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("No ticker returned for {}", hotstuff_symbol))
    }

    /// Fetch all tickers in a single call.
    async fn fetch_all_tickers(&self) -> Result<Vec<HotstuffTicker>> {
        self.post_info(
            "ticker",
            TickerParams {
                symbol: "all".to_string(),
            },
        )
        .await
    }

    /// Map a human-readable interval to Hotstuff resolution string.
    fn map_interval(interval: &str) -> Result<&'static str> {
        match interval {
            "1m" => Ok("1"),
            "5m" => Ok("5"),
            "15m" => Ok("15"),
            "30m" => Ok("30"),
            "1h" => Ok("60"),
            "4h" => Ok("240"),
            "1d" => Ok("1D"),
            "1w" => Ok("1W"),
            other => Err(anyhow!(
                "Unsupported interval '{}'. Supported: 1m, 5m, 15m, 30m, 1h, 4h, 1d, 1w",
                other
            )),
        }
    }
}

impl Default for HotstuffClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IPerps for HotstuffClient {
    fn get_name(&self) -> &str {
        "hotstuff"
    }

    /// Convert global symbol to Hotstuff format: "BTC" → "BTC-PERP".
    fn parse_symbol(&self, symbol: &str) -> String {
        Self::to_hotstuff_symbol(symbol)
    }

    async fn get_markets(&self) -> Result<Vec<Market>> {
        let instruments = self.fetch_instruments().await?;

        instruments
            .iter()
            .filter(|i| !i.delisted)
            .map(|i| {
                // Strip "-PERP" suffix for the global symbol
                let global = i.name.trim_end_matches("-PERP").to_string();
                super::conversions::instrument_to_market(i, global)
            })
            .collect()
    }

    async fn get_market(&self, symbol: &str) -> Result<Market> {
        let hotstuff_symbol = self.parse_symbol(symbol);
        let instruments = self.fetch_instruments().await?;

        let inst = instruments
            .iter()
            .find(|i| i.name == hotstuff_symbol)
            .ok_or_else(|| anyhow!("Market not found: {}", hotstuff_symbol))?;

        super::conversions::instrument_to_market(inst, symbol.to_string())
    }

    async fn get_ticker(&self, symbol: &str) -> Result<Ticker> {
        let hotstuff_symbol = self.parse_symbol(symbol);
        tracing::debug!("Hotstuff: fetching ticker for {}", hotstuff_symbol);

        let ht = self.fetch_ticker(&hotstuff_symbol).await?;
        super::conversions::ticker_to_ticker(&ht, symbol.to_string())
    }

    async fn get_all_tickers(&self) -> Result<Vec<Ticker>> {
        tracing::debug!("Hotstuff: fetching all tickers");

        let tickers = self.fetch_all_tickers().await?;
        let mut result = Vec::new();

        for ht in &tickers {
            let global = ht.symbol.trim_end_matches("-PERP").to_string();
            match super::conversions::ticker_to_ticker(ht, global) {
                Ok(t) => result.push(t),
                Err(e) => tracing::warn!("Hotstuff: failed to convert ticker {}: {}", ht.symbol, e),
            }
        }

        Ok(result)
    }

    async fn get_orderbook(&self, symbol: &str, depth: u32) -> Result<MultiResolutionOrderbook> {
        let hotstuff_symbol = self.parse_symbol(symbol);
        tracing::debug!("Hotstuff: fetching orderbook for {}", hotstuff_symbol);

        let mut ob: crate::hotstuff::types::HotstuffOrderbook = self
            .post_info(
                "orderbook",
                OrderbookParams {
                    symbol: hotstuff_symbol,
                },
            )
            .await?;

        // Truncate to requested depth
        let depth = depth as usize;
        ob.bids.truncate(depth);
        ob.asks.truncate(depth);

        let orderbook = super::conversions::orderbook_to_orderbook(ob, symbol.to_string())?;
        Ok(MultiResolutionOrderbook::from_single(orderbook))
    }

    async fn get_funding_rate(&self, symbol: &str) -> Result<FundingRate> {
        let hotstuff_symbol = self.parse_symbol(symbol);
        tracing::debug!("Hotstuff: fetching funding rate for {}", hotstuff_symbol);

        let ht = self.fetch_ticker(&hotstuff_symbol).await?;
        super::conversions::ticker_to_funding_rate(&ht, symbol.to_string())
    }

    /// Hotstuff does not expose funding rate history via REST.
    /// Returns an empty vector.
    async fn get_funding_rate_history(
        &self,
        _symbol: &str,
        _start_time: Option<DateTime<Utc>>,
        _end_time: Option<DateTime<Utc>>,
        _limit: Option<u32>,
    ) -> Result<Vec<FundingRate>> {
        Ok(vec![])
    }

    async fn get_open_interest(&self, symbol: &str) -> Result<OpenInterest> {
        let hotstuff_symbol = self.parse_symbol(symbol);
        tracing::debug!("Hotstuff: fetching open interest for {}", hotstuff_symbol);

        let ht = self.fetch_ticker(&hotstuff_symbol).await?;
        super::conversions::ticker_to_open_interest(&ht, symbol.to_string())
    }

    async fn get_klines(
        &self,
        symbol: &str,
        interval: &str,
        start_time: Option<DateTime<Utc>>,
        end_time: Option<DateTime<Utc>>,
        limit: Option<u32>,
    ) -> Result<Vec<Kline>> {
        let hotstuff_symbol = self.parse_symbol(symbol);
        let resolution = Self::map_interval(interval)?;
        let instrument_id = self.get_instrument_id(&hotstuff_symbol).await?;

        // from = Unix seconds, to = Unix milliseconds (Hotstuff API inconsistency)
        let now = Utc::now();
        let default_limit = limit.unwrap_or(500) as i64;

        // If no start_time, derive from limit and interval
        let from_secs = start_time
            .unwrap_or_else(|| now - chrono::Duration::seconds(default_limit * 60))
            .timestamp(); // Unix seconds

        let to_ms = end_time.unwrap_or(now).timestamp_millis(); // Unix milliseconds

        tracing::debug!(
            "Hotstuff: fetching klines for {} (id={}) interval={} from={} to={}",
            hotstuff_symbol, instrument_id, resolution, from_secs, to_ms
        );

        let klines: Vec<HotstuffKline> = self
            .post_info(
                "chart",
                ChartParams {
                    symbol: instrument_id.to_string(),
                    chart_type: "ltp".to_string(),
                    resolution: resolution.to_string(),
                    from: from_secs,
                    to: to_ms,
                },
            )
            .await?;

        let interval_str = interval.to_string();
        let result: Vec<Kline> = klines
            .iter()
            .map(|k| super::conversions::kline_to_kline(k, symbol.to_string(), interval_str.clone()))
            .collect::<Result<Vec<_>>>()?;

        // Apply limit (take last N if more than requested)
        let result = if let Some(lim) = limit {
            let lim = lim as usize;
            if result.len() > lim {
                result.into_iter().rev().take(lim).rev().collect()
            } else {
                result
            }
        } else {
            result
        };

        Ok(result)
    }

    async fn get_recent_trades(&self, symbol: &str, limit: u32) -> Result<Vec<Trade>> {
        let hotstuff_symbol = self.parse_symbol(symbol);
        tracing::debug!("Hotstuff: fetching trades for {}", hotstuff_symbol);

        let mut trades: Vec<HotstuffTrade> = self
            .post_info(
                "trades",
                TradesParams {
                    symbol: hotstuff_symbol,
                },
            )
            .await?;

        // Truncate to limit
        trades.truncate(limit as usize);

        trades
            .into_iter()
            .map(|t| super::conversions::trade_to_trade(t, symbol.to_string()))
            .collect()
    }

    async fn get_market_stats(&self, symbol: &str) -> Result<MarketStats> {
        let hotstuff_symbol = self.parse_symbol(symbol);
        tracing::debug!("Hotstuff: fetching market stats for {}", hotstuff_symbol);

        let ht = self.fetch_ticker(&hotstuff_symbol).await?;
        super::conversions::ticker_to_market_stats(&ht, symbol.to_string())
    }

    async fn get_all_market_stats(&self) -> Result<Vec<MarketStats>> {
        tracing::debug!("Hotstuff: fetching all market stats");

        let tickers = self.fetch_all_tickers().await?;
        let mut result = Vec::new();

        for ht in &tickers {
            let global = ht.symbol.trim_end_matches("-PERP").to_string();
            match super::conversions::ticker_to_market_stats(ht, global) {
                Ok(s) => result.push(s),
                Err(e) => {
                    tracing::warn!("Hotstuff: failed to convert market stats {}: {}", ht.symbol, e)
                }
            }
        }

        Ok(result)
    }

    async fn is_supported(&self, symbol: &str) -> Result<bool> {
        let hotstuff_symbol = self.parse_symbol(symbol);

        self.symbols_cache
            .get_or_init(|| async {
                let instruments = self.fetch_instruments().await?;
                Ok(instruments.iter().map(|i| i.name.clone()).collect())
            })
            .await?;

        Ok(self.symbols_cache.contains(&hotstuff_symbol).await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_symbol() {
        let client = HotstuffClient::new();
        assert_eq!(client.parse_symbol("BTC"), "BTC-PERP");
        assert_eq!(client.parse_symbol("ETH"), "ETH-PERP");
        assert_eq!(client.parse_symbol("btc"), "BTC-PERP");
        assert_eq!(client.parse_symbol("BTC-PERP"), "BTC-PERP");
        assert_eq!(client.parse_symbol("BTCUSDT"), "BTC-PERP");
    }

    #[test]
    fn test_get_name() {
        let client = HotstuffClient::new();
        assert_eq!(client.get_name(), "hotstuff");
    }

    #[test]
    fn test_map_interval() {
        assert_eq!(HotstuffClient::map_interval("1m").unwrap(), "1");
        assert_eq!(HotstuffClient::map_interval("5m").unwrap(), "5");
        assert_eq!(HotstuffClient::map_interval("15m").unwrap(), "15");
        assert_eq!(HotstuffClient::map_interval("30m").unwrap(), "30");
        assert_eq!(HotstuffClient::map_interval("1h").unwrap(), "60");
        assert_eq!(HotstuffClient::map_interval("4h").unwrap(), "240");
        assert_eq!(HotstuffClient::map_interval("1d").unwrap(), "1D");
        assert_eq!(HotstuffClient::map_interval("1w").unwrap(), "1W");
        assert!(HotstuffClient::map_interval("2h").is_err());
    }
}
