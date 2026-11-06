use crate::bitget::conversions::*;
use crate::bitget::types::*;
use crate::bitget::ws_client::BitgetWsClient;
use crate::cache::SymbolsCache;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use perps_core::types::*;
use perps_core::{
    execute_with_retry, FullOrderbookAdapter, IPerps, RateLimiter, RetryConfig, WsOrderbookManager,
};
use std::sync::Arc;

const BASE_URL: &str = "https://api.bitget.com";
const CATEGORY: &str = "USDT-FUTURES";

/// A client for the Bitget exchange (USDT-margined perpetual futures only).
#[derive(Clone)]
pub struct BitgetClient {
    http: reqwest::Client,
    /// Cached set of supported symbols
    symbols_cache: SymbolsCache,
    /// Rate limiter for API calls
    rate_limiter: Arc<RateLimiter>,
    /// Present when `ENABLE_ORDERBOOK_STREAMING=true` (and `DATABASE_URL` is set). When
    /// present, `get_orderbook` prefers a persistent WebSocket connection via
    /// `WsOrderbookManager`, falling back to REST on a cache miss.
    orderbook_manager: Option<Arc<WsOrderbookManager>>,
}

impl BitgetClient {
    pub fn new() -> Self {
        let orderbook_manager = if std::env::var("DATABASE_URL").is_ok()
            && std::env::var("ENABLE_ORDERBOOK_STREAMING")
                .map(|v| v.to_lowercase() == "true")
                .unwrap_or(false)
        {
            tracing::info!("BitgetClient: WebSocket orderbook streaming enabled");
            Some(Arc::new(WsOrderbookManager::new(
                Arc::new(FullOrderbookAdapter(Arc::new(BitgetWsClient::new()))),
                Default::default(),
                vec![],
            )))
        } else {
            None
        };
        Self {
            http: reqwest::Client::new(),
            symbols_cache: SymbolsCache::new(),
            rate_limiter: Arc::new(RateLimiter::bitget()),
            orderbook_manager,
        }
    }

    /// Ensure the symbols cache is initialized
    ///
    /// Cache holds exchange-format symbols (e.g. "BTCUSDT"), matching what
    /// `is_supported` looks up via `parse_symbol`.
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

    /// Takes a normalized global symbol (not exchange format) since it's shared between
    /// the REST path and the WS-manager's fallback closure, which only has the normalized
    /// symbol on hand.
    async fn fetch_orderbook_rest(&self, normalized_symbol: &str, depth: u32) -> Result<Orderbook> {
        let exch_symbol = self.parse_symbol(normalized_symbol);
        let data: OrderbookData = self
            .get(&format!(
                "/api/v3/market/orderbook?category={}&symbol={}&limit={}",
                CATEGORY, exch_symbol, depth
            ))
            .await?;

        let mut orderbook = orderbook_data_to_orderbook(&data, normalized_symbol.to_string())?;
        // Defensive sort: bids descending, asks ascending (per house convention; API
        // already returns this order, but don't rely on it).
        orderbook.bids.sort_by(|a, b| b.price.cmp(&a.price));
        orderbook.asks.sort_by(|a, b| a.price.cmp(&b.price));
        Ok(orderbook)
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, endpoint: &str) -> Result<T> {
        let config = RetryConfig::default();
        let url = format!("{}{}", BASE_URL, endpoint);
        let http = self.http.clone();
        let rate_limiter = self.rate_limiter.clone();

        execute_with_retry(&config, || {
            let url = url.clone();
            let http = http.clone();
            let rate_limiter = rate_limiter.clone();
            async move {
                rate_limiter
                    .execute(|| {
                        let url = url.clone();
                        let http = http.clone();
                        async move {
                            let response = http.get(&url).send().await?;
                            if !response.status().is_success() {
                                return Err(anyhow!(
                                    "GET request to {} failed with status: {}",
                                    url,
                                    response.status()
                                ));
                            }
                            let wrapper: BitgetResponse<T> = response.json().await?;
                            if wrapper.code != "00000" {
                                return Err(anyhow!(
                                    "Bitget API error: code {} - {}",
                                    wrapper.code,
                                    wrapper.msg
                                ));
                            }
                            Ok(wrapper.data)
                        }
                    })
                    .await
            }
        })
        .await
    }
}

impl Default for BitgetClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IPerps for BitgetClient {
    fn get_name(&self) -> &str {
        "bitget"
    }

    fn parse_symbol(&self, symbol: &str) -> String {
        let symbol = crate::symbol_aliases::resolve_alias("bitget", symbol);
        let upper = symbol.to_uppercase();

        let suffixes = ["USDT", "USDC", "BTC", "ETH"];
        for suffix in suffixes {
            if upper.ends_with(suffix) && upper.len() > suffix.len() {
                return upper;
            }
        }
        format!("{}USDT", upper)
    }

    fn normalize_symbol(&self, exchange_symbol: &str) -> String {
        let upper = exchange_symbol.to_uppercase();
        let base = upper
            .strip_suffix("USDT")
            .or_else(|| upper.strip_suffix("USDC"))
            .unwrap_or(&upper);
        crate::symbol_aliases::unresolve_alias("bitget", base).to_string()
    }

    async fn is_supported(&self, symbol: &str) -> Result<bool> {
        self.ensure_cache_initialized().await?;
        Ok(self
            .symbols_cache
            .contains(&self.parse_symbol(symbol))
            .await)
    }

    async fn get_markets(&self) -> Result<Vec<Market>> {
        let instruments: Vec<Instrument> = self
            .get(&format!("/api/v3/market/instruments?category={}", CATEGORY))
            .await?;

        instruments
            .iter()
            .filter(|i| i.status == "online")
            .map(|i| instrument_to_market(i, self.normalize_symbol(&i.symbol)))
            .collect()
    }

    async fn get_market(&self, symbol: &str) -> Result<Market> {
        let exch_symbol = self.parse_symbol(symbol);
        let instruments: Vec<Instrument> = self
            .get(&format!(
                "/api/v3/market/instruments?category={}&symbol={}",
                CATEGORY, exch_symbol
            ))
            .await?;

        let instrument = instruments
            .into_iter()
            .find(|i| i.symbol == exch_symbol)
            .ok_or_else(|| anyhow!("Market {} not found", exch_symbol))?;

        instrument_to_market(&instrument, self.normalize_symbol(&instrument.symbol))
    }

    async fn get_ticker(&self, symbol: &str) -> Result<Ticker> {
        let exch_symbol = self.parse_symbol(symbol);
        let tickers: Vec<TickerData> = self
            .get(&format!(
                "/api/v3/market/tickers?category={}&symbol={}",
                CATEGORY, exch_symbol
            ))
            .await?;

        let ticker = tickers
            .into_iter()
            .find(|t| t.symbol == exch_symbol)
            .ok_or_else(|| anyhow!("Ticker for {} not found", exch_symbol))?;

        ticker_data_to_ticker(&ticker, self.normalize_symbol(&ticker.symbol))
    }

    async fn get_all_tickers(&self) -> Result<Vec<Ticker>> {
        let tickers: Vec<TickerData> = self
            .get(&format!("/api/v3/market/tickers?category={}", CATEGORY))
            .await?;

        Ok(tickers
            .iter()
            .filter_map(|t| ticker_data_to_ticker(t, self.normalize_symbol(&t.symbol)).ok())
            .collect())
    }

    async fn get_orderbook(&self, symbol: &str, depth: u32) -> Result<MultiResolutionOrderbook> {
        let depth = depth.clamp(1, 1000);
        let normalized = self.normalize_symbol(symbol);

        if let Some(manager) = &self.orderbook_manager {
            let client = self.clone();
            let fallback_symbol = normalized.clone();
            let book = manager
                .get_orderbook(&normalized, depth, || async move {
                    let ob = client.fetch_orderbook_rest(&fallback_symbol, depth).await?;
                    Ok((ob, 0))
                })
                .await?;
            return Ok(MultiResolutionOrderbook::from_single(book));
        }

        let orderbook = self.fetch_orderbook_rest(&normalized, depth).await?;
        Ok(MultiResolutionOrderbook::from_single(orderbook))
    }

    async fn get_recent_trades(&self, symbol: &str, limit: u32) -> Result<Vec<Trade>> {
        let exch_symbol = self.parse_symbol(symbol);
        let fills: Vec<FillData> = self
            .get(&format!(
                "/api/v3/market/fills?category={}&symbol={}&limit={}",
                CATEGORY, exch_symbol, limit
            ))
            .await?;

        let global_symbol = self.normalize_symbol(&exch_symbol);
        fills
            .iter()
            .map(|f| fill_to_trade(f, global_symbol.clone()))
            .collect()
    }

    async fn get_funding_rate(&self, symbol: &str) -> Result<FundingRate> {
        let exch_symbol = self.parse_symbol(symbol);
        let funding: Vec<FundingRateData> = self
            .get(&format!(
                "/api/v3/market/current-fund-rate?category={}&symbol={}",
                CATEGORY, exch_symbol
            ))
            .await?;

        let data = funding
            .into_iter()
            .find(|f| f.symbol == exch_symbol)
            .ok_or_else(|| anyhow!("Funding rate for {} not found", exch_symbol))?;

        funding_data_to_funding_rate(&data, self.normalize_symbol(&data.symbol))
    }

    async fn get_funding_rate_history(
        &self,
        symbol: &str,
        start_time: Option<chrono::DateTime<chrono::Utc>>,
        end_time: Option<chrono::DateTime<chrono::Utc>>,
        limit: Option<u32>,
    ) -> Result<Vec<FundingRate>> {
        let exch_symbol = self.parse_symbol(symbol);
        // `startTime`/`endTime` are live-confirmed to be ignored by this endpoint — it
        // only returns the most recent `limit` entries, so the window is applied
        // client-side after fetching the max available page.
        let fetch_limit = limit.unwrap_or(100).min(100);
        let result: HistoryFundResult = self
            .get(&format!(
                "/api/v3/market/history-fund-rate?category={}&symbol={}&limit={}",
                CATEGORY, exch_symbol, fetch_limit
            ))
            .await?;

        let global_symbol = self.normalize_symbol(&exch_symbol);
        let mut rates = result
            .result_list
            .iter()
            .map(|e| history_entry_to_funding_rate(e, global_symbol.clone()))
            .collect::<Result<Vec<_>>>()?;

        if let Some(start) = start_time {
            rates.retain(|r| r.funding_time >= start);
        }
        if let Some(end) = end_time {
            rates.retain(|r| r.funding_time <= end);
        }
        if let Some(lim) = limit {
            rates.truncate(lim as usize);
        }

        Ok(rates)
    }

    async fn get_open_interest(&self, symbol: &str) -> Result<OpenInterest> {
        // Reuses the already-fetched `tickers` payload (holdingAmount/markPrice) rather
        // than a dedicated endpoint — see docs/plan/bitget-integration/02_phase_2_rest_extended.md.
        let ticker = self.get_ticker(symbol).await?;
        Ok(OpenInterest {
            symbol: ticker.symbol,
            open_interest: ticker.open_interest,
            open_value: ticker.open_interest_notional,
            timestamp: ticker.timestamp,
        })
    }

    async fn get_klines(
        &self,
        symbol: &str,
        interval: &str,
        start_time: Option<chrono::DateTime<chrono::Utc>>,
        end_time: Option<chrono::DateTime<chrono::Utc>>,
        limit: Option<u32>,
    ) -> Result<Vec<Kline>> {
        let exch_symbol = self.parse_symbol(symbol);
        // Confirmed Bitget enum (no `1w`): 1m,3m,5m,15m,30m,1H,4H,6H,12H,1D.
        let (bitget_interval, duration) = match interval {
            "1m" => ("1m", chrono::Duration::minutes(1)),
            "5m" => ("5m", chrono::Duration::minutes(5)),
            "15m" => ("15m", chrono::Duration::minutes(15)),
            "30m" => ("30m", chrono::Duration::minutes(30)),
            "1h" => ("1H", chrono::Duration::hours(1)),
            "4h" => ("4H", chrono::Duration::hours(4)),
            "1d" => ("1D", chrono::Duration::days(1)),
            other => anyhow::bail!(
                "unsupported kline interval for bitget: {} (supported: 1m,5m,15m,30m,1h,4h,1d)",
                other
            ),
        };

        let mut endpoint = format!(
            "/api/v3/market/candles?category={}&symbol={}&interval={}",
            CATEGORY, exch_symbol, bitget_interval
        );
        if let Some(start) = start_time {
            endpoint.push_str(&format!("&startTime={}", start.timestamp_millis()));
        }
        if let Some(end) = end_time {
            endpoint.push_str(&format!("&endTime={}", end.timestamp_millis()));
        }
        if let Some(lim) = limit {
            endpoint.push_str(&format!("&limit={}", lim));
        }

        let rows: Vec<Vec<String>> = self.get(&endpoint).await?;
        let global_symbol = self.normalize_symbol(&exch_symbol);
        rows.iter()
            .map(|row| candle_row_to_kline(row, global_symbol.clone(), interval, duration))
            .collect()
    }

    async fn get_market_stats(&self, symbol: &str) -> Result<MarketStats> {
        let exch_symbol = self.parse_symbol(symbol);
        let tickers: Vec<TickerData> = self
            .get(&format!(
                "/api/v3/market/tickers?category={}&symbol={}",
                CATEGORY, exch_symbol
            ))
            .await?;

        let ticker = tickers
            .into_iter()
            .find(|t| t.symbol == exch_symbol)
            .ok_or_else(|| anyhow!("Ticker for {} not found", exch_symbol))?;

        ticker_data_to_market_stats(&ticker, self.normalize_symbol(&ticker.symbol))
    }

    async fn get_all_market_stats(&self) -> Result<Vec<MarketStats>> {
        let tickers: Vec<TickerData> = self
            .get(&format!("/api/v3/market/tickers?category={}", CATEGORY))
            .await?;

        Ok(tickers
            .iter()
            .filter_map(|t| ticker_data_to_market_stats(t, self.normalize_symbol(&t.symbol)).ok())
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_symbol() {
        let client = BitgetClient::new();
        assert_eq!(client.parse_symbol("BTC"), "BTCUSDT");
        assert_eq!(client.parse_symbol("eth"), "ETHUSDT");
        assert_eq!(client.parse_symbol("BTCUSDT"), "BTCUSDT");
    }

    #[test]
    fn test_normalize_symbol() {
        let client = BitgetClient::new();
        assert_eq!(client.normalize_symbol("BTCUSDT"), "BTC");
        assert_eq!(client.normalize_symbol("ethusdt"), "ETH");
    }

    #[test]
    fn test_parse_normalize_round_trip_idempotent() {
        let client = BitgetClient::new();
        for symbol in ["BTC", "ETH", "SOL"] {
            let exch = client.parse_symbol(symbol);
            assert_eq!(client.normalize_symbol(&exch), symbol);
            // idempotent: parse_symbol on an already-exchange-format symbol is a no-op
            assert_eq!(client.parse_symbol(&exch), exch);
        }
    }

    #[test]
    fn test_client_creation() {
        let client = BitgetClient::new();
        assert_eq!(client.get_name(), "bitget");
    }
}
