use crate::cache::SymbolsCache;
use crate::hyperliquid::types::*;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use perps_core::types::*;
use perps_core::{execute_with_retry, IPerps, RateLimiter, RetryConfig};
use rust_decimal::Decimal;
use serde_json::Value;
use std::str::FromStr;
use std::sync::Arc;

const INFO_URL: &str = "https://api.hyperliquid.xyz/info";

/// A client for the Hyperliquid exchange.
#[derive(Clone)]
pub struct HyperliquidClient {
    http: reqwest::Client,
    /// Cached set of supported symbols
    symbols_cache: SymbolsCache,
    /// Rate limiter for API calls
    rate_limiter: Arc<RateLimiter>,
}

impl HyperliquidClient {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::new(),
            symbols_cache: SymbolsCache::new(),
            rate_limiter: Arc::new(RateLimiter::hyperliquid()),
        }
    }

    /// Ensure the symbols cache is initialized
    async fn ensure_cache_initialized(&self) -> Result<()> {
        self.symbols_cache
            .get_or_init(|| async {
                let markets = self.get_markets().await?;
                Ok(markets.into_iter().map(|m| m.symbol).collect())
            })
            .await
    }

    async fn post<T: serde::de::DeserializeOwned>(&self, body: Value) -> Result<T> {
        let config = RetryConfig::default();
        let http = self.http.clone();
        let rate_limiter = self.rate_limiter.clone();

        execute_with_retry(&config, || {
            let http = http.clone();
            let body = body.clone();
            let rate_limiter = rate_limiter.clone();
            async move {
                rate_limiter
                    .execute(|| {
                        let http = http.clone();
                        let body = body.clone();
                        async move {
                            let response = http.post(INFO_URL).json(&body).send().await?;
                            if !response.status().is_success() {
                                let status = response.status();
                                let text = response
                                    .text()
                                    .await
                                    .unwrap_or_else(|_| "Failed to read error body".to_string());
                                return Err(anyhow!(
                                    "info request failed with status: {}. Body: {}",
                                    status,
                                    text
                                ));
                            }
                            let data = response.json().await?;
                            Ok(data)
                        }
                    })
                    .await
            }
        })
        .await
    }

    /// Helper method to fetch combined meta and asset contexts
    async fn get_meta_and_asset_ctxs(&self) -> Result<MetaAndAssetCtxs> {
        let body = serde_json::json!({ "type": "metaAndAssetCtxs" });
        // The response is an array: [{"universe": [...]}, [...assetCtxs]]
        let response: Vec<Value> = self.post(body).await?;

        if response.len() < 2 {
            return Err(anyhow!("Unexpected response format from metaAndAssetCtxs"));
        }

        // First element is {"universe": [...]}
        let meta: Meta = serde_json::from_value(response[0].clone())?;
        let asset_ctxs: Vec<AssetCtx> = serde_json::from_value(response[1].clone())?;

        Ok(MetaAndAssetCtxs {
            universe: meta.universe,
            asset_ctxs,
        })
    }

    /// Helper method to fetch orderbook at a specific resolution (nSigFigs)
    async fn fetch_orderbook_at_resolution(
        &self,
        symbol: &str,
        n_sig_figs: u32,
    ) -> Result<Orderbook> {
        let mut body = serde_json::json!({
            "type": "l2Book",
            "coin": symbol,
            "nSigFigs": n_sig_figs,
            "mantisa": 5,
        });
        if n_sig_figs == 0 {
            body["nSigFigs"] = Value::Null;
        }
        let book: L2Book = self.post(body).await?;

        let bids = book.levels[0]
            .iter()
            .map(|l| {
                Ok(OrderbookLevel {
                    price: Decimal::from_str(&l.px)?,
                    quantity: Decimal::from_str(&l.sz)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let asks = book.levels[1]
            .iter()
            .map(|l| {
                Ok(OrderbookLevel {
                    price: Decimal::from_str(&l.px)?,
                    quantity: Decimal::from_str(&l.sz)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(Orderbook {
            symbol: book.coin,
            bids,
            asks,
            timestamp: Utc::now(),
        })
    }

    /// Helper method to fetch 24h high/low prices from 1-hour candles
    async fn get_24h_high_low(&self, symbol: &str) -> Result<(Decimal, Decimal)> {
        // Fetch last 24 1-hour candles to calculate high/low
        let start_time = Utc::now() - chrono::Duration::hours(24);
        let body = serde_json::json!({
            "type": "candleSnapshot",
            "req": {
                "coin": symbol,
                "interval": "1h",
                "startTime": start_time.timestamp_millis()
            }
        });

        let candles: Vec<CandleSnapshot> = self.post(body).await?;

        if candles.is_empty() {
            return Ok((Decimal::ZERO, Decimal::ZERO));
        }

        // Calculate high/low from all candles
        let mut high = Decimal::ZERO;
        let mut low = Decimal::MAX;

        for candle in candles {
            let candle_high = Decimal::from_str(&candle.h)?;
            let candle_low = Decimal::from_str(&candle.l)?;

            if candle_high > high {
                high = candle_high;
            }
            if candle_low < low {
                low = candle_low;
            }
        }

        // If low is still MAX, no valid data found
        if low == Decimal::MAX {
            low = Decimal::ZERO;
        }

        Ok((high, low))
    }
}

impl Default for HyperliquidClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IPerps for HyperliquidClient {
    fn get_name(&self) -> &str {
        "hyperliquid"
    }

    fn parse_symbol(&self, symbol: &str) -> String {
        // Hyperliquid uses uppercase symbols like "BTC"
        symbol.to_uppercase()
    }

    async fn get_markets(&self) -> Result<Vec<Market>> {
        let body = serde_json::json!({ "type": "meta" });
        let meta: Meta = self.post(body).await?;
        let markets = meta
            .universe
            .into_iter()
            .map(|u| Market {
                symbol: u.name.clone(),
                contract: u.name,
                contract_size: Decimal::ONE, // Not provided
                price_scale: 0,              // Not provided
                quantity_scale: u.sz_decimals as i32,
                min_order_qty: Decimal::ZERO,   // Not provided
                max_order_qty: Decimal::ZERO,   // Not provided
                min_order_value: Decimal::ZERO, // Not provided
                max_leverage: u.max_leverage.into(),
            })
            .collect();
        Ok(markets)
    }

    async fn get_market(&self, symbol: &str) -> Result<Market> {
        let markets = self.get_markets().await?;
        markets
            .into_iter()
            .find(|m| m.symbol == symbol)
            .ok_or_else(|| anyhow!("Market {} not found", symbol))
    }

    async fn get_ticker(&self, symbol: &str) -> Result<Ticker> {
        let data = self.get_meta_and_asset_ctxs().await?;

        // Find the index of the symbol in the universe
        let index = data
            .universe
            .iter()
            .position(|u| u.name == symbol)
            .ok_or_else(|| anyhow!("Symbol {} not found", symbol))?;

        let asset_ctx = data
            .asset_ctxs
            .get(index)
            .ok_or_else(|| anyhow!("Asset context not found for {}", symbol))?;

        // Get orderbook for best bid/ask with quantities
        // Use best available resolution (fine → medium → coarse)
        let multi_orderbook = self.get_orderbook(symbol, 1).await?;
        let orderbook = multi_orderbook
            .best_for_tight_spreads()
            .ok_or_else(|| anyhow!("No orderbook available for {}", symbol))?;

        let (best_bid_price, best_bid_qty) = orderbook
            .bids
            .first()
            .map(|l| (l.price, l.quantity))
            .unwrap_or((Decimal::ZERO, Decimal::ZERO));
        let (best_ask_price, best_ask_qty) = orderbook
            .asks
            .first()
            .map(|l| (l.price, l.quantity))
            .unwrap_or((Decimal::ZERO, Decimal::ZERO));

        let last_price =
            Decimal::from_str(&asset_ctx.mid_px.clone().unwrap_or_else(|| "0".to_string()))?;
        let prev_day_px = Decimal::from_str(&asset_ctx.prev_day_px)?;
        let price_change = last_price - prev_day_px;
        // Store as decimal (e.g., 0.05 for 5%) to match other exchanges
        let price_change_percent = if prev_day_px > Decimal::ZERO {
            price_change / prev_day_px
        } else {
            Decimal::ZERO
        };

        // Fetch 24h high/low from 1-day candle
        let (high_price_24h, low_price_24h) = self.get_24h_high_low(symbol).await?;

        let open_interest = Decimal::from_str(&asset_ctx.open_interest).unwrap_or(Decimal::ZERO);

        let mark_price = Decimal::from_str(&asset_ctx.mark_px)?;

        Ok(Ticker {
            symbol: symbol.to_string(),
            last_price,
            mark_price,
            index_price: Decimal::from_str(&asset_ctx.oracle_px)?,
            best_bid_price,
            best_bid_qty,
            best_ask_price,
            best_ask_qty,
            volume_24h: Decimal::from_str(&asset_ctx.day_base_vlm)?,
            turnover_24h: Decimal::from_str(&asset_ctx.day_ntl_vlm)?,
            open_interest,
            open_interest_notional: open_interest * mark_price,
            price_change_24h: price_change,
            price_change_pct: price_change_percent,
            high_price_24h,
            low_price_24h,
            timestamp: Utc::now(),
        })
    }

    async fn get_all_tickers(&self) -> Result<Vec<Ticker>> {
        let data = self.get_meta_and_asset_ctxs().await?;
        let mut tickers = Vec::new();

        for (index, universe_item) in data.universe.iter().enumerate() {
            if let Some(asset_ctx) = data.asset_ctxs.get(index) {
                // For all tickers, we skip fetching orderbook to improve performance
                // Use mid price as last price and mark price for bid/ask approximation
                let last_price = Decimal::from_str(
                    &asset_ctx
                        .mid_px
                        .clone()
                        .unwrap_or_else(|| asset_ctx.mark_px.clone()),
                )?;
                let prev_day_px = Decimal::from_str(&asset_ctx.prev_day_px)?;
                let price_change = last_price - prev_day_px;
                // Store as decimal (e.g., 0.05 for 5%) to match other exchanges
                let price_change_percent = if prev_day_px > Decimal::ZERO {
                    price_change / prev_day_px
                } else {
                    Decimal::ZERO
                };

                tickers.push(Ticker {
                    symbol: universe_item.name.clone(),
                    last_price,
                    mark_price: Decimal::from_str(&asset_ctx.mark_px)?,
                    index_price: Decimal::from_str(&asset_ctx.oracle_px)?,
                    best_bid_price: last_price, // Approximation
                    best_bid_qty: Decimal::ZERO,
                    best_ask_price: last_price, // Approximation
                    best_ask_qty: Decimal::ZERO,
                    volume_24h: Decimal::from_str(&asset_ctx.day_base_vlm)?,
                    turnover_24h: Decimal::from_str(&asset_ctx.day_ntl_vlm)?,
                    open_interest: Decimal::ZERO,
                    open_interest_notional: Decimal::ZERO,
                    price_change_24h: price_change,
                    price_change_pct: price_change_percent,
                    high_price_24h: Decimal::ZERO,
                    low_price_24h: Decimal::ZERO,
                    timestamp: Utc::now(),
                });
            }
        }

        Ok(tickers)
    }

    async fn get_orderbook(&self, symbol: &str, _depth: u32) -> Result<MultiResolutionOrderbook> {
        // Fetch all 3 resolutions in parallel for optimal performance
        let (fine_result, medium_result, coarse_result) = match symbol {
            "BTC" => tokio::join!(
                self.fetch_orderbook_at_resolution(symbol, 0),
                self.fetch_orderbook_at_resolution(symbol, 5),
                self.fetch_orderbook_at_resolution(symbol, 4),
            ),
            _ => tokio::join!(
                self.fetch_orderbook_at_resolution(symbol, 5),
                self.fetch_orderbook_at_resolution(symbol, 4),
                self.fetch_orderbook_at_resolution(symbol, 2),
            ),
        };

        // Log results with enhanced debugging information
        match &fine_result {
            Ok(book) => {
                let best_bid = book.bids.first().map(|l| (l.price, l.quantity));
                let best_ask = book.asks.first().map(|l| (l.price, l.quantity));
                tracing::debug!(
                    "✓ Fine orderbook fetched for {}: {} bids, {} asks | Best bid: {} @ {} | Best ask: {} @ {}",
                    symbol,
                    book.bids.len(),
                    book.asks.len(),
                    best_bid.map(|(p, _)| p.to_string()).unwrap_or_else(|| "N/A".to_string()),
                    best_bid.map(|(_, q)| q.to_string()).unwrap_or_else(|| "N/A".to_string()),
                    best_ask.map(|(p, _)| p.to_string()).unwrap_or_else(|| "N/A".to_string()),
                    best_ask.map(|(_, q)| q.to_string()).unwrap_or_else(|| "N/A".to_string()),
                );
            }
            Err(e) => tracing::warn!("✗ Fine orderbook fetch failed for {}: {}", symbol, e),
        }

        match &medium_result {
            Ok(book) => {
                let best_bid = book.bids.first().map(|l| (l.price, l.quantity));
                let best_ask = book.asks.first().map(|l| (l.price, l.quantity));
                tracing::debug!(
                    "✓ Medium orderbook fetched for {}: {} bids, {} asks | Best bid: {} @ {} | Best ask: {} @ {}",
                    symbol,
                    book.bids.len(),
                    book.asks.len(),
                    best_bid.map(|(p, _)| p.to_string()).unwrap_or_else(|| "N/A".to_string()),
                    best_bid.map(|(_, q)| q.to_string()).unwrap_or_else(|| "N/A".to_string()),
                    best_ask.map(|(p, _)| p.to_string()).unwrap_or_else(|| "N/A".to_string()),
                    best_ask.map(|(_, q)| q.to_string()).unwrap_or_else(|| "N/A".to_string()),
                );
            }
            Err(e) => tracing::warn!("✗ Medium orderbook fetch failed for {}: {}", symbol, e),
        }

        match &coarse_result {
            Ok(book) => {
                let best_bid = book.bids.first().map(|l| (l.price, l.quantity));
                let best_ask = book.asks.first().map(|l| (l.price, l.quantity));
                tracing::debug!(
                    "✓ Coarse orderbook fetched for {}: {} bids, {} asks | Best bid: {} @ {} | Best ask: {} @ {}",
                    symbol,
                    book.bids.len(),
                    book.asks.len(),
                    best_bid.map(|(p, _)| p.to_string()).unwrap_or_else(|| "N/A".to_string()),
                    best_bid.map(|(_, q)| q.to_string()).unwrap_or_else(|| "N/A".to_string()),
                    best_ask.map(|(p, _)| p.to_string()).unwrap_or_else(|| "N/A".to_string()),
                    best_ask.map(|(_, q)| q.to_string()).unwrap_or_else(|| "N/A".to_string()),
                );
            }
            Err(e) => tracing::warn!("✗ Coarse orderbook fetch failed for {}: {}", symbol, e),
        }

        // Collect successful orderbooks in order: fine → medium → coarse
        let mut orderbooks = Vec::new();

        if let Ok(fine_book) = fine_result {
            orderbooks.push(fine_book);
        }
        if let Ok(medium_book) = medium_result {
            orderbooks.push(medium_book);
        }
        if let Ok(coarse_book) = coarse_result {
            orderbooks.push(coarse_book);
        }

        // Ensure at least one resolution succeeded
        if orderbooks.is_empty() {
            return Err(anyhow!("All orderbook resolutions failed for {}", symbol));
        }

        let timestamp = Utc::now();
        Ok(MultiResolutionOrderbook::from_multiple(
            symbol.to_string(),
            timestamp,
            orderbooks,
        ))
    }

    async fn get_funding_rate(&self, symbol: &str) -> Result<FundingRate> {
        let body = serde_json::json!({ "type": "fundingHistory", "coin": symbol, "startTime": Utc::now().timestamp_millis() - 1000 * 60 * 60 * 8 });
        let history: Vec<FundingHistory> = self.post(body).await?;
        let last_rate = history
            .last()
            .ok_or_else(|| anyhow!("No funding history found"))?;
        Ok(FundingRate {
            symbol: symbol.to_string(),
            funding_rate: Decimal::from_str(&last_rate.funding_rate)?,
            predicted_rate: Decimal::ZERO, // Not available
            funding_time: Utc.timestamp_millis_opt(last_rate.time as i64).unwrap(),
            next_funding_time: Utc.timestamp_millis_opt(0).unwrap(), // Not available
            funding_interval: 0,                                     // Not available
            funding_rate_cap_floor: Decimal::ZERO,                   // Not available
        })
    }

    async fn get_funding_rate_history(
        &self,
        symbol: &str,
        start_time: Option<DateTime<Utc>>,
        end_time: Option<DateTime<Utc>>,
        _limit: Option<u32>,
    ) -> Result<Vec<FundingRate>> {
        let start = start_time.unwrap_or_else(|| Utc::now() - chrono::Duration::days(7));
        let end = end_time.unwrap_or_else(Utc::now);
        let body = serde_json::json!({ "type": "fundingHistory", "coin": symbol, "startTime": start.timestamp_millis(), "endTime": end.timestamp_millis() });
        let history: Vec<FundingHistory> = self.post(body).await?;
        let rates = history
            .into_iter()
            .map(|h| {
                Ok(FundingRate {
                    symbol: symbol.to_string(),
                    funding_rate: Decimal::from_str(&h.funding_rate)?,
                    predicted_rate: Decimal::ZERO,
                    funding_time: Utc.timestamp_millis_opt(h.time as i64).unwrap(),
                    next_funding_time: Utc.timestamp_millis_opt(0).unwrap(),
                    funding_interval: 0,
                    funding_rate_cap_floor: Decimal::ZERO,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(rates)
    }

    async fn get_open_interest(&self, symbol: &str) -> Result<OpenInterest> {
        let data = self.get_meta_and_asset_ctxs().await?;

        // Find the index of the symbol in the universe
        let index = data
            .universe
            .iter()
            .position(|u| u.name == symbol)
            .ok_or_else(|| anyhow!("Symbol {} not found", symbol))?;

        let asset_ctx = data
            .asset_ctxs
            .get(index)
            .ok_or_else(|| anyhow!("Asset context not found for {}", symbol))?;

        let open_interest = Decimal::from_str(&asset_ctx.open_interest)?;
        let mark_price = Decimal::from_str(&asset_ctx.mark_px)?;
        let open_value = open_interest * mark_price;

        Ok(OpenInterest {
            symbol: symbol.to_string(),
            open_interest,
            open_value,
            timestamp: Utc::now(),
        })
    }

    async fn get_klines(
        &self,
        symbol: &str,
        interval: &str,
        start_time: Option<DateTime<Utc>>,
        _end_time: Option<DateTime<Utc>>,
        _limit: Option<u32>,
    ) -> Result<Vec<Kline>> {
        let body = serde_json::json!({ "type": "candleSnapshot", "req": { "coin": symbol, "interval": interval, "startTime": start_time.unwrap_or_else(Utc::now).timestamp_millis() } });
        let klines: Vec<CandleSnapshot> = self.post(body).await?;
        let klines = klines
            .into_iter()
            .map(|k| {
                Ok(Kline {
                    symbol: symbol.to_string(),
                    interval: interval.to_string(),
                    open_time: Utc.timestamp_millis_opt(k.t as i64).unwrap(),
                    close_time: Utc.timestamp_millis_opt(k.T as i64).unwrap(),
                    open: Decimal::from_str(&k.o)?,
                    high: Decimal::from_str(&k.h)?,
                    low: Decimal::from_str(&k.l)?,
                    close: Decimal::from_str(&k.c)?,
                    volume: Decimal::from_str(&k.v)?,
                    turnover: Decimal::ZERO, // Not available
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(klines)
    }

    async fn get_recent_trades(&self, _symbol: &str, _limit: u32) -> Result<Vec<Trade>> {
        // Hyperliquid doesn't have a public recent trades endpoint
        // Would require userFills with a known address
        Err(anyhow!(
            "Recent trades are not available from Hyperliquid public API"
        ))
    }

    async fn get_market_stats(&self, symbol: &str) -> Result<MarketStats> {
        let ticker = self.get_ticker(symbol).await?;
        let oi = self.get_open_interest(symbol).await?;
        let funding = self.get_funding_rate(symbol).await?;

        Ok(MarketStats {
            symbol: symbol.to_string(),
            last_price: ticker.last_price,
            mark_price: ticker.mark_price,
            index_price: ticker.index_price,
            high_price_24h: ticker.high_price_24h,
            low_price_24h: ticker.low_price_24h,
            volume_24h: ticker.volume_24h,
            turnover_24h: ticker.turnover_24h,
            price_change_24h: ticker.price_change_24h,
            price_change_pct: ticker.price_change_pct,
            open_interest: oi.open_interest,
            funding_rate: funding.funding_rate,
            timestamp: Utc::now(),
        })
    }

    async fn get_all_market_stats(&self) -> Result<Vec<MarketStats>> {
        let data = self.get_meta_and_asset_ctxs().await?;
        let mut stats = Vec::new();

        for (index, universe_item) in data.universe.iter().enumerate() {
            if let Some(asset_ctx) = data.asset_ctxs.get(index) {
                let last_price = Decimal::from_str(
                    &asset_ctx
                        .mid_px
                        .clone()
                        .unwrap_or_else(|| asset_ctx.mark_px.clone()),
                )?;
                let prev_day_px = Decimal::from_str(&asset_ctx.prev_day_px)?;
                let price_change = last_price - prev_day_px;
                // Store as decimal (e.g., 0.05 for 5%) to match other exchanges
                let price_change_percent = if prev_day_px > Decimal::ZERO {
                    price_change / prev_day_px
                } else {
                    Decimal::ZERO
                };

                stats.push(MarketStats {
                    symbol: universe_item.name.clone(),
                    last_price,
                    mark_price: Decimal::from_str(&asset_ctx.mark_px)?,
                    index_price: Decimal::from_str(&asset_ctx.oracle_px)?,
                    high_price_24h: Decimal::ZERO,
                    low_price_24h: Decimal::ZERO,
                    volume_24h: Decimal::from_str(&asset_ctx.day_base_vlm)?,
                    turnover_24h: Decimal::from_str(&asset_ctx.day_ntl_vlm)?,
                    price_change_24h: price_change,
                    price_change_pct: price_change_percent,
                    open_interest: Decimal::from_str(&asset_ctx.open_interest)?,
                    funding_rate: Decimal::from_str(&asset_ctx.funding)?,
                    timestamp: Utc::now(),
                });
            }
        }

        Ok(stats)
    }

    async fn is_supported(&self, symbol: &str) -> Result<bool> {
        self.ensure_cache_initialized().await?;
        Ok(self.symbols_cache.contains(symbol).await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_get_markets() {
        let client = HyperliquidClient::default();
        let markets = client.get_markets().await.unwrap();
        assert!(!markets.is_empty());
    }

    #[tokio::test]
    async fn test_get_orderbook() {
        let client = HyperliquidClient::default();
        let orderbook = client.get_orderbook("BTC", 10).await.unwrap();
        assert_eq!(orderbook.symbol, "BTC");
        assert!(!orderbook.orderbooks.is_empty());
    }

    #[tokio::test]
    async fn test_get_funding_rate() {
        let client = HyperliquidClient::default();
        let funding_rate = client.get_funding_rate("BTC").await.unwrap();
        assert_eq!(funding_rate.symbol, "BTC");
    }

    #[tokio::test]
    async fn test_get_ticker() {
        let client = HyperliquidClient::default();
        let ticker = client.get_ticker("BTC").await.unwrap();
        assert_eq!(ticker.symbol, "BTC");
        assert!(ticker.last_price > Decimal::ZERO);
    }

    #[tokio::test]
    async fn test_get_open_interest() {
        let client = HyperliquidClient::default();
        let oi = client.get_open_interest("BTC").await.unwrap();
        assert_eq!(oi.symbol, "BTC");
        assert!(oi.open_interest >= Decimal::ZERO);
    }
}
