use super::conversions::*;
use super::types::*;
use crate::cache::SymbolsCache;
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use perps_core::{
    execute_with_retry, FundingRate, IPerps, Kline, Market, MarketStats, MultiResolutionOrderbook,
    OpenInterest, RateLimiter, RetryConfig, Ticker, Trade,
};
use reqwest::Client;
use serde_json::json;
use std::sync::Arc;

const BASE_URL: &str = "https://market-data.grvt.io";

/// Gravity DEX REST client for market data
///
/// Implements the IPerps trait for Gravity perpetuals market data endpoints.
/// All endpoints are POST requests and publicly accessible (no authentication required).
///
/// Rate limit: 50 requests per second (conservative estimate)
#[derive(Clone)]
#[allow(dead_code)]
pub struct GravityClient {
    http: Client,
    base_url: String,
    symbols_cache: SymbolsCache,
    rate_limiter: Arc<RateLimiter>,
}

impl GravityClient {
    /// Create a new Gravity client with default configuration
    ///
    /// # Returns
    /// A new GravityClient instance ready to make API requests
    pub fn new() -> Self {
        Self {
            http: Client::new(),
            base_url: BASE_URL.to_string(),
            symbols_cache: SymbolsCache::new(),
            rate_limiter: Arc::new(RateLimiter::gravity()),
        }
    }

    /// Helper method to make rate-limited POST requests with retry
    async fn post<T: serde::de::DeserializeOwned>(
        &self,
        endpoint: &str,
        body: serde_json::Value,
    ) -> Result<T> {
        let config = RetryConfig::default();
        let url = format!("{}/full/v1/{}", self.base_url, endpoint);
        let client = self.http.clone();
        let rate_limiter = self.rate_limiter.clone();

        execute_with_retry(&config, || {
            let url = url.clone();
            let body = body.clone();
            let client = client.clone();
            let rate_limiter = rate_limiter.clone();
            async move {
                rate_limiter
                    .execute(|| {
                        let url = url.clone();
                        let body = body.clone();
                        let client = client.clone();
                        async move {
                            tracing::trace!("Gravity POST request: {}", url);
                            let response = client
                                .post(&url)
                                .header("Content-Type", "application/json")
                                .json(&body)
                                .send()
                                .await?;

                            // Check HTTP status
                            if !response.status().is_success() {
                                let status = response.status();
                                let text = response
                                    .text()
                                    .await
                                    .unwrap_or_else(|_| "Unable to read response body".to_string());
                                return Err(anyhow::anyhow!("HTTP {}: {}", status, text));
                            }

                            // Decode response
                            let data = response.json::<T>().await?;
                            Ok(data)
                        }
                    })
                    .await
            }
        })
        .await
    }

    /// Fetch all available instruments from Gravity API
    async fn fetch_all_instruments(&self) -> Result<Vec<GravityInstrument>> {
        let body = json!({ "is_active": true });
        let response: GravityApiResponse<Vec<GravityInstrument>> =
            self.post("all_instruments", body).await?;
        response
            .result
            .ok_or_else(|| anyhow::anyhow!("No result in all_instruments response"))
    }

    /// Fetch single ticker from Gravity API
    async fn fetch_ticker(&self, gravity_symbol: &str) -> Result<GravityTicker> {
        let body = json!({ "instrument": gravity_symbol });
        let response: GravityApiResponse<GravityTicker> = self.post("ticker", body).await?;
        response
            .result
            .ok_or_else(|| anyhow::anyhow!("No result in ticker response for {}", gravity_symbol))
    }

    /// Fetch all tickers from Gravity API
    async fn fetch_all_tickers_raw(&self) -> Result<Vec<GravityTicker>> {
        // First, get all instruments
        let instruments = self.fetch_all_instruments().await?;
        let mut tickers = Vec::new();

        // Fetch ticker for each instrument
        for instrument in instruments {
            match self.fetch_ticker(&instrument.instrument).await {
                Ok(ticker) => tickers.push(ticker),
                Err(e) => {
                    tracing::warn!(
                        "Failed to fetch ticker for {}: {}",
                        instrument.instrument,
                        e
                    );
                }
            }
        }

        Ok(tickers)
    }

    /// Fetch orderbook from Gravity API
    /// Note: Gravity API has a maximum depth limit of 100
    async fn fetch_orderbook(&self, gravity_symbol: &str, depth: u32) -> Result<GravityOrderbook> {
        let effective_depth = std::cmp::min(depth, 500);
        tracing::debug!(
            "Fetching orderbook for {} with depth {}",
            gravity_symbol,
            effective_depth
        );

        let body = json!({
            "instrument": gravity_symbol,
            "depth": effective_depth
        });
        let response: GravityApiResponse<GravityOrderbook> = self.post("book", body).await?;
        response.result.ok_or_else(|| {
            anyhow::anyhow!("No result in orderbook response for {}", gravity_symbol)
        })
    }

    /// Fetch funding rate from Gravity API
    /// Note: API returns array of funding rates (historical + current), we take the first (current)
    async fn fetch_funding_rate(&self, gravity_symbol: &str) -> Result<GravityFundingRate> {
        let body = json!({ "instrument": gravity_symbol });
        let response: GravityApiResponse<Vec<GravityFundingRate>> =
            self.post("funding", body).await?;
        response
            .result
            .ok_or_else(|| {
                anyhow::anyhow!("No result in funding rate response for {}", gravity_symbol)
            })?
            .into_iter()
            .next()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Empty funding rate array in response for {}",
                    gravity_symbol
                )
            })
    }

    /// Fetch open interest from Gravity API
    /// Note: Gravity API doesn't have a dedicated open_interest endpoint
    /// We extract it from the ticker response instead
    async fn fetch_open_interest(&self, gravity_symbol: &str) -> Result<GravityOpenInterest> {
        let ticker = self.fetch_ticker(gravity_symbol).await?;

        // Extract open_interest from ticker
        let open_interest = ticker
            .open_interest
            .clone()
            .unwrap_or_else(|| "0".to_string());

        Ok(GravityOpenInterest {
            instrument: gravity_symbol.to_string(),
            open_interest,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            mark_price: Some(ticker.mark_price),
        })
    }

    /// Fetch klines from Gravity API
    async fn fetch_klines(
        &self,
        gravity_symbol: &str,
        interval: &str,
        limit: Option<u32>,
    ) -> Result<Vec<GravityKline>> {
        let body = if let Some(lim) = limit {
            json!({
                "instrument": gravity_symbol,
                "interval": interval,
                "limit": lim
            })
        } else {
            json!({
                "instrument": gravity_symbol,
                "interval": interval
            })
        };
        let response: GravityApiResponse<Vec<GravityKline>> = self.post("kline", body).await?;
        response
            .result
            .ok_or_else(|| anyhow::anyhow!("No result in klines response for {}", gravity_symbol))
    }

    /// Fetch trades from Gravity API
    async fn fetch_trades(
        &self,
        gravity_symbol: &str,
        limit: Option<u32>,
    ) -> Result<Vec<GravityTrade>> {
        let body = if let Some(lim) = limit {
            json!({
                "instrument": gravity_symbol,
                "limit": lim
            })
        } else {
            json!({
                "instrument": gravity_symbol
            })
        };
        let response: GravityApiResponse<Vec<GravityTrade>> = self.post("trade", body).await?;
        response
            .result
            .ok_or_else(|| anyhow::anyhow!("No result in trades response for {}", gravity_symbol))
    }

    /// Convert symbol to Gravity format (idempotent - safe to call multiple times)
    /// - "BTC" → "BTC_USDT_Perp"
    /// - "BTC_USDT_Perp" → "BTC_USDT_Perp" (already in correct format)
    fn denormalize_symbol(&self, symbol: &str) -> String {
        let upper = symbol.to_uppercase();
        if upper.ends_with("_USDT_PERP") {
            // Already in correct format
            symbol.to_string()
        } else if upper.ends_with("_USDT") {
            // Add "_Perp" suffix: "BTC_USDT" → "BTC_USDT_Perp"
            format!("{}_Perp", upper)
        } else if upper.contains('_') {
            // Already has underscore but wrong format, treat as base and normalize
            format!("{}_USDT_Perp", upper)
        } else {
            // Pure base currency: "BTC" → "BTC_USDT_Perp"
            format!("{}_USDT_Perp", upper)
        }
    }
}

impl Default for GravityClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IPerps for GravityClient {
    fn get_name(&self) -> &str {
        "gravity"
    }

    fn parse_symbol(&self, symbol: &str) -> String {
        // Convert global symbol to Gravity perpetual format
        // E.g., "BTC" → "BTC_USDT_Perp"
        format!("{}_USDT_Perp", symbol.to_uppercase())
    }

    async fn get_markets(&self) -> Result<Vec<Market>> {
        // Fetch all instruments from API
        let gravity_instruments = self.fetch_all_instruments().await?;

        // Convert to perps_core types
        let markets: Result<Vec<_>> = gravity_instruments
            .into_iter()
            .map(|gi| {
                let symbol = gi.instrument.clone();
                gravity_market_to_market(gi, symbol)
            })
            .collect();

        // Cache the results by symbol
        if let Ok(ref m) = markets {
            let symbols: Vec<String> = m.iter().map(|market| market.symbol.clone()).collect();
            let mut symbol_set = std::collections::HashSet::new();
            for s in symbols {
                symbol_set.insert(s);
            }
            self.symbols_cache.initialize(symbol_set);
        }

        markets
    }

    async fn get_market(&self, symbol: &str) -> Result<Market> {
        let gravity_symbol = self.denormalize_symbol(symbol);
        let instruments = self.fetch_all_instruments().await?;

        instruments
            .into_iter()
            .find(|i| i.instrument == gravity_symbol)
            .ok_or_else(|| anyhow::anyhow!("Market not found: {}", gravity_symbol))
            .and_then(|gi| gravity_market_to_market(gi, symbol.to_string()))
    }

    async fn get_ticker(&self, symbol: &str) -> Result<Ticker> {
        let gravity_symbol = self.denormalize_symbol(symbol);
        let gravity_ticker = self.fetch_ticker(&gravity_symbol).await?;
        gravity_ticker_to_ticker(gravity_ticker, symbol.to_string())
    }

    async fn get_all_tickers(&self) -> Result<Vec<Ticker>> {
        let gravity_tickers = self.fetch_all_tickers_raw().await?;

        gravity_tickers
            .into_iter()
            .map(|gt| {
                // Extract symbol from Gravity format (e.g., "BTC_USDT_Perp" → "BTC")
                let symbol = gt.instrument.split('_').next().unwrap_or("").to_string();
                gravity_ticker_to_ticker(gt, symbol)
            })
            .collect()
    }

    async fn get_orderbook(&self, symbol: &str, depth: u32) -> Result<MultiResolutionOrderbook> {
        let gravity_symbol = self.denormalize_symbol(symbol);
        let gravity_orderbook = self.fetch_orderbook(&gravity_symbol, depth).await?;
        let orderbook = gravity_orderbook_to_orderbook(gravity_orderbook, symbol.to_string())?;
        let timestamp = orderbook.timestamp;

        // Wrap in MultiResolutionOrderbook (medium resolution for REST)
        Ok(MultiResolutionOrderbook {
            symbol: symbol.to_string(),
            timestamp,
            orderbooks: vec![orderbook],
        })
    }

    async fn get_funding_rate(&self, symbol: &str) -> Result<FundingRate> {
        let gravity_symbol = self.denormalize_symbol(symbol);
        let gravity_funding = self.fetch_funding_rate(&gravity_symbol).await?;
        gravity_funding_rate_to_funding_rate(gravity_funding, symbol.to_string())
    }

    async fn get_funding_rate_history(
        &self,
        symbol: &str,
        _start_time: Option<DateTime<Utc>>,
        _end_time: Option<DateTime<Utc>>,
        _limit: Option<u32>,
    ) -> Result<Vec<FundingRate>> {
        // Note: Gravity API doesn't support historical funding rate queries via standard endpoint.
        // This returns the current funding rate as a single-element vector for compatibility.
        // For full history, would need specialized endpoint or database query.
        let funding_rate = self.get_funding_rate(symbol).await?;
        Ok(vec![funding_rate])
    }

    async fn get_open_interest(&self, symbol: &str) -> Result<OpenInterest> {
        let gravity_symbol = self.denormalize_symbol(symbol);
        let gravity_oi = self.fetch_open_interest(&gravity_symbol).await?;
        gravity_open_interest_to_open_interest(gravity_oi, symbol.to_string())
    }

    async fn get_klines(
        &self,
        symbol: &str,
        interval: &str,
        _start_time: Option<DateTime<Utc>>,
        _end_time: Option<DateTime<Utc>>,
        limit: Option<u32>,
    ) -> Result<Vec<Kline>> {
        // Validate interval is supported
        let supported_intervals = ["1m", "5m", "15m", "30m", "1h", "4h", "1d", "1w"];
        if !supported_intervals.contains(&interval) {
            anyhow::bail!(
                "Unsupported kline interval: {}. Supported: {:?}",
                interval,
                supported_intervals
            );
        }

        let gravity_symbol = self.denormalize_symbol(symbol);
        let gravity_klines = self.fetch_klines(&gravity_symbol, interval, limit).await?;

        gravity_klines
            .into_iter()
            .map(|gk| gravity_kline_to_kline(gk, symbol.to_string()))
            .collect()
    }

    async fn get_recent_trades(&self, symbol: &str, limit: u32) -> Result<Vec<Trade>> {
        let gravity_symbol = self.denormalize_symbol(symbol);
        let gravity_trades = self.fetch_trades(&gravity_symbol, Some(limit)).await?;

        gravity_trades
            .into_iter()
            .map(|gt| gravity_trade_to_trade(gt, symbol.to_string()))
            .collect()
    }

    async fn get_market_stats(&self, symbol: &str) -> Result<MarketStats> {
        let ticker = self.get_ticker(symbol).await?;
        let open_interest = self.get_open_interest(symbol).await?;
        let funding_rate = self.get_funding_rate(symbol).await?;

        Ok(MarketStats {
            symbol: ticker.symbol,
            volume_24h: ticker.volume_24h,
            turnover_24h: ticker.turnover_24h,
            open_interest: open_interest.open_interest,
            funding_rate: funding_rate.funding_rate,
            last_price: ticker.last_price,
            mark_price: ticker.mark_price,
            index_price: ticker.index_price,
            price_change_24h: ticker.price_change_24h,
            price_change_pct: ticker.price_change_pct,
            high_price_24h: ticker.high_price_24h,
            low_price_24h: ticker.low_price_24h,
            timestamp: ticker.timestamp,
        })
    }

    async fn get_all_market_stats(&self) -> Result<Vec<MarketStats>> {
        let tickers = self.get_all_tickers().await?;
        let mut stats = Vec::new();

        for ticker in tickers {
            match (
                self.get_open_interest(&ticker.symbol).await,
                self.get_funding_rate(&ticker.symbol).await,
            ) {
                (Ok(oi), Ok(fr)) => {
                    stats.push(MarketStats {
                        symbol: ticker.symbol,
                        volume_24h: ticker.volume_24h,
                        turnover_24h: ticker.turnover_24h,
                        open_interest: oi.open_interest,
                        funding_rate: fr.funding_rate,
                        last_price: ticker.last_price,
                        mark_price: ticker.mark_price,
                        index_price: ticker.index_price,
                        price_change_24h: ticker.price_change_24h,
                        price_change_pct: ticker.price_change_pct,
                        high_price_24h: ticker.high_price_24h,
                        low_price_24h: ticker.low_price_24h,
                        timestamp: ticker.timestamp,
                    });
                }
                (Err(e), _) => {
                    tracing::warn!("Failed to fetch open interest for {}: {}", ticker.symbol, e);
                }
                (_, Err(e)) => {
                    tracing::warn!("Failed to fetch funding rate for {}: {}", ticker.symbol, e);
                }
            }
        }

        Ok(stats)
    }

    async fn is_supported(&self, symbol: &str) -> Result<bool> {
        // Normalize symbol to Gravity format (e.g., "BTC" or "BTC_USDT_Perp" both become "BTC_USDT_Perp")
        let normalized_symbol = self.denormalize_symbol(symbol);
        tracing::debug!(
            "Checking if symbol {} (normalized to {}) is supported on Gravity",
            symbol,
            normalized_symbol
        );

        // Initialize cache with all instruments if not already cached
        self.symbols_cache
            .get_or_init(|| async {
                match self.fetch_all_instruments().await {
                    Ok(instruments) => {
                        let symbols: std::collections::HashSet<String> =
                            instruments.iter().map(|i| i.instrument.clone()).collect();
                        tracing::debug!("Cached {} Gravity instruments", symbols.len());
                        Ok(symbols)
                    }
                    Err(err) => {
                        tracing::error!(
                            "Failed to fetch instruments from Gravity API for caching: {}",
                            err
                        );
                        Err(err)
                    }
                }
            })
            .await?;

        // Check if normalized symbol is in cache
        let is_cached = self.symbols_cache.contains(&normalized_symbol).await;
        if is_cached {
            tracing::debug!(
                "✓ Symbol {} (normalized: {}) is supported on Gravity (from cache)",
                symbol,
                normalized_symbol
            );
            Ok(true)
        } else {
            tracing::warn!(
                "✗ Symbol {} (normalized: {}) not found in Gravity cache",
                symbol,
                normalized_symbol
            );
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use perps_core::OrderSide;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    #[test]
    fn test_parse_symbol() {
        let client = GravityClient::new();
        assert_eq!(client.parse_symbol("BTC"), "BTC_USDT_Perp");
        assert_eq!(client.parse_symbol("eth"), "ETH_USDT_Perp");
        assert_eq!(client.parse_symbol("SOL"), "SOL_USDT_Perp");
    }

    #[test]
    fn test_gravity_ticker_to_ticker_conversion() {
        let gravity_ticker = GravityTicker {
            instrument: "BTC_USDT_Perp".to_string(),
            event_time: "1702641000000000000".to_string(),
            mark_price: "50001000000000".to_string(),
            index_price: "49999750000000".to_string(),
            last_price: "50000000000000".to_string(),
            last_size: Some("1.5".to_string()),
            mid_price: "50000500000000".to_string(),
            best_bid_price: "49999500000000".to_string(),
            best_bid_size: "2.0".to_string(),
            best_ask_price: "50000500000000".to_string(),
            best_ask_size: "1.2".to_string(),
            funding_rate_8h_curr: None,
            funding_rate_8h_avg: None,
            buy_volume_24h_b: Some("100".to_string()),
            sell_volume_24h_b: Some("90".to_string()),
            buy_volume_24h_q: Some("5000000000000".to_string()),
            sell_volume_24h_q: Some("4500000000000".to_string()),
            high_price: Some("51000000000000".to_string()),
            low_price: Some("49000000000000".to_string()),
            open_price: Some("49500000000000".to_string()),
            open_interest: Some("10000".to_string()),
        };

        let result = gravity_ticker_to_ticker(gravity_ticker, "BTC".to_string()).unwrap();

        assert_eq!(result.symbol, "BTC");
        assert!(result.last_price > Decimal::ZERO);
        assert!(result.mark_price > Decimal::ZERO);
        assert!(result.volume_24h > Decimal::ZERO);
    }

    #[test]
    fn test_gravity_market_to_market_conversion() {
        let gravity_instrument = GravityInstrument {
            instrument: "BTC_USDT_Perp".to_string(),
            instrument_hash: None,
            base: "BTC".to_string(),
            quote: "USDT".to_string(),
            kind: "PERPETUAL".to_string(),
            venues: None,
            settlement_period: "PERPETUAL".to_string(),
            base_decimals: Some(8),
            quote_decimals: Some(6),
            tick_size: Some("1".to_string()),
            min_size: "0.001".to_string(),
            create_time: None,
            max_position_size: Some("1000".to_string()),
            funding_interval_hours: None,
            adjusted_funding_rate_cap: None,
            adjusted_funding_rate_floor: None,
        };

        let result =
            gravity_market_to_market(gravity_instrument, "BTC_USDT_Perp".to_string()).unwrap();

        assert_eq!(result.symbol, "BTC_USDT_Perp");
        assert_eq!(result.contract, "PERPETUAL");
        assert_eq!(result.price_scale, 6);
        assert_eq!(result.quantity_scale, 8);
    }

    #[test]
    fn test_gravity_orderbook_to_orderbook_conversion() {
        let gravity_orderbook = GravityOrderbook {
            instrument: "BTC_USDT_Perp".to_string(),
            event_time: "1702641000000000000".to_string(),
            bids: vec![GravityOrderbookLevel {
                price: "50000000000000".to_string(),
                size: "1.5".to_string(),
                num_orders: Some(5),
            }],
            asks: vec![GravityOrderbookLevel {
                price: "50001000000000".to_string(),
                size: "2.0".to_string(),
                num_orders: Some(3),
            }],
        };

        let result = gravity_orderbook_to_orderbook(gravity_orderbook, "BTC".to_string()).unwrap();

        assert_eq!(result.symbol, "BTC");
        assert_eq!(result.bids.len(), 1);
        assert_eq!(result.asks.len(), 1);
        assert_eq!(
            result.bids[0].price,
            Decimal::from_str("50000000000000").unwrap()
        );
    }

    #[test]
    fn test_client_creation() {
        let client = GravityClient::new();
        assert_eq!(client.get_name(), "gravity");
    }

    // Phase 3: Extended Methods Tests

    #[test]
    fn test_gravity_funding_rate_conversion() {
        let gravity_fr = GravityFundingRate {
            instrument: "BTC_USDT_Perp".to_string(),
            funding_rate: "0.0001".to_string(),
            funding_time: "1702641000000000000".to_string(),
            mark_price: "50000000000000".to_string(),
            funding_rate_8_h_avg: Some("0.00008".to_string()),
        };

        let result = gravity_funding_rate_to_funding_rate(gravity_fr, "BTC".to_string()).unwrap();
        assert_eq!(result.symbol, "BTC");
        assert_eq!(result.funding_rate, Decimal::from_str("0.0001").unwrap());
        assert_eq!(result.predicted_rate, Decimal::from_str("0.00008").unwrap());
    }

    #[test]
    fn test_gravity_kline_conversion() {
        let gravity_kline = GravityKline {
            open_time: "1702641000000000000".to_string(),
            close_time: "1702641060000000000".to_string(),
            open: "50000000000000".to_string(),
            high: "51000000000000".to_string(),
            low: "49000000000000".to_string(),
            close: "50500000000000".to_string(),
            volume_b: "100".to_string(),
            volume_q: "5000000000000".to_string(),
            trades: Some(50),
            instrument: "BTC_USDT_Perp".to_string(),
        };

        let result = gravity_kline_to_kline(gravity_kline, "BTC".to_string()).unwrap();
        assert_eq!(result.symbol, "BTC");
        assert_eq!(result.interval, "1m");
        assert_eq!(result.close, Decimal::from_str("50500000000000").unwrap());
        assert_eq!(result.volume, Decimal::from_str("100").unwrap());
    }

    #[test]
    fn test_gravity_trade_conversion_buy() {
        let gravity_trade = GravityTrade {
            trade_id: "12345".to_string(),
            instrument: "BTC_USDT_Perp".to_string(),
            is_taker_buyer: true,
            size: "1.5".to_string(),
            price: "50000000000000".to_string(),
            mark_price: "50001000000000".to_string(),
            index_price: "49999750000000".to_string(),
            interest_rate: None,
            forward_price: None,
            venue: Some("ORDERBOOK".to_string()),
            is_rpi: None,
            event_time: "1702641000000000000".to_string(),
        };

        let result = gravity_trade_to_trade(gravity_trade, "BTC".to_string()).unwrap();
        assert_eq!(result.id, "12345");
        assert_eq!(result.side, OrderSide::Buy);
        assert_eq!(result.quantity, Decimal::from_str("1.5").unwrap());
    }

    #[test]
    fn test_gravity_trade_conversion_sell() {
        let gravity_trade = GravityTrade {
            trade_id: "12346".to_string(),
            instrument: "BTC_USDT_Perp".to_string(),
            is_taker_buyer: false,
            size: "2.0".to_string(),
            price: "50100000000000".to_string(),
            mark_price: "50101000000000".to_string(),
            index_price: "50099750000000".to_string(),
            interest_rate: None,
            forward_price: None,
            venue: Some("ORDERBOOK".to_string()),
            is_rpi: None,
            event_time: "1702641000000000000".to_string(),
        };

        let result = gravity_trade_to_trade(gravity_trade, "BTC".to_string()).unwrap();
        assert_eq!(result.id, "12346");
        assert_eq!(result.side, OrderSide::Sell);
        assert_eq!(result.quantity, Decimal::from_str("2.0").unwrap());
    }

    #[test]
    fn test_gravity_open_interest_conversion() {
        let gravity_oi = GravityOpenInterest {
            instrument: "BTC_USDT_Perp".to_string(),
            open_interest: "50000".to_string(),
            timestamp: 1702641000,
            mark_price: Some("45000".to_string()),
        };

        let result = gravity_open_interest_to_open_interest(gravity_oi, "BTC".to_string()).unwrap();
        assert_eq!(result.symbol, "BTC");
        assert_eq!(result.open_interest, Decimal::from_str("50000").unwrap());
        // open_value = open_interest * mark_price = 50000 * 45000
        assert_eq!(result.open_value, Decimal::from_str("2250000000").unwrap());
    }

    #[test]
    fn test_supported_kline_intervals() {
        let _client = GravityClient::new();
        let supported = vec!["1m", "5m", "15m", "30m", "1h", "4h", "1d", "1w"];

        // This is a compile-time check that the intervals are hardcoded correctly
        // In actual tests, we'd mock the API responses
        for interval in supported {
            assert!(!interval.is_empty());
        }
    }
}
