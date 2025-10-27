use crate::cache::SymbolsCache;
use crate::extended::types::*;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use perps_core::types::*;
use perps_core::{execute_with_retry, IPerps, RateLimiter, RetryConfig};
use rust_decimal::Decimal;
use std::collections::HashSet;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::RwLock;

const BASE_URL: &str = "https://api.starknet.extended.exchange/api/v1";

/// A client for the Extended Exchange (Starknet L2 DEX).
#[derive(Clone)]
pub struct ExtendedClient {
    http: reqwest::Client,
    /// Cached set of supported symbols
    symbols_cache: SymbolsCache,
    /// Rate limiter for API requests
    rate_limiter: Arc<RateLimiter>,
    /// Optional stream manager for WebSocket-based orderbook streaming
    #[cfg(feature = "streaming")]
    stream_manager: Option<Arc<perps_core::StreamManager>>,
    /// Track active WebSocket streams (for backward compatibility with old code)
    #[cfg(feature = "streaming")]
    #[allow(dead_code)]
    active_streams: Arc<RwLock<HashSet<String>>>,
}

impl ExtendedClient {
    /// Create a new REST-only client (no WebSocket streaming)
    pub fn new_rest_only() -> Self {
        // Build HTTP client with minimal headers to match curl behavior
        let http = reqwest::Client::builder()
            .user_agent("perps-stats/0.1.0") // Simple user agent
            .build()
            .expect("Failed to build HTTP client");

        Self {
            http,
            symbols_cache: SymbolsCache::new(),
            rate_limiter: Arc::new(RateLimiter::extended()),
            #[cfg(feature = "streaming")]
            stream_manager: None,
            #[cfg(feature = "streaming")]
            active_streams: Arc::new(RwLock::new(HashSet::new())),
        }
    }

    /// Create a new client with optional WebSocket streaming
    /// Automatically enables streaming if DATABASE_URL is set
    pub async fn new() -> Result<Self> {
        let should_enable_streaming = std::env::var("DATABASE_URL").is_ok()
            && std::env::var("ENABLE_ORDERBOOK_STREAMING")
                .map(|v| v.to_lowercase() != "false")
                .unwrap_or(true);

        if !should_enable_streaming {
            tracing::debug!("ExtendedClient: Streaming disabled, using REST-only mode");
            return Ok(Self::new_rest_only());
        }

        #[cfg(feature = "streaming")]
        {
            match Self::try_init_streaming().await {
                Ok(client) => {
                    tracing::info!("✓ ExtendedClient initialized with WebSocket streaming");
                    Ok(client)
                }
                Err(e) => {
                    tracing::warn!("Failed to initialize streaming for ExtendedClient: {}", e);
                    tracing::warn!("Falling back to REST-only mode");
                    Ok(Self::new_rest_only())
                }
            }
        }

        #[cfg(not(feature = "streaming"))]
        {
            tracing::debug!("ExtendedClient: Streaming feature not enabled, using REST-only mode");
            Ok(Self::new_rest_only())
        }
    }

    /// Try to initialize with streaming support using StreamManager
    #[cfg(feature = "streaming")]
    async fn try_init_streaming() -> Result<Self> {
        use super::ws_client::ExtendedWsClient;
        use perps_core::{StreamConfig, StreamManager};

        tracing::info!("Initializing ExtendedClient with StreamManager");

        let http = reqwest::Client::builder()
            .user_agent("perps-stats/0.1.0")
            .build()
            .expect("Failed to build HTTP client");

        // Create WebSocket client
        let ws_client = Arc::new(ExtendedWsClient::new());

        // Create StreamManager with default config
        let stream_manager = Arc::new(StreamManager::new(
            ws_client as Arc<dyn perps_core::OrderbookStreamer>,
            StreamConfig::default(),
        ));

        Ok(Self {
            http,
            symbols_cache: SymbolsCache::new(),
            rate_limiter: Arc::new(RateLimiter::extended()),
            stream_manager: Some(stream_manager),
            active_streams: Arc::new(RwLock::new(HashSet::new())),
        })
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

                            // Parse response wrapper
                            let wrapper: ExtendedResponse<T> = response.json().await?;
                            if wrapper.status != "OK" {
                                let error = wrapper.error.unwrap_or(ExtendedError {
                                    code: "UNKNOWN".to_string(),
                                    message: "Unknown error".to_string(),
                                });
                                return Err(anyhow!(
                                    "Extended API error: {} - {}",
                                    error.code,
                                    error.message
                                ));
                            }

                            wrapper
                                .data
                                .ok_or_else(|| anyhow!("No data in response from {}", url))
                        }
                    })
                    .await
            }
        })
        .await
    }

    /// Normalize Extended symbol to global format (BTC-USD -> BTC)
    fn normalize_symbol(exchange_symbol: &str) -> String {
        exchange_symbol
            .split('-')
            .next()
            .unwrap_or(exchange_symbol)
            .to_string()
    }
}

impl Default for ExtendedClient {
    fn default() -> Self {
        Self::new_rest_only()
    }
}

#[async_trait]
impl IPerps for ExtendedClient {
    fn get_name(&self) -> &str {
        "extended"
    }

    fn parse_symbol(&self, symbol: &str) -> String {
        // Check if symbol is already in Extended format (e.g., "BTC-USD")
        let symbol_upper = symbol.to_uppercase();

        // If already contains "-USD" suffix, return as-is
        if symbol_upper.ends_with("-USD") {
            return symbol_upper;
        }

        // Otherwise, convert global symbol format (e.g., "BTC") to Extended format ("BTC-USD")
        format!("{}-USD", symbol_upper)
    }

    async fn get_markets(&self) -> Result<Vec<Market>> {
        let markets: Vec<ExtendedMarket> = self.get("/info/markets").await?;

        let markets = markets
            .into_iter()
            .filter(|m| m.active && m.status == "ACTIVE")
            .map(|m| {
                // Calculate precision from asset precision
                let price_scale = m.collateral_asset_precision.max(0);
                let quantity_scale = m.asset_precision.max(0);

                Market {
                    symbol: Self::normalize_symbol(&m.name),
                    contract: m.name.clone(),
                    price_scale,
                    quantity_scale,
                    // Fields not provided by Extended API
                    min_order_qty: Decimal::new(1, quantity_scale as u32),
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
        let exchange_symbol = self.parse_symbol(symbol);

        // Query for specific market using query parameter
        let markets: Vec<ExtendedMarket> = self
            .get(&format!("/info/markets?market={}", exchange_symbol))
            .await?;

        let market = markets
            .into_iter()
            .find(|m| m.active && m.status == "ACTIVE" && m.name == exchange_symbol)
            .ok_or_else(|| anyhow!("Market {} not found or inactive", symbol))?;

        // Calculate precision from asset precision
        let price_scale = market.collateral_asset_precision.max(0);
        let quantity_scale = market.asset_precision.max(0);

        Ok(Market {
            symbol: Self::normalize_symbol(&market.name),
            contract: market.name.clone(),
            price_scale,
            quantity_scale,
            min_order_qty: Decimal::new(1, quantity_scale as u32),
            contract_size: Decimal::ONE,
            max_order_qty: Decimal::ZERO,
            min_order_value: Decimal::ZERO,
            max_leverage: Decimal::ZERO,
        })
    }

    async fn get_ticker(&self, symbol: &str) -> Result<Ticker> {
        let exchange_symbol = self.parse_symbol(symbol);
        let stats: ExtendedMarketStats = self
            .get(&format!("/info/markets/{}/stats", exchange_symbol))
            .await?;

        let last_price = Decimal::from_str(&stats.last_price).unwrap_or(Decimal::ZERO);
        let mark_price = Decimal::from_str(&stats.mark_price).unwrap_or(Decimal::ZERO);
        let index_price = Decimal::from_str(&stats.index_price).unwrap_or(Decimal::ZERO);
        let mut bid_price = Decimal::from_str(&stats.bid_price).unwrap_or(Decimal::ZERO);
        let mut ask_price = Decimal::from_str(&stats.ask_price).unwrap_or(Decimal::ZERO);
        let open_interest = Decimal::from_str(&stats.open_interest).unwrap_or(Decimal::ZERO);
        let price_change_pct =
            Decimal::from_str(&stats.daily_price_change_percentage).unwrap_or(Decimal::ZERO);
        let price_change_24h =
            Decimal::from_str(&stats.daily_price_change).unwrap_or(Decimal::ZERO);

        // Fetch orderbook to get best bid/ask quantities
        // Use minimal depth (1) to reduce API overhead
        let (best_bid_qty, best_ask_qty) = match self.get_orderbook(symbol, 1).await {
            Ok(multi_orderbook) => {
                // Use best available resolution
                if let Some(orderbook) = multi_orderbook.best_for_tight_spreads() {
                    bid_price = orderbook
                        .bids
                        .first()
                        .map(|level| level.price)
                        .unwrap_or(bid_price);

                    let bid_qty = orderbook
                        .bids
                        .first()
                        .map(|level| level.quantity)
                        .unwrap_or(Decimal::ZERO);

                    ask_price = orderbook
                        .asks
                        .first()
                        .map(|level| level.price)
                        .unwrap_or(ask_price);
                    let ask_qty = orderbook
                        .asks
                        .first()
                        .map(|level| level.quantity)
                        .unwrap_or(Decimal::ZERO);
                    (bid_qty, ask_qty)
                } else {
                    (Decimal::ZERO, Decimal::ZERO)
                }
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to fetch orderbook for {} to get quantities: {}",
                    symbol,
                    e
                );
                (Decimal::ZERO, Decimal::ZERO)
            }
        };

        Ok(Ticker {
            symbol: symbol.to_string(),
            last_price,
            mark_price,
            index_price,
            best_bid_price: bid_price,
            best_bid_qty,
            best_ask_price: ask_price,
            best_ask_qty,
            volume_24h: Decimal::from_str(&stats.daily_volume_base).unwrap_or(Decimal::ZERO),
            turnover_24h: Decimal::from_str(&stats.daily_volume).unwrap_or(Decimal::ZERO),
            open_interest: open_interest / mark_price,
            open_interest_notional: open_interest,
            price_change_24h,
            price_change_pct,
            high_price_24h: Decimal::from_str(&stats.daily_high).unwrap_or(Decimal::ZERO),
            low_price_24h: Decimal::from_str(&stats.daily_low).unwrap_or(Decimal::ZERO),
            timestamp: Utc::now(),
        })
    }

    async fn get_all_tickers(&self) -> Result<Vec<Ticker>> {
        let markets = self.get_markets().await?;
        let mut tickers = Vec::new();

        for market in markets {
            match self.get_ticker(&market.symbol).await {
                Ok(ticker) => tickers.push(ticker),
                Err(e) => {
                    tracing::warn!("Failed to fetch ticker for {}: {}", market.symbol, e)
                }
            }
        }

        Ok(tickers)
    }

    async fn get_orderbook(&self, symbol: &str, depth: u32) -> Result<MultiResolutionOrderbook> {
        // Check if StreamManager is available (streaming mode)
        #[cfg(feature = "streaming")]
        if let Some(ref manager) = self.stream_manager {
            let exchange_symbol = self.parse_symbol(symbol);

            // Subscribe (idempotent, auto-starts streaming)
            manager.subscribe(exchange_symbol.clone()).await?;

            // Get orderbook (auto cache + fallback)
            let orderbook = manager
                .get_orderbook(&exchange_symbol, depth, || async move {
                    // REST fallback returns (Orderbook, lastUpdateId) for snapshot initialization
                    Err(anyhow!("REST fallback not set"))
                })
                .await?;

            return Ok(MultiResolutionOrderbook::from_single(orderbook));
        }

        // REST API fallback
        let exchange_symbol = self.parse_symbol(symbol);
        let orderbook: ExtendedOrderbook = self
            .get(&format!("/info/markets/{}/orderbook", exchange_symbol))
            .await?;

        let bids = orderbook
            .bid
            .into_iter()
            .map(|level| {
                Ok(OrderbookLevel {
                    price: Decimal::from_str(&level.price)?,
                    quantity: Decimal::from_str(&level.qty)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let asks = orderbook
            .ask
            .into_iter()
            .map(|level| {
                Ok(OrderbookLevel {
                    price: Decimal::from_str(&level.price)?,
                    quantity: Decimal::from_str(&level.qty)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let single_orderbook = Orderbook {
            symbol: symbol.to_string(),
            bids,
            asks,
            timestamp: Utc::now(),
        };

        Ok(MultiResolutionOrderbook::from_single(single_orderbook))
    }

    async fn get_recent_trades(&self, symbol: &str, limit: u32) -> Result<Vec<Trade>> {
        let exchange_symbol = self.parse_symbol(symbol);
        let trades: Vec<ExtendedTrade> = self
            .get(&format!(
                "/info/markets/{}/trades?limit={}",
                exchange_symbol, limit
            ))
            .await?;

        let trades = trades
            .into_iter()
            .map(|t| {
                Ok(Trade {
                    id: t.id.to_string(),
                    symbol: symbol.to_string(),
                    price: Decimal::from_str(&t.price)?,
                    quantity: Decimal::from_str(&t.quantity)?,
                    side: if t.side == "BUY" {
                        OrderSide::Buy
                    } else {
                        OrderSide::Sell
                    },
                    timestamp: Utc.timestamp_millis_opt(t.timestamp).unwrap(),
                })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(trades)
    }

    async fn get_funding_rate(&self, symbol: &str) -> Result<FundingRate> {
        // Extended doesn't have a separate funding rates endpoint
        // Funding rate is included in the stats response
        let exchange_symbol = self.parse_symbol(symbol);
        let stats: ExtendedMarketStats = self
            .get(&format!("/info/markets/{}/stats", exchange_symbol))
            .await?;

        let funding_rate = Decimal::from_str(&stats.funding_rate).unwrap_or(Decimal::ZERO);

        // next_funding_rate is a timestamp in milliseconds
        let next_funding_time = if stats.next_funding_rate > 0 {
            Utc.timestamp_millis_opt(stats.next_funding_rate).unwrap()
        } else {
            Utc::now() + chrono::Duration::hours(8) // Default to 8 hours from now
        };

        Ok(FundingRate {
            symbol: symbol.to_string(),
            funding_rate,
            funding_time: Utc::now(), // Current time as Extended doesn't provide last funding time
            predicted_rate: Decimal::ZERO, // Not provided
            next_funding_time,
            funding_interval: 8,                   // Assume 8 hours (standard)
            funding_rate_cap_floor: Decimal::ZERO, // Not provided
        })
    }

    async fn get_funding_rate_history(
        &self,
        _symbol: &str,
        _start_time: Option<DateTime<Utc>>,
        _end_time: Option<DateTime<Utc>>,
        _limit: Option<u32>,
    ) -> Result<Vec<FundingRate>> {
        // Extended Exchange doesn't provide funding rate history
        // Only current funding rate is available in the stats endpoint
        Err(anyhow!(
            "get_funding_rate_history is not supported by Extended Exchange (only current rate available via get_funding_rate)"
        ))
    }

    async fn get_open_interest(&self, symbol: &str) -> Result<OpenInterest> {
        let ticker = self.get_ticker(symbol).await?;

        Ok(OpenInterest {
            symbol: symbol.to_string(),
            open_interest: ticker.open_interest,
            open_value: ticker.open_interest_notional,
            timestamp: Utc::now(),
        })
    }

    async fn get_klines(
        &self,
        symbol: &str,
        interval: &str,
        _start_time: Option<DateTime<Utc>>,
        end_time: Option<DateTime<Utc>>,
        limit: Option<u32>,
    ) -> Result<Vec<Kline>> {
        let exchange_symbol = self.parse_symbol(symbol);

        // Map interval to Extended format (need to verify supported intervals)
        // Extended supports intervals like "P1H" (1 hour), "P1D" (1 day)
        let extended_interval = match interval {
            "1m" => "PT1M",
            "5m" => "PT5M",
            "15m" => "PT15M",
            "30m" => "PT30M",
            "1h" => "PT1H",
            "4h" => "PT4H",
            "1d" => "PT24H",
            _ => return Err(anyhow!("Unsupported interval: {}", interval)),
        };

        // Calculate interval duration in seconds for close_time
        let interval_seconds = match interval {
            "1m" => 60,
            "5m" => 300,
            "15m" => 900,
            "30m" => 1800,
            "1h" => 3600,
            "4h" => 14400,
            "1d" => 86400,
            _ => 3600,
        };

        let mut endpoint = format!(
            "/info/candles/{}/trades?interval={}",
            exchange_symbol, extended_interval
        );

        // if let Some(start) = start_time {
        //     endpoint.push_str(&format!("&from={}", start.timestamp_millis()));
        // }
        if let Some(end) = end_time {
            endpoint.push_str(&format!("&endTime={}", end.timestamp_millis()));
        }
        if let Some(lim) = limit {
            endpoint.push_str(&format!("&limit={}", lim));
        }

        let klines: Vec<ExtendedKline> = self.get(&endpoint).await?;

        let klines = klines
            .into_iter()
            .map(|k| {
                let open_time = Utc.timestamp_millis_opt(k.timestamp).unwrap();
                let close_time = open_time + chrono::Duration::seconds(interval_seconds);

                Ok(Kline {
                    symbol: symbol.to_string(),
                    interval: interval.to_string(),
                    open_time,
                    close_time,
                    open: Decimal::from_str(&k.open)?,
                    high: Decimal::from_str(&k.high)?,
                    low: Decimal::from_str(&k.low)?,
                    close: Decimal::from_str(&k.close)?,
                    volume: Decimal::from_str(&k.volume)?,
                    turnover: Decimal::ZERO, // Calculate if needed: volume * average price
                })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(klines)
    }

    async fn is_supported(&self, symbol: &str) -> Result<bool> {
        // Cache stores global symbols (e.g., "BTC"), so normalize input first
        let normalized_symbol = Self::normalize_symbol(symbol);
        self.ensure_cache_initialized().await?;
        Ok(self.symbols_cache.contains(&normalized_symbol).await)
    }

    async fn get_market_stats(&self, _symbol: &str) -> Result<MarketStats> {
        Err(anyhow!(
            "get_market_stats is not implemented for Extended (use get_ticker instead)"
        ))
    }

    async fn get_all_market_stats(&self) -> Result<Vec<MarketStats>> {
        Err(anyhow!(
            "get_all_market_stats is not implemented for Extended (use get_all_tickers instead)"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_symbol() {
        let client = ExtendedClient::default();
        // Test conversion from global format
        assert_eq!(client.parse_symbol("BTC"), "BTC-USD");
        assert_eq!(client.parse_symbol("ETH"), "ETH-USD");
        assert_eq!(client.parse_symbol("btc"), "BTC-USD");

        // Test idempotency - already in Extended format
        assert_eq!(client.parse_symbol("BTC-USD"), "BTC-USD");
        assert_eq!(client.parse_symbol("ETH-USD"), "ETH-USD");
        assert_eq!(client.parse_symbol("btc-usd"), "BTC-USD");
    }

    #[test]
    fn test_normalize_symbol() {
        assert_eq!(ExtendedClient::normalize_symbol("BTC-USD"), "BTC");
        assert_eq!(ExtendedClient::normalize_symbol("ETH-USD"), "ETH");
        assert_eq!(ExtendedClient::normalize_symbol("BTC"), "BTC");
    }
}
