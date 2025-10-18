use crate::aster::types::{BookTicker, Depth, ExchangeInfo, MarkPrice, Ticker24hr};
use crate::cache::SymbolsCache;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{DateTime, Duration, TimeZone, Utc};
use perps_core::types::{
    FundingRate, Kline, Market, MarketStats, OpenInterest, OrderSide, Orderbook, OrderbookLevel,
    Ticker, Trade,
};
use perps_core::{execute_with_retry, IPerps, RateLimiter, RetryConfig};
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, RwLock};

const BASE_URL: &str = "https://fapi.asterdex.com";
const BOOK_TICKER_CACHE_TTL_SECS: i64 = 5; // Cache book tickers for 5 seconds
const DEFAULT_STALENESS: u64 = 2; // 2 seconds staleness threshold for orderbook cache

/// Cached book ticker data with expiration
#[derive(Clone, Debug)]
struct BookTickerCache {
    data: HashMap<String, BookTicker>,
    expires_at: DateTime<Utc>,
}

impl BookTickerCache {
    fn new(data: HashMap<String, BookTicker>, ttl_secs: i64) -> Self {
        Self {
            data,
            expires_at: Utc::now() + Duration::seconds(ttl_secs),
        }
    }

    fn is_expired(&self) -> bool {
        Utc::now() > self.expires_at
    }
}

/// A client for the Aster DEX exchange (Binance-compatible API).
#[derive(Clone)]
pub struct AsterClient {
    http: reqwest::Client,
    /// Cached set of supported symbols
    symbols_cache: SymbolsCache,
    /// Rate limiter for API calls
    rate_limiter: Arc<RateLimiter>,
    /// Cached book ticker data
    book_ticker_cache: Arc<RwLock<Option<BookTickerCache>>>,
    /// Optional stream manager for WebSocket orderbook streaming
    stream_manager: Option<Arc<perps_core::StreamManager>>,
}

impl AsterClient {
    pub async fn new() -> Result<Self> {
        // Check if streaming should be enabled
        let should_enable_streaming = std::env::var("DATABASE_URL").is_ok()
            && std::env::var("ENABLE_ORDERBOOK_STREAMING")
                .map(|v| v.to_lowercase() != "false")
                .unwrap_or(true);

        if !should_enable_streaming {
            tracing::debug!("AsterClient: Streaming disabled, using REST-only mode");
            return Ok(Self::new_rest_only());
        }

        // Try to initialize with streaming
        match Self::try_init_streaming().await {
            Ok(client) => {
                tracing::info!("✓ AsterClient initialized with WebSocket streaming");
                Ok(client)
            }
            Err(e) => {
                tracing::warn!("Failed to initialize streaming for AsterClient: {}", e);
                tracing::warn!("Falling back to REST-only mode");
                Ok(Self::new_rest_only())
            }
        }
    }

    /// Create a REST-only client (no streaming)
    pub fn new_rest_only() -> Self {
        Self {
            http: reqwest::Client::new(),
            symbols_cache: SymbolsCache::new(),
            rate_limiter: Arc::new(RateLimiter::aster()),
            book_ticker_cache: Arc::new(RwLock::new(None)),
            stream_manager: None,
        }
    }

    /// Try to initialize with streaming support using StreamManager
    #[cfg(feature = "streaming")]
    async fn try_init_streaming() -> Result<Self> {
        use crate::aster::ws_client::AsterWsClient;
        use perps_core::{StreamConfig, StreamManager};

        tracing::info!("Initializing AsterClient with StreamManager");

        // Create AsterWsClient as the OrderbookStreamer implementation
        let ws_client = Arc::new(AsterWsClient::new());

        // Create StreamManager with default config
        let stream_manager = Arc::new(StreamManager::new(
            ws_client as Arc<dyn perps_core::streaming::OrderbookStreamer>,
            StreamConfig::default(),
        ));

        Ok(Self {
            http: reqwest::Client::new(),
            symbols_cache: SymbolsCache::new(),
            rate_limiter: Arc::new(RateLimiter::aster()),
            book_ticker_cache: Arc::new(RwLock::new(None)),
            stream_manager: Some(stream_manager),
        })
    }

    /// Fallback when streaming feature is disabled
    #[cfg(not(feature = "streaming"))]
    async fn try_init_streaming() -> Result<Self> {
        Err(anyhow!("Streaming feature not enabled"))
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

    /// Fetch book tickers with caching
    async fn fetch_book_tickers_cached(&self) -> Result<HashMap<String, BookTicker>> {
        // Check if cache is valid
        {
            let cache = self.book_ticker_cache.read().unwrap();
            if let Some(cached) = cache.as_ref() {
                if !cached.is_expired() {
                    tracing::debug!(
                        "Using cached book ticker data ({} symbols)",
                        cached.data.len()
                    );
                    return Ok(cached.data.clone());
                }
            }
        }

        // Cache is expired or empty, fetch new data
        tracing::debug!("Fetching fresh book ticker data from API");
        let book_tickers: Vec<BookTicker> = self.get("/fapi/v1/ticker/bookTicker").await?;

        let mut book_ticker_map = HashMap::new();
        for book_ticker in book_tickers {
            book_ticker_map.insert(book_ticker.symbol.clone(), book_ticker);
        }

        // Update cache
        {
            let mut cache = self.book_ticker_cache.write().unwrap();
            *cache = Some(BookTickerCache::new(
                book_ticker_map.clone(),
                BOOK_TICKER_CACHE_TTL_SECS,
            ));
        }

        Ok(book_ticker_map)
    }

    /// Helper method to make GET requests with rate limiting and retry
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
                            tracing::debug!("Requesting: {}", url);
                            let response = http.get(&url).send().await?;

                            if !response.status().is_success() {
                                let status = response.status();
                                let text = response
                                    .text()
                                    .await
                                    .unwrap_or_else(|_| "Unable to read response body".to_string());
                                return Err(anyhow!(
                                    "GET request to {} failed with status: {}. Body: {}",
                                    url,
                                    status,
                                    text
                                ));
                            }

                            let data = response.json::<T>().await?;
                            Ok(data)
                        }
                    })
                    .await
            }
        })
        .await
    }
}

// Note: Cannot implement Default trait because new() is async

#[async_trait]
impl IPerps for AsterClient {
    fn get_name(&self) -> &str {
        "aster"
    }

    fn parse_symbol(&self, symbol: &str) -> String {
        // Aster uses Binance-compatible format: BTC -> BTCUSDT
        format!("{}USDT", symbol.to_uppercase())
    }

    async fn get_markets(&self) -> Result<Vec<Market>> {
        let info: ExchangeInfo = self.get("/fapi/v1/exchangeInfo").await?;

        let markets = info
            .symbols
            .into_iter()
            .filter(|s| s.status == "TRADING")
            .map(|s| {
                let qty_step = Decimal::new(1, s.quantity_precision as u32);

                Market {
                    symbol: s.symbol.clone(),
                    contract: s.symbol,
                    price_scale: s.price_precision,
                    quantity_scale: s.quantity_precision,
                    min_order_qty: qty_step,
                    contract_size: Decimal::ONE,
                    max_order_qty: Decimal::ZERO,
                    min_order_value: Decimal::ZERO,
                    max_leverage: Decimal::ZERO,
                }
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
        // Aster requires combining 24hr ticker, mark price, and book ticker for complete data
        let ticker: Ticker24hr = self
            .get(&format!("/fapi/v1/ticker/24hr?symbol={}", symbol))
            .await?;

        let mark_price_data: MarkPrice = self
            .get(&format!("/fapi/v1/premiumIndex?symbol={}", symbol))
            .await?;

        let book_ticker: BookTicker = self
            .get(&format!("/fapi/v1/ticker/bookTicker?symbol={}", symbol))
            .await?;

        let last_price = Decimal::from_str(&ticker.last_price)?;
        let open_price = Decimal::from_str(&ticker.open_price)?;
        let price_change_24h = last_price - open_price;
        let open_interest = self.get_open_interest(symbol).await?;

        let mark_price = Decimal::from_str(&mark_price_data.mark_price)?;
        let index_price = Decimal::from_str(&mark_price_data.index_price)?;

        Ok(Ticker {
            symbol: ticker.symbol,
            last_price,
            mark_price,
            index_price,
            best_bid_price: Decimal::from_str(&book_ticker.bid_price)?,
            best_bid_qty: Decimal::from_str(&book_ticker.bid_qty)?,
            best_ask_price: Decimal::from_str(&book_ticker.ask_price)?,
            best_ask_qty: Decimal::from_str(&book_ticker.ask_qty)?,
            volume_24h: Decimal::from_str(&ticker.volume)?,
            turnover_24h: Decimal::from_str(&ticker.quote_volume)?,
            open_interest: open_interest.open_interest,
            open_interest_notional: open_interest.open_interest * mark_price,
            price_change_24h,
            price_change_pct: Decimal::from_str(&ticker.price_change_percent)? / Decimal::from(100),
            high_price_24h: Decimal::from_str(&ticker.high_price)?,
            low_price_24h: Decimal::from_str(&ticker.low_price)?,
            timestamp: Utc.timestamp_millis_opt(ticker.close_time).unwrap(),
        })
    }

    async fn get_all_tickers(&self) -> Result<Vec<Ticker>> {
        let tickers: Vec<Ticker24hr> = self.get("/fapi/v1/ticker/24hr").await?;

        // Fetch book tickers with caching
        let book_ticker_map = self.fetch_book_tickers_cached().await?;

        let mut results = Vec::new();
        for ticker in tickers {
            // For all tickers, we skip mark price to avoid too many requests
            // Users should call get_ticker() for individual symbols if they need mark price
            let last_price = match Decimal::from_str(&ticker.last_price) {
                Ok(p) => p,
                Err(_) => continue,
            };
            let open_price = match Decimal::from_str(&ticker.open_price) {
                Ok(p) => p,
                Err(_) => continue,
            };
            let price_change_24h = last_price - open_price;

            // Get book ticker data if available
            let (best_bid_price, best_bid_qty, best_ask_price, best_ask_qty) =
                if let Some(book_ticker) = book_ticker_map.get(&ticker.symbol) {
                    (
                        Decimal::from_str(&book_ticker.bid_price).unwrap_or_else(|e| {
                            tracing::warn!(
                                "Failed to parse bid price for {}: {}",
                                ticker.symbol,
                                e
                            );
                            Decimal::ZERO
                        }),
                        Decimal::from_str(&book_ticker.bid_qty).unwrap_or_else(|e| {
                            tracing::warn!(
                                "Failed to parse bid quantity for {}: {}",
                                ticker.symbol,
                                e
                            );
                            Decimal::ZERO
                        }),
                        Decimal::from_str(&book_ticker.ask_price).unwrap_or_else(|e| {
                            tracing::warn!(
                                "Failed to parse ask price for {}: {}",
                                ticker.symbol,
                                e
                            );
                            Decimal::ZERO
                        }),
                        Decimal::from_str(&book_ticker.ask_qty).unwrap_or_else(|e| {
                            tracing::warn!(
                                "Failed to parse ask quantity for {}: {}",
                                ticker.symbol,
                                e
                            );
                            Decimal::ZERO
                        }),
                    )
                } else {
                    tracing::debug!(
                        "Book ticker data not available for {}, using zeros for best bid/ask",
                        ticker.symbol
                    );
                    (Decimal::ZERO, Decimal::ZERO, Decimal::ZERO, Decimal::ZERO)
                };

            if let Ok(mut t) = self.try_parse_ticker(&ticker, last_price, price_change_24h) {
                // Update with book ticker data
                t.best_bid_price = best_bid_price;
                t.best_bid_qty = best_bid_qty;
                t.best_ask_price = best_ask_price;
                t.best_ask_qty = best_ask_qty;
                results.push(t);
            }
        }

        Ok(results)
    }

    async fn get_orderbook(&self, symbol: &str, depth: u32) -> Result<Orderbook> {
        // Check if StreamManager is available
        if let Some(ref manager) = self.stream_manager {
            // Subscribe to symbol (idempotent - no-op if already subscribed)
            manager.subscribe(symbol.to_string()).await?;

            // Get orderbook from cache or REST fallback
            // StreamManager handles all complexity: caching, staleness checks, WebSocket streaming
            let client_clone = self.clone();
            let symbol_clone = symbol.to_string();
            let orderbook = manager
                .get_orderbook(symbol, depth, || async move {
                    // REST API fallback closure - returns (Orderbook, lastUpdateId) for snapshot initialization
                    client_clone
                        .fetch_orderbook_snapshot_with_update_id(&symbol_clone, depth)
                        .await
                })
                .await?;

            return Ok(orderbook);
        }

        // Fallback when StreamManager is not available: direct REST API call
        self.fetch_orderbook_rest(symbol, depth).await
    }

    async fn get_recent_trades(&self, symbol: &str, limit: u32) -> Result<Vec<Trade>> {
        let limit = limit.min(1000); // Aster max limit is 1000
        let trades: Vec<crate::aster::types::Trade> = self
            .get(&format!(
                "/fapi/v1/trades?symbol={}&limit={}",
                symbol, limit
            ))
            .await?;

        let trades = trades
            .into_iter()
            .map(|t| {
                Ok(Trade {
                    id: t.id.to_string(),
                    symbol: symbol.to_string(),
                    price: Decimal::from_str(&t.price)?,
                    quantity: Decimal::from_str(&t.qty)?,
                    side: if t.is_buyer_maker {
                        OrderSide::Sell
                    } else {
                        OrderSide::Buy
                    },
                    timestamp: Utc.timestamp_millis_opt(t.time).unwrap(),
                })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(trades)
    }

    async fn get_funding_rate(&self, symbol: &str) -> Result<FundingRate> {
        let rates: Vec<crate::aster::types::FundingRate> = self
            .get(&format!("/fapi/v1/fundingRate?symbol={}&limit=1", symbol))
            .await?;

        let rate = rates
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("No funding rate data found for {}", symbol))?;

        Ok(FundingRate {
            symbol: rate.symbol,
            funding_rate: Decimal::from_str(&rate.funding_rate)?,
            funding_time: Utc.timestamp_millis_opt(rate.funding_time).unwrap(),
            predicted_rate: Decimal::ZERO,
            next_funding_time: Utc.timestamp_millis_opt(0).unwrap(), // Not available
            funding_interval: 8, // Default 8 hours for perpetuals
            funding_rate_cap_floor: Decimal::ZERO,
        })
    }

    async fn get_funding_rate_history(
        &self,
        symbol: &str,
        start_time: Option<DateTime<Utc>>,
        end_time: Option<DateTime<Utc>>,
        limit: Option<u32>,
    ) -> Result<Vec<FundingRate>> {
        let mut endpoint = format!("/fapi/v1/fundingRate?symbol={}", symbol);

        if let Some(start) = start_time {
            endpoint.push_str(&format!("&startTime={}", start.timestamp_millis()));
        }
        if let Some(end) = end_time {
            endpoint.push_str(&format!("&endTime={}", end.timestamp_millis()));
        }
        if let Some(lim) = limit {
            endpoint.push_str(&format!("&limit={}", lim.min(1000)));
        }

        let rates: Vec<crate::aster::types::FundingRate> = self.get(&endpoint).await?;

        let funding_rates = rates
            .into_iter()
            .map(|r| {
                Ok(FundingRate {
                    symbol: r.symbol.clone(),
                    funding_rate: Decimal::from_str(&r.funding_rate)?,
                    funding_time: Utc.timestamp_millis_opt(r.funding_time).unwrap(),
                    predicted_rate: Decimal::ZERO,
                    next_funding_time: Utc.timestamp_millis_opt(0).unwrap(),
                    funding_interval: 8,
                    funding_rate_cap_floor: Decimal::ZERO,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(funding_rates)
    }

    async fn get_open_interest(&self, symbol: &str) -> Result<OpenInterest> {
        let oi: crate::aster::types::OpenInterest = self
            .get(&format!("/fapi/v1/openInterest?symbol={}", symbol))
            .await?;

        Ok(OpenInterest {
            symbol: oi.symbol,
            open_interest: Decimal::from_str(&oi.open_interest)?,
            open_value: Decimal::ZERO, // Calculate if price is available
            timestamp: Utc.timestamp_millis_opt(oi.time).unwrap(),
        })
    }

    async fn get_klines(
        &self,
        symbol: &str,
        interval: &str,
        start_time: Option<DateTime<Utc>>,
        end_time: Option<DateTime<Utc>>,
        limit: Option<u32>,
    ) -> Result<Vec<Kline>> {
        // Aster supports same intervals as Binance
        let interval = match interval {
            "1m" | "3m" | "5m" | "15m" | "30m" | "1h" | "2h" | "4h" | "6h" | "8h" | "12h"
            | "1d" | "3d" | "1w" | "1M" => interval,
            _ => {
                return Err(anyhow!(
                    "Unsupported interval '{}' for Aster. Supported: 1m, 3m, 5m, 15m, 30m, 1h, 2h, 4h, 6h, 8h, 12h, 1d, 3d, 1w, 1M",
                    interval
                ))
            }
        };

        let mut endpoint = format!("/fapi/v1/klines?symbol={}&interval={}", symbol, interval);

        if let Some(start) = start_time {
            endpoint.push_str(&format!("&startTime={}", start.timestamp_millis()));
        }
        if let Some(end) = end_time {
            endpoint.push_str(&format!("&endTime={}", end.timestamp_millis()));
        }
        if let Some(lim) = limit {
            endpoint.push_str(&format!("&limit={}", lim.min(1500)));
        }

        let klines: Vec<Vec<serde_json::Value>> = self.get(&endpoint).await?;

        let klines = klines
            .into_iter()
            .filter_map(|k| {
                if k.len() < 12 {
                    tracing::warn!(
                        "Invalid kline data format for Aster: expected 12 fields, got {}",
                        k.len()
                    );
                    return None;
                }

                // Aster kline format (same as Binance):
                // [0] open_time, [1] open, [2] high, [3] low, [4] close, [5] volume,
                // [6] close_time, [7] quote_asset_volume, [8] number_of_trades,
                // [9] taker_buy_base_asset_volume, [10] taker_buy_quote_asset_volume, [11] ignore
                let open_time = k[0].as_i64()?;
                let close_time = k[6].as_i64()?;
                let open = Decimal::from_str(k[1].as_str()?).ok()?;
                let high = Decimal::from_str(k[2].as_str()?).ok()?;
                let low = Decimal::from_str(k[3].as_str()?).ok()?;
                let close = Decimal::from_str(k[4].as_str()?).ok()?;
                let volume = Decimal::from_str(k[5].as_str()?).ok()?;
                let quote_volume = Decimal::from_str(k[7].as_str()?).ok()?;

                Some(Kline {
                    symbol: symbol.to_string(),
                    interval: interval.to_string(),
                    open_time: Utc.timestamp_millis_opt(open_time).single()?,
                    close_time: Utc.timestamp_millis_opt(close_time).single()?,
                    open,
                    high,
                    low,
                    close,
                    volume,
                    turnover: quote_volume,
                })
            })
            .collect();

        Ok(klines)
    }

    async fn is_supported(&self, symbol: &str) -> Result<bool> {
        self.ensure_cache_initialized().await?;
        Ok(self.symbols_cache.contains(symbol).await)
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
            volume_24h: ticker.volume_24h,
            turnover_24h: ticker.turnover_24h,
            open_interest: oi.open_interest,
            funding_rate: funding.funding_rate,
            price_change_24h: ticker.price_change_24h,
            price_change_pct: ticker.price_change_pct,
            high_price_24h: ticker.high_price_24h,
            low_price_24h: ticker.low_price_24h,
            timestamp: Utc::now(),
        })
    }

    async fn get_all_market_stats(&self) -> Result<Vec<MarketStats>> {
        let markets = self.get_markets().await?;
        let mut stats = Vec::new();

        for market in markets {
            match self.get_market_stats(&market.symbol).await {
                Ok(stat) => stats.push(stat),
                Err(e) => tracing::warn!("Failed to get stats for {}: {}", market.symbol, e),
            }
        }

        Ok(stats)
    }
}

impl AsterClient {
    /// Fetch orderbook snapshot with lastUpdateId from REST API
    /// Returns (Orderbook, lastUpdateId) for StreamManager initialization
    async fn fetch_orderbook_snapshot_with_update_id(
        &self,
        symbol: &str,
        depth: u32,
    ) -> Result<(Orderbook, u64)> {
        let limit = depth.min(1000); // Aster max depth is 1000
        let orderbook: Depth = self
            .get(&format!("/fapi/v1/depth?symbol={}&limit={}", symbol, limit))
            .await?;

        // Extract lastUpdateId
        let last_update_id = orderbook.last_update_id as u64;

        let bids = orderbook
            .bids
            .into_iter()
            .map(|(price, quantity)| {
                Ok(OrderbookLevel {
                    price: Decimal::from_str(&price)?,
                    quantity: Decimal::from_str(&quantity)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let asks = orderbook
            .asks
            .into_iter()
            .map(|(price, quantity)| {
                Ok(OrderbookLevel {
                    price: Decimal::from_str(&price)?,
                    quantity: Decimal::from_str(&quantity)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok((
            Orderbook {
                symbol: symbol.to_string(),
                bids,
                asks,
                timestamp: Utc
                    .timestamp_millis_opt(orderbook.transaction_time)
                    .unwrap(),
            },
            last_update_id,
        ))
    }

    /// Fetch orderbook directly from REST API (used as fallback when StreamManager is not available)
    async fn fetch_orderbook_rest(&self, symbol: &str, depth: u32) -> Result<Orderbook> {
        let limit = depth.min(1000); // Aster max depth is 1000
        let orderbook: Depth = self
            .get(&format!("/fapi/v1/depth?symbol={}&limit={}", symbol, limit))
            .await?;

        let bids = orderbook
            .bids
            .into_iter()
            .map(|(price, quantity)| {
                Ok(OrderbookLevel {
                    price: Decimal::from_str(&price)?,
                    quantity: Decimal::from_str(&quantity)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let asks = orderbook
            .asks
            .into_iter()
            .map(|(price, quantity)| {
                Ok(OrderbookLevel {
                    price: Decimal::from_str(&price)?,
                    quantity: Decimal::from_str(&quantity)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(Orderbook {
            symbol: symbol.to_string(),
            bids,
            asks,
            timestamp: Utc
                .timestamp_millis_opt(orderbook.transaction_time)
                .unwrap(),
        })
    }

    /// Helper to parse ticker without mark price data
    fn try_parse_ticker(
        &self,
        ticker: &Ticker24hr,
        last_price: Decimal,
        price_change_24h: Decimal,
    ) -> Result<Ticker> {
        Ok(Ticker {
            symbol: ticker.symbol.clone(),
            last_price,
            mark_price: last_price, // Fallback to last price
            index_price: last_price,
            best_bid_price: Decimal::ZERO,
            best_bid_qty: Decimal::ZERO,
            best_ask_price: Decimal::ZERO,
            best_ask_qty: Decimal::ZERO,
            volume_24h: Decimal::from_str(&ticker.volume)?,
            turnover_24h: Decimal::from_str(&ticker.quote_volume)?,
            open_interest: Decimal::ZERO,
            open_interest_notional: Decimal::ZERO,
            price_change_24h,
            price_change_pct: Decimal::from_str(&ticker.price_change_percent)? / Decimal::from(100),
            high_price_24h: Decimal::from_str(&ticker.high_price)?,
            low_price_24h: Decimal::from_str(&ticker.low_price)?,
            timestamp: Utc.timestamp_millis_opt(ticker.close_time).unwrap(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_parse_symbol() {
        let client = AsterClient::new_rest_only();
        assert_eq!(client.parse_symbol("BTC"), "BTCUSDT");
        assert_eq!(client.parse_symbol("ETH"), "ETHUSDT");
        assert_eq!(client.parse_symbol("btc"), "BTCUSDT");
    }

    #[test]
    fn test_client_creation() {
        let client = AsterClient::new_rest_only();
        assert_eq!(client.get_name(), "aster");
    }

    #[test]
    fn test_try_parse_ticker_valid_data() {
        let client = AsterClient::new_rest_only();

        let ticker = Ticker24hr {
            symbol: "BTCUSDT".to_string(),
            price_change: "1000.50".to_string(),
            price_change_percent: "2.5".to_string(),
            weighted_avg_price: "40000.00".to_string(),
            last_price: "41000.50".to_string(),
            last_qty: "0.1".to_string(),
            open_price: "40000.00".to_string(),
            high_price: "42000.00".to_string(),
            low_price: "39000.00".to_string(),
            volume: "1000.5".to_string(),
            quote_volume: "41000000.25".to_string(),
            open_time: 1640000000000,
            close_time: 1640086400000,
            first_id: 1,
            last_id: 10000,
            count: 9999,
        };

        let last_price = Decimal::from_str("41000.50").unwrap();
        let price_change_24h = Decimal::from_str("1000.50").unwrap();

        let result = client.try_parse_ticker(&ticker, last_price, price_change_24h);
        assert!(result.is_ok());

        let parsed = result.unwrap();
        assert_eq!(parsed.symbol, "BTCUSDT");
        assert_eq!(parsed.last_price, Decimal::from_str("41000.50").unwrap());
        assert_eq!(parsed.volume_24h, Decimal::from_str("1000.5").unwrap());
        assert_eq!(
            parsed.turnover_24h,
            Decimal::from_str("41000000.25").unwrap()
        );
        assert_eq!(
            parsed.price_change_24h,
            Decimal::from_str("1000.50").unwrap()
        );
        assert_eq!(parsed.price_change_pct, Decimal::from_str("0.025").unwrap()); // 2.5% / 100
        assert_eq!(
            parsed.high_price_24h,
            Decimal::from_str("42000.00").unwrap()
        );
        assert_eq!(parsed.low_price_24h, Decimal::from_str("39000.00").unwrap());

        // Best bid/ask should be zero in try_parse_ticker (filled later from book ticker)
        assert_eq!(parsed.best_bid_price, Decimal::ZERO);
        assert_eq!(parsed.best_ask_price, Decimal::ZERO);
    }

    #[test]
    fn test_try_parse_ticker_invalid_number() {
        let client = AsterClient::new_rest_only();

        let ticker = Ticker24hr {
            symbol: "BTCUSDT".to_string(),
            price_change: "invalid".to_string(), // Invalid number
            price_change_percent: "2.5".to_string(),
            weighted_avg_price: "40000.00".to_string(),
            last_price: "41000.50".to_string(),
            last_qty: "0.1".to_string(),
            open_price: "40000.00".to_string(),
            high_price: "42000.00".to_string(),
            low_price: "39000.00".to_string(),
            volume: "invalid".to_string(), // Invalid number
            quote_volume: "41000000.25".to_string(),
            open_time: 1640000000000,
            close_time: 1640086400000,
            first_id: 1,
            last_id: 10000,
            count: 9999,
        };

        let last_price = Decimal::from_str("41000.50").unwrap();
        let price_change_24h = Decimal::from_str("1000.50").unwrap();

        let result = client.try_parse_ticker(&ticker, last_price, price_change_24h);
        assert!(result.is_err());
    }

    #[test]
    fn test_book_ticker_deserialization() {
        let json = r#"{
            "symbol": "BTCUSDT",
            "bidPrice": "40000.50",
            "bidQty": "10.5",
            "askPrice": "40001.00",
            "askQty": "5.25",
            "time": 1640086400000
        }"#;

        let book_ticker: Result<BookTicker, _> = serde_json::from_str(json);
        assert!(book_ticker.is_ok());

        let bt = book_ticker.unwrap();
        assert_eq!(bt.symbol, "BTCUSDT");
        assert_eq!(bt.bid_price, "40000.50");
        assert_eq!(bt.bid_qty, "10.5");
        assert_eq!(bt.ask_price, "40001.00");
        assert_eq!(bt.ask_qty, "5.25");
        assert_eq!(bt.time, 1640086400000);
    }

    #[test]
    fn test_ticker24hr_deserialization() {
        let json = r#"{
            "symbol": "BTCUSDT",
            "priceChange": "1000.50",
            "priceChangePercent": "2.5",
            "weightedAvgPrice": "40000.00",
            "lastPrice": "41000.50",
            "lastQty": "0.1",
            "openPrice": "40000.00",
            "highPrice": "42000.00",
            "lowPrice": "39000.00",
            "volume": "1000.5",
            "quoteVolume": "41000000.25",
            "openTime": 1640000000000,
            "closeTime": 1640086400000,
            "firstId": 1,
            "lastId": 10000,
            "count": 9999
        }"#;

        let ticker: Result<Ticker24hr, _> = serde_json::from_str(json);
        assert!(ticker.is_ok());

        let t = ticker.unwrap();
        assert_eq!(t.symbol, "BTCUSDT");
        assert_eq!(t.last_price, "41000.50");
        assert_eq!(t.volume, "1000.5");
        assert_eq!(t.quote_volume, "41000000.25");
    }

    #[test]
    fn test_mark_price_deserialization() {
        let json = r#"{
            "symbol": "BTCUSDT",
            "markPrice": "41000.25",
            "indexPrice": "41000.00",
            "estimatedSettlePrice": "41000.10",
            "lastFundingRate": "0.0001",
            "nextFundingTime": 1640097600000,
            "interestRate": "0.0005",
            "time": 1640086400000
        }"#;

        let mark_price: Result<MarkPrice, _> = serde_json::from_str(json);
        assert!(mark_price.is_ok());

        let mp = mark_price.unwrap();
        assert_eq!(mp.symbol, "BTCUSDT");
        assert_eq!(mp.mark_price, "41000.25");
        assert_eq!(mp.index_price, "41000.00");
        assert_eq!(mp.last_funding_rate, "0.0001");
    }

    #[test]
    fn test_best_bid_ask_conversion() {
        // Test that bid/ask prices and quantities are correctly parsed from strings to Decimal
        let bid_price_str = "40000.50";
        let bid_qty_str = "10.5";
        let ask_price_str = "40001.00";
        let ask_qty_str = "5.25";

        let bid_price = Decimal::from_str(bid_price_str).unwrap();
        let bid_qty = Decimal::from_str(bid_qty_str).unwrap();
        let ask_price = Decimal::from_str(ask_price_str).unwrap();
        let ask_qty = Decimal::from_str(ask_qty_str).unwrap();

        assert_eq!(bid_price, Decimal::new(4000050, 2));
        assert_eq!(bid_qty, Decimal::new(105, 1));
        assert_eq!(ask_price, Decimal::new(40001, 0));
        assert_eq!(ask_qty, Decimal::new(525, 2));

        // Verify spread is positive (ask > bid)
        assert!(ask_price > bid_price);
    }

    #[test]
    fn test_empty_book_ticker_fallback() {
        // Test that when book ticker data is missing, we fall back to zeros gracefully
        let empty_map: HashMap<String, BookTicker> = HashMap::new();

        let symbol = "BTCUSDT".to_string();
        let result = empty_map.get(&symbol);

        assert!(result.is_none());

        // This simulates the fallback behavior in get_all_tickers
        let (bid, bid_qty, ask, ask_qty) = if result.is_some() {
            (Decimal::ONE, Decimal::ONE, Decimal::ONE, Decimal::ONE)
        } else {
            (Decimal::ZERO, Decimal::ZERO, Decimal::ZERO, Decimal::ZERO)
        };

        assert_eq!(bid, Decimal::ZERO);
        assert_eq!(ask, Decimal::ZERO);
    }
}
