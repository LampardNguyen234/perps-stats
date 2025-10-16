use async_trait::async_trait;
use chrono::{DateTime, Utc};
use perps_core::*;
use rust_decimal::Decimal;
use std::sync::Arc;
use tracing::{debug, warn};
use perps_core::{RetryConfig, execute_with_retry};

use crate::cache::SymbolsCache;
use super::conversions::*;
use super::error::BinanceError;
use super::ticker_calculator::{calculate_ticker_from_klines, parse_timeframe};

const DEFAULT_STALENESS: u64 = 2;

/// Binance Futures client implementing the IPerps trait
pub struct BinanceClient {
    /// Optional API key for authenticated requests (future use for private endpoints)
    #[allow(dead_code)]
    api_key: Option<String>,
    /// Optional secret key for authenticated requests (future use for private endpoints)
    #[allow(dead_code)]
    secret_key: Option<String>,
    /// Base URL for the Binance Futures API
    base_url: String,
    /// HTTP client
    client: reqwest::Client,
    /// Cached set of supported symbols (normalized format)
    symbols_cache: SymbolsCache,
    /// Rate limiter for API calls
    rate_limiter: Arc<RateLimiter>,
    /// Optional orderbook manager for local orderbook maintenance
    orderbook_manager: Option<Arc<perps_core::OrderbookManager>>,
}

impl BinanceClient {
    /// Create a new Binance client without authentication (public endpoints only)
    ///
    /// Automatically enables WebSocket streaming if:
    /// - DATABASE_URL environment variable is set
    /// - ENABLE_ORDERBOOK_STREAMING is not set to "false"
    ///
    /// Falls back gracefully to REST-only mode if streaming initialization fails.
    pub async fn new() -> anyhow::Result<Self> {
        // Check if streaming should be enabled
        let should_enable_streaming = std::env::var("DATABASE_URL").is_ok()
            && std::env::var("ENABLE_ORDERBOOK_STREAMING")
                .map(|v| v.to_lowercase() != "false")
                .unwrap_or(true);

        if !should_enable_streaming {
            tracing::debug!("BinanceClient: Streaming disabled, using REST-only mode");
            return Ok(Self::new_rest_only());
        }

        // Try to initialize with streaming
        match Self::try_init_streaming().await {
            Ok(client) => {
                tracing::info!("✓ BinanceClient initialized with WebSocket streaming");
                Ok(client)
            }
            Err(e) => {
                tracing::warn!("Failed to initialize streaming for BinanceClient: {}", e);
                tracing::warn!("Falling back to REST-only mode");
                Ok(Self::new_rest_only())
            }
        }
    }

    /// Create a REST-only client (no streaming)
    pub fn new_rest_only() -> Self {
        Self {
            api_key: None,
            secret_key: None,
            base_url: "https://fapi.binance.com".to_string(),
            client: reqwest::Client::new(),
            symbols_cache: SymbolsCache::new(),
            rate_limiter: Arc::new(RateLimiter::binance()),
            orderbook_manager: None,
        }
    }

    /// Try to initialize with streaming support using OrderbookManager
    #[cfg(feature = "streaming")]
    async fn try_init_streaming() -> anyhow::Result<Self> {
        use perps_core::{OrderbookManager, OrderbookManagerConfig};

        // Get database URL
        let database_url = std::env::var("DATABASE_URL")
            .map_err(|_| anyhow::anyhow!("DATABASE_URL not set"))?;

        tracing::info!("Initializing BinanceClient with OrderbookManager (lazy symbol detection)");

        // Connect to database
        let pool = sqlx::PgPool::connect(&database_url).await?;
        let repository = Arc::new(perps_database::PostgresRepository::new(pool));

        // Create OrderbookManager
        let orderbook_manager = Arc::new(OrderbookManager::new(
            "binance".to_string(),
            Some(repository),
            OrderbookManagerConfig {
                staleness_threshold: std::time::Duration::from_secs(DEFAULT_STALENESS),
                database_write_interval: std::time::Duration::from_secs(1),
            },
        ));

        Ok(Self {
            api_key: None,
            secret_key: None,
            base_url: "https://fapi.binance.com".to_string(),
            client: reqwest::Client::new(),
            symbols_cache: SymbolsCache::new(),
            rate_limiter: Arc::new(RateLimiter::binance()),
            orderbook_manager: Some(orderbook_manager),
        })
    }

    /// Fallback when streaming feature is disabled
    #[cfg(not(feature = "streaming"))]
    async fn try_init_streaming() -> anyhow::Result<Self> {
        Err(anyhow::anyhow!("Streaming feature not enabled"))
    }

    /// Create a new Binance client with API credentials (REST-only, no streaming)
    pub fn with_credentials(api_key: String, secret_key: String) -> Self {
        Self {
            api_key: Some(api_key),
            secret_key: Some(secret_key),
            base_url: "https://fapi.binance.com".to_string(),
            client: reqwest::Client::new(),
            symbols_cache: SymbolsCache::new(),
            rate_limiter: Arc::new(RateLimiter::binance()),
            orderbook_manager: None,
        }
    }

    /// Create a new Binance client with custom rate limiter (REST-only, no streaming)
    pub fn with_rate_limiter(rate_limiter: Arc<RateLimiter>) -> Self {
        Self {
            api_key: None,
            secret_key: None,
            base_url: "https://fapi.binance.com".to_string(),
            client: reqwest::Client::new(),
            symbols_cache: SymbolsCache::new(),
            rate_limiter,
            orderbook_manager: None,
        }
    }

    /// Create a new Binance client with custom base URL (REST-only, no streaming, useful for testnet)
    pub fn with_base_url(base_url: String) -> Self {
        Self {
            api_key: None,
            secret_key: None,
            base_url,
            client: reqwest::Client::new(),
            symbols_cache: SymbolsCache::new(),
            rate_limiter: Arc::new(RateLimiter::binance()),
            orderbook_manager: None,
        }
    }

    /// Ensure the symbols cache is initialized
    async fn ensure_cache_initialized(&self) -> anyhow::Result<()> {
        self.symbols_cache
            .get_or_init(|| async {
                let markets = self.get_markets().await?;
                Ok(markets.into_iter().map(|m| m.symbol).collect())
            })
            .await
    }

    /// Helper to make GET requests to Binance API with rate limiting and retry
    async fn get<T: serde::de::DeserializeOwned>(
        &self,
        endpoint: &str,
    ) -> anyhow::Result<T> {
        let config = RetryConfig::default();
        let url = format!("{}{}", self.base_url, endpoint);
        let client = self.client.clone();
        let rate_limiter = self.rate_limiter.clone();

        execute_with_retry(&config, || {
            let url = url.clone();
            let client = client.clone();
            let rate_limiter = rate_limiter.clone();
            async move {
                rate_limiter.execute(|| {
                    let url = url.clone();
                    let client = client.clone();
                    async move {
                        debug!("Requesting: {}", url);

                        let response = client.get(&url).send().await?;

                        if !response.status().is_success() {
                            let status = response.status();
                            let text = response.text().await?;
                            return Err(BinanceError::ApiError(format!(
                                "HTTP {}: {}",
                                status, text
                            ))
                            .into());
                        }

                        let data = response.json::<T>().await?;
                        Ok(data)
                    }
                }).await
            }
        }).await
    }

    /// Convert Binance exchange info to our Market type
    fn convert_exchange_info_to_market(&self, info: serde_json::Value) -> anyhow::Result<Market> {
        let symbol = info["symbol"]
            .as_str()
            .ok_or_else(|| BinanceError::ConversionError("Missing symbol".to_string()))?;

        let contract_type = info["contractType"]
            .as_str()
            .unwrap_or("PERPETUAL");

        let price_precision = info["pricePrecision"]
            .as_i64()
            .unwrap_or(2) as i32;

        let quantity_precision = info["quantityPrecision"]
            .as_i64()
            .unwrap_or(3) as i32;

        // Extract filter information
        let mut min_qty = Decimal::ZERO;
        let mut max_qty = Decimal::MAX;
        let mut min_notional = Decimal::ZERO;
        let max_leverage = Decimal::new(125, 0); // Default max leverage

        if let Some(filters) = info["filters"].as_array() {
            for filter in filters {
                match filter["filterType"].as_str() {
                    Some("LOT_SIZE") => {
                        if let Some(min) = filter["minQty"].as_str() {
                            min_qty = str_to_decimal(min)?;
                        }
                        if let Some(max) = filter["maxQty"].as_str() {
                            max_qty = str_to_decimal(max)?;
                        }
                    }
                    Some("MIN_NOTIONAL") => {
                        if let Some(notional) = filter["notional"].as_str() {
                            min_notional = str_to_decimal(notional)?;
                        }
                    }
                    _ => {}
                }
            }
        }

        Ok(Market {
            symbol: normalize_symbol(symbol),
            contract: contract_type.to_string(),
            contract_size: Decimal::ONE, // Binance uses 1:1 contract size for USDT futures
            price_scale: price_precision,
            quantity_scale: quantity_precision,
            min_order_qty: min_qty,
            max_order_qty: max_qty,
            min_order_value: min_notional,
            max_leverage,
        })
    }

    /// Convert Binance ticker to our Ticker type
    /// Combines data from 24hr ticker, premium index, and book ticker endpoints
    fn convert_ticker(
        &self,
        ticker: serde_json::Value,
        premium: serde_json::Value,
        book_ticker: serde_json::Value,
        open_interest: Decimal,
    ) -> anyhow::Result<Ticker> {
        let symbol = ticker["symbol"]
            .as_str()
            .ok_or_else(|| BinanceError::ConversionError("Missing symbol".to_string()))?;

        let last_price = str_to_decimal(ticker["lastPrice"].as_str().unwrap_or("0"))?;
        let volume = str_to_decimal(ticker["volume"].as_str().unwrap_or("0"))?;
        let quote_volume = str_to_decimal(ticker["quoteVolume"].as_str().unwrap_or("0"))?;
        let price_change = str_to_decimal(ticker["priceChange"].as_str().unwrap_or("0"))?;
        let price_change_pct = str_to_decimal(ticker["priceChangePercent"].as_str().unwrap_or("0"))?;
        let high_price = str_to_decimal(ticker["highPrice"].as_str().unwrap_or("0"))?;
        let low_price = str_to_decimal(ticker["lowPrice"].as_str().unwrap_or("0"))?;

        // Get mark and index prices from premium index endpoint
        let mark_price = str_to_decimal(premium["markPrice"].as_str().unwrap_or("0"))?;
        let index_price = str_to_decimal(premium["indexPrice"].as_str().unwrap_or("0"))?;

        // Get best bid/ask from book ticker endpoint
        let best_bid = str_to_decimal(book_ticker["bidPrice"].as_str().unwrap_or("0"))?;
        let best_bid_qty = str_to_decimal(book_ticker["bidQty"].as_str().unwrap_or("0"))?;
        let best_ask = str_to_decimal(book_ticker["askPrice"].as_str().unwrap_or("0"))?;
        let best_ask_qty = str_to_decimal(book_ticker["askQty"].as_str().unwrap_or("0"))?;

        let timestamp = Utc::now().timestamp_millis();

        Ok(Ticker {
            symbol: normalize_symbol(symbol),
            last_price,
            mark_price,
            index_price,
            best_bid_price: best_bid,
            best_bid_qty,
            best_ask_price: best_ask,
            best_ask_qty,
            volume_24h: volume,
            turnover_24h: quote_volume,
            open_interest,
            open_interest_notional: open_interest * mark_price,
            price_change_24h: price_change,
            price_change_pct: price_change_pct / Decimal::new(100, 0), // Convert percentage to decimal
            high_price_24h: high_price,
            low_price_24h: low_price,
            timestamp: timestamp_to_datetime(timestamp)?,
        })
    }

    /// Convert Binance orderbook to our Orderbook type
    fn convert_orderbook(
        &self,
        symbol: &str,
        data: serde_json::Value,
    ) -> anyhow::Result<Orderbook> {
        let mut bids = Vec::new();
        if let Some(bid_array) = data["bids"].as_array() {
            for bid in bid_array {
                if let Some(price_qty) = bid.as_array() {
                    if price_qty.len() >= 2 {
                        let price = str_to_decimal(price_qty[0].as_str().unwrap_or("0"))?;
                        let quantity = str_to_decimal(price_qty[1].as_str().unwrap_or("0"))?;
                        bids.push(OrderbookLevel { price, quantity });
                    }
                }
            }
        }

        let mut asks = Vec::new();
        if let Some(ask_array) = data["asks"].as_array() {
            for ask in ask_array {
                if let Some(price_qty) = ask.as_array() {
                    if price_qty.len() >= 2 {
                        let price = str_to_decimal(price_qty[0].as_str().unwrap_or("0"))?;
                        let quantity = str_to_decimal(price_qty[1].as_str().unwrap_or("0"))?;
                        asks.push(OrderbookLevel { price, quantity });
                    }
                }
            }
        }

        let timestamp = data["T"]
            .as_i64()
            .or_else(|| data["E"].as_i64())
            .unwrap_or_else(|| Utc::now().timestamp_millis());

        Ok(Orderbook {
            symbol: normalize_symbol(symbol),
            bids,
            asks,
            timestamp: timestamp_to_datetime(timestamp)?,
        })
    }

    /// Get orderbook snapshot with lastUpdateId for OrderbookManager initialization
    /// Returns (Orderbook, lastUpdateId)
    async fn get_orderbook_snapshot_with_update_id(
        &self,
        symbol: &str,
        depth: u32,
    ) -> anyhow::Result<(Orderbook, u64)> {
        let binance_symbol = denormalize_symbol(symbol);
        let endpoint = format!("/fapi/v1/depth?symbol={}&limit={}", binance_symbol, depth);
        let data: serde_json::Value = self.get(&endpoint).await?;

        // Extract lastUpdateId
        let last_update_id = data["lastUpdateId"]
            .as_u64()
            .ok_or_else(|| BinanceError::ConversionError("Missing lastUpdateId".to_string()))?;

        let orderbook = self.convert_orderbook(symbol, data)?;

        Ok((orderbook, last_update_id))
    }

    /// Start background WebSocket task with buffering (following Binance's specification)
    /// 1. Connects to WebSocket and buffers events
    /// 2. Waits for orderbook to be initialized via initialize_orderbook()
    /// 3. Processes buffered events following Binance's rules
    /// 4. Continues with real-time processing
    #[cfg(feature = "streaming")]
    async fn start_orderbook_stream_with_buffering(
        &self,
        binance_symbol: String,
        manager: Arc<perps_core::OrderbookManager>,
    ) -> anyhow::Result<()> {
        use super::ws_client::BinanceWsClient;
        use super::ws_types::BinanceWsDepthUpdate;
        use futures::{SinkExt, StreamExt};
        use tokio_tungstenite::{connect_async, tungstenite::Message};
        use url::Url;
        use std::collections::VecDeque;

        // Build WebSocket URL for depth@100ms stream
        let stream_url = format!(
            "wss://fstream.binance.com/ws/{}@depth@100ms",
            binance_symbol.to_lowercase()
        );
        let url = Url::parse(&stream_url)?;

        tracing::info!("Starting WebSocket orderbook stream with buffering for {}", binance_symbol);

        // Spawn background task
        tokio::spawn(async move {
            let symbol_for_log = binance_symbol.clone();
            let ws_client = BinanceWsClient::new();

            // Connect to WebSocket
            let mut ws_stream = match connect_async(url.as_str()).await {
                Ok((stream, _)) => {
                    tracing::info!("Connected to orderbook stream for {}", symbol_for_log);
                    stream
                }
                Err(e) => {
                    tracing::error!("Failed to connect to orderbook stream for {}: {}", symbol_for_log, e);
                    return;
                }
            };

            // Buffer for events received before orderbook initialization
            let mut buffered_events: VecDeque<(u64, u64, Vec<perps_core::OrderbookLevel>, Vec<perps_core::OrderbookLevel>)> = VecDeque::new();
            let mut orderbook_initialized = false;

            // Process messages
            while let Some(msg_result) = ws_stream.next().await {
                match msg_result {
                    Ok(Message::Text(text)) => {
                        // Parse depth update
                        match serde_json::from_str::<BinanceWsDepthUpdate>(&text) {
                            Ok(ws_depth) => {
                                // Convert to structured data
                                match ws_client.convert_depth_update(&ws_depth) {
                                    Ok((_symbol, first_update_id, final_update_id, bids, asks)) => {
                                        // Check if orderbook has been initialized
                                        if !orderbook_initialized {
                                            // Check if initialization happened
                                            if manager.get_orderbook(&binance_symbol, 1).await.is_some() {
                                                orderbook_initialized = true;
                                                tracing::info!(
                                                    "Orderbook initialized for {}, processing {} buffered events",
                                                    binance_symbol,
                                                    buffered_events.len()
                                                );

                                                // Process buffered events following Binance's rules
                                                for (buf_first, buf_final, buf_bids, buf_asks) in buffered_events.drain(..) {
                                                    if let Err(e) = manager
                                                        .apply_update(&binance_symbol, buf_first, buf_final, buf_bids, buf_asks)
                                                        .await
                                                    {
                                                        tracing::debug!(
                                                            "Skipped buffered update for {}: {}",
                                                            binance_symbol,
                                                            e
                                                        );
                                                    }
                                                }
                                            } else {
                                                // Still buffering - add to buffer
                                                buffered_events.push_back((first_update_id, final_update_id, bids, asks));

                                                // Limit buffer size to prevent memory issues
                                                if buffered_events.len() > 1000 {
                                                    buffered_events.pop_front();
                                                    tracing::warn!("Buffer full for {}, dropping oldest event", binance_symbol);
                                                }
                                                continue;
                                            }
                                        }

                                        // Apply delta update to OrderbookManager (real-time or from buffer)
                                        if let Err(e) = manager
                                            .apply_update(&binance_symbol, first_update_id, final_update_id, bids, asks)
                                            .await
                                        {
                                            tracing::debug!(
                                                "Failed to apply orderbook update for {}: {}",
                                                binance_symbol,
                                                e
                                            );
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!("Failed to convert depth update for {}: {}", symbol_for_log, e);
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::debug!("Failed to parse depth update for {}: {}", symbol_for_log, e);
                            }
                        }
                    }
                    Ok(Message::Ping(payload)) => {
                        // Respond to ping with pong
                        if let Err(e) = ws_stream.send(Message::Pong(payload)).await {
                            tracing::error!("Failed to send pong for {}: {}", symbol_for_log, e);
                            break;
                        }
                    }
                    Ok(Message::Close(frame)) => {
                        tracing::info!("WebSocket closed for {}: {:?}", symbol_for_log, frame);
                        break;
                    }
                    Err(e) => {
                        tracing::error!("WebSocket error for {}: {}", symbol_for_log, e);
                        break;
                    }
                    _ => {}
                }
            }

            tracing::info!("Orderbook stream ended for {}", symbol_for_log);
        });

        Ok(())
    }

    /// Fallback when streaming feature is disabled
    #[cfg(not(feature = "streaming"))]
    async fn start_orderbook_stream_with_buffering(
        &self,
        _binance_symbol: String,
        _manager: Arc<perps_core::OrderbookManager>,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    /// Convert Binance funding rate from premium index endpoint to our FundingRate type
    /// Used by get_funding_rate() which calls /fapi/v1/premiumIndex
    fn convert_funding_rate(&self, data: serde_json::Value) -> anyhow::Result<FundingRate> {
        let symbol = data["symbol"]
            .as_str()
            .ok_or_else(|| BinanceError::ConversionError("Missing symbol".to_string()))?;

        let funding_rate = str_to_decimal(data["lastFundingRate"].as_str().unwrap_or("0"))?;
        let _mark_price = str_to_decimal(data["markPrice"].as_str().unwrap_or("0"))?;

        let funding_time = data["time"]
            .as_i64()
            .unwrap_or_else(|| Utc::now().timestamp_millis());

        let next_funding_time = data["nextFundingTime"]
            .as_i64()
            .unwrap_or(funding_time + 8 * 3600 * 1000); // Default: 8 hours later

        Ok(FundingRate {
            symbol: normalize_symbol(symbol),
            funding_rate,
            predicted_rate: funding_rate, // Binance doesn't provide predicted rate separately
            funding_time: timestamp_to_datetime(funding_time)?,
            next_funding_time: timestamp_to_datetime(next_funding_time)?,
            funding_interval: 8, // Binance uses 8-hour funding intervals
            funding_rate_cap_floor: Decimal::new(75, 4), // ±0.75%
        })
    }

    /// Convert Binance funding rate from funding rate history endpoint to our FundingRate type
    /// Used by get_funding_rate_history() which calls /fapi/v1/fundingRate
    /// This endpoint returns different field names: fundingRate instead of lastFundingRate, fundingTime instead of time
    fn convert_funding_rate_history(&self, data: serde_json::Value) -> anyhow::Result<FundingRate> {
        let symbol = data["symbol"]
            .as_str()
            .ok_or_else(|| BinanceError::ConversionError("Missing symbol".to_string()))?;

        // Note: fundingRate field (not lastFundingRate)
        let funding_rate = str_to_decimal(data["fundingRate"].as_str().unwrap_or("0"))?;
        let _mark_price = str_to_decimal(data["markPrice"].as_str().unwrap_or("0"))?;

        // Note: fundingTime field (not time)
        let funding_time = data["fundingTime"]
            .as_i64()
            .unwrap_or_else(|| Utc::now().timestamp_millis());

        // Historical data doesn't include nextFundingTime, calculate it
        let next_funding_time = funding_time + 8 * 3600 * 1000; // 8 hours later

        Ok(FundingRate {
            symbol: normalize_symbol(symbol),
            funding_rate,
            predicted_rate: funding_rate, // Binance doesn't provide predicted rate separately
            funding_time: timestamp_to_datetime(funding_time)?,
            next_funding_time: timestamp_to_datetime(next_funding_time)?,
            funding_interval: 8, // Binance uses 8-hour funding intervals
            funding_rate_cap_floor: Decimal::new(75, 4), // ±0.75%
        })
    }

    /// Convert Binance kline to our Kline type
    fn convert_kline(&self, symbol: &str, interval: &str, data: &[serde_json::Value]) -> anyhow::Result<Kline> {
        if data.len() < 11 {
            return Err(BinanceError::ConversionError("Invalid kline data".to_string()).into());
        }

        let open_time = data[0].as_i64().unwrap_or(0);
        let open = str_to_decimal(data[1].as_str().unwrap_or("0"))?;
        let high = str_to_decimal(data[2].as_str().unwrap_or("0"))?;
        let low = str_to_decimal(data[3].as_str().unwrap_or("0"))?;
        let close = str_to_decimal(data[4].as_str().unwrap_or("0"))?;
        let volume = str_to_decimal(data[5].as_str().unwrap_or("0"))?;
        let close_time = data[6].as_i64().unwrap_or(0);
        let quote_volume = str_to_decimal(data[7].as_str().unwrap_or("0"))?;

        Ok(Kline {
            symbol: normalize_symbol(symbol),
            interval: interval.to_string(),
            open_time: timestamp_to_datetime(open_time)?,
            close_time: timestamp_to_datetime(close_time)?,
            open,
            high,
            low,
            close,
            volume,
            turnover: quote_volume,
        })
    }

    /// Convert Binance trade to our Trade type
    fn convert_trade(&self, data: serde_json::Value) -> anyhow::Result<Trade> {
        let symbol = data["symbol"]
            .as_str()
            .ok_or_else(|| BinanceError::ConversionError("Missing symbol".to_string()))?;

        let id = data["id"]
            .as_i64()
            .map(|i| i.to_string())
            .or_else(|| data["a"].as_i64().map(|i| i.to_string()))
            .unwrap_or_else(|| "0".to_string());

        let price = str_to_decimal(data["price"].as_str().or(data["p"].as_str()).unwrap_or("0"))?;
        let qty = str_to_decimal(data["qty"].as_str().or(data["q"].as_str()).unwrap_or("0"))?;

        let is_buyer_maker = data["isBuyerMaker"]
            .as_bool()
            .or(data["m"].as_bool())
            .unwrap_or(false);

        let side = if is_buyer_maker {
            OrderSide::Sell
        } else {
            OrderSide::Buy
        };

        let time = data["time"]
            .as_i64()
            .or(data["T"].as_i64())
            .unwrap_or_else(|| Utc::now().timestamp_millis());

        Ok(Trade {
            id,
            symbol: normalize_symbol(symbol),
            price,
            quantity: qty,
            side,
            timestamp: timestamp_to_datetime(time)?,
        })
    }

    /// Get ticker with custom timeframe support
    /// For "24h" or "1d", uses Binance's native 24hr ticker endpoint (more efficient)
    /// For other timeframes (5m, 15m, 30m, 1h, 4h), calculates statistics from klines
    pub async fn get_ticker_with_timeframe(
        &self,
        symbol: &str,
        timeframe: &str,
    ) -> anyhow::Result<Ticker> {
        let normalized_timeframe = timeframe.to_lowercase();

        // For 24h, use the native endpoint for better efficiency
        if normalized_timeframe == "24h" || normalized_timeframe == "1d" {
            return self.get_ticker(symbol).await;
        }

        // Parse the timeframe
        let (binance_interval, duration) = parse_timeframe(&normalized_timeframe)?;

        // Calculate the time range
        let end_time = Utc::now();
        let start_time = end_time - duration;

        // Fetch klines for the timeframe
        let klines = self.get_klines(
            symbol,
            &binance_interval,
            Some(start_time),
            Some(end_time),
            None,
        ).await?;

        if klines.is_empty() {
            return Err(anyhow::anyhow!("No klines data available for {} in timeframe {}", symbol, timeframe));
        }

        // Get current prices from ticker for accurate current state
        let current_ticker = self.get_ticker(symbol).await?;

        // Calculate ticker statistics from klines
        calculate_ticker_from_klines(
            symbol,
            &klines,
            current_ticker.last_price,
            current_ticker.mark_price,
            current_ticker.index_price,
            current_ticker.best_bid_price,
            current_ticker.best_bid_qty,
            current_ticker.best_ask_price,
            current_ticker.best_ask_qty,
        )
    }
}

// Note: Cannot implement Default trait because new() is async

#[async_trait]
impl IPerps for BinanceClient {
    fn get_name(&self) -> &str {
        "Binance"
    }

    fn parse_symbol(&self, symbol: &str) -> String {
        let upper_symbol = symbol.to_uppercase();
        if upper_symbol.contains('-') {
            // Assumes format "BASE-QUOTE"
            upper_symbol
        } else if upper_symbol.ends_with("USDT") {
            // Assumes format "BASEQUOTE" like "BTCUSDT"
            normalize_symbol(&upper_symbol)
        } else {
            // Assumes format "BASE" like "BTC"
            format!("{}-USDT", upper_symbol)
        }
    }

    async fn get_markets(&self) -> anyhow::Result<Vec<Market>> {
        let data: serde_json::Value = self.get("/fapi/v1/exchangeInfo").await?;

        let symbols = data["symbols"]
            .as_array()
            .ok_or(BinanceError::InvalidResponse)?;

        let mut markets = Vec::new();
        for symbol_info in symbols {
            // Only include perpetual futures
            if symbol_info["contractType"].as_str() == Some("PERPETUAL")
                && symbol_info["status"].as_str() == Some("TRADING")
            {
                match self.convert_exchange_info_to_market(symbol_info.clone()) {
                    Ok(market) => markets.push(market),
                    Err(e) => warn!("Failed to convert market: {}", e),
                }
            }
        }

        Ok(markets)
    }

    async fn get_market(&self, symbol: &str) -> anyhow::Result<Market> {
        let binance_symbol = denormalize_symbol(symbol);
        let data: serde_json::Value = self.get("/fapi/v1/exchangeInfo").await?;

        let symbols = data["symbols"]
            .as_array()
            .ok_or(BinanceError::InvalidResponse)?;

        for symbol_info in symbols {
            if symbol_info["symbol"].as_str() == Some(&binance_symbol) {
                return self.convert_exchange_info_to_market(symbol_info.clone());
            }
        }

        Err(BinanceError::SymbolNotSupported(symbol.to_string()).into())
    }

    async fn get_ticker(&self, symbol: &str) -> anyhow::Result<Ticker> {
        let binance_symbol = denormalize_symbol(symbol);

        // Fetch 24hr ticker data
        let ticker_endpoint = format!("/fapi/v1/ticker/24hr?symbol={}", binance_symbol);
        let ticker_data: serde_json::Value = self.get(&ticker_endpoint).await?;

        // Fetch premium index data for mark and index prices
        let premium_endpoint = format!("/fapi/v1/premiumIndex?symbol={}", binance_symbol);
        let premium_data: serde_json::Value = self.get(&premium_endpoint).await?;

        // Fetch book ticker data for best bid/ask
        let book_ticker_endpoint = format!("/fapi/v1/ticker/bookTicker?symbol={}", binance_symbol);
        let book_ticker_data: serde_json::Value = self.get(&book_ticker_endpoint).await?;

        // Fetch open interest
        let oi = self.get_open_interest(symbol).await?;

        self.convert_ticker(ticker_data, premium_data, book_ticker_data, oi.open_interest)
    }

    async fn get_all_tickers(&self) -> anyhow::Result<Vec<Ticker>> {
        // Fetch 24hr ticker data, premium index data, book ticker data, and open interest data
        let ticker_data: serde_json::Value = self.get("/fapi/v1/ticker/24hr").await?;
        let premium_data: serde_json::Value = self.get("/fapi/v1/premiumIndex").await?;
        let book_ticker_data: serde_json::Value = self.get("/fapi/v1/ticker/bookTicker").await?;
        let open_interest_data: serde_json::Value = self.get("/fapi/v1/openInterest").await?;

        let tickers_array = ticker_data
            .as_array()
            .ok_or(BinanceError::InvalidResponse)?;
        let premium_array = premium_data
            .as_array()
            .ok_or(BinanceError::InvalidResponse)?;
        let book_ticker_array = book_ticker_data
            .as_array()
            .ok_or(BinanceError::InvalidResponse)?;
        let open_interest_array = open_interest_data
            .as_array()
            .ok_or(BinanceError::InvalidResponse)?;

        // Create maps of symbol -> data for fast lookup
        let mut premium_map = std::collections::HashMap::new();
        for premium_item in premium_array {
            if let Some(symbol) = premium_item["symbol"].as_str() {
                premium_map.insert(symbol.to_string(), premium_item.clone());
            }
        }

        let mut book_ticker_map = std::collections::HashMap::new();
        for book_item in book_ticker_array {
            if let Some(symbol) = book_item["symbol"].as_str() {
                book_ticker_map.insert(symbol.to_string(), book_item.clone());
            }
        }

        let mut open_interest_map = std::collections::HashMap::new();
        for oi_item in open_interest_array {
            if let Some(symbol) = oi_item["symbol"].as_str() {
                if let Ok(oi) = str_to_decimal(oi_item["openInterest"].as_str().unwrap_or("0")) {
                    open_interest_map.insert(symbol.to_string(), oi);
                }
            }
        }

        let mut tickers = Vec::new();
        for ticker_item in tickers_array {
            if let Some(symbol) = ticker_item["symbol"].as_str() {
                if let (Some(premium_item), Some(book_item)) =
                    (premium_map.get(symbol), book_ticker_map.get(symbol)) {
                    // Get open interest or use zero if not available
                    let oi = open_interest_map.get(symbol).copied().unwrap_or(Decimal::ZERO);
                    match self.convert_ticker(ticker_item.clone(), premium_item.clone(), book_item.clone(), oi) {
                        Ok(ticker) => tickers.push(ticker),
                        Err(e) => warn!("Failed to convert ticker for {}: {}", symbol, e),
                    }
                }
            }
        }

        Ok(tickers)
    }

    async fn get_orderbook(&self, symbol: &str, depth: u32) -> anyhow::Result<Orderbook> {
        // Check if OrderbookManager is available
        if let Some(ref manager) = self.orderbook_manager {
            let binance_symbol = denormalize_symbol(symbol);

            // Try to get from OrderbookManager cache
            if let Some(cached_orderbook) = manager.get_orderbook(&binance_symbol, depth as usize).await {
                // Check if cached data is fresh
                if manager.is_fresh(&binance_symbol).await {
                    tracing::debug!(
                        "Returning cached orderbook for {} (age: {:?})",
                        binance_symbol,
                        Utc::now().signed_duration_since(cached_orderbook.timestamp).to_string()
                    );
                    return Ok(cached_orderbook);
                }
            }

            // If not cached or stale, initialize following Binance's specification:
            // 1. Start WebSocket and buffer events
            // 2. Get REST snapshot
            // 3. Initialize local orderbook
            // 4. WebSocket task will process buffered events

            tracing::debug!("Initializing orderbook for {} following Binance spec", binance_symbol);

            // Step 1: Start WebSocket FIRST (will buffer events)
            self.start_orderbook_stream_with_buffering(binance_symbol.clone(), manager.clone())
                .await?;

            // Small delay to ensure WebSocket is connected and buffering
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

            // Step 2: Get snapshot from REST API with lastUpdateId
            tracing::debug!("Fetching REST API snapshot for {}", binance_symbol);
            let (orderbook, last_update_id) = self
                .get_orderbook_snapshot_with_update_id(symbol, 1000)
                .await?;

            // Step 3: Initialize local orderbook in OrderbookManager
            // This will cause the WebSocket task to start processing buffered events
            manager
                .initialize_orderbook(
                    binance_symbol.clone(),
                    orderbook.bids.clone(),
                    orderbook.asks.clone(),
                    last_update_id,
                )
                .await;

            tracing::info!(
                "Initialized local orderbook for {} with lastUpdateId={}",
                binance_symbol,
                last_update_id
            );

            // Return the snapshot (limited to requested depth)
            return Ok(Orderbook {
                symbol: orderbook.symbol.clone(),
                bids: orderbook.bids.iter().take(depth as usize).cloned().collect(),
                asks: orderbook.asks.iter().take(depth as usize).cloned().collect(),
                timestamp: orderbook.timestamp,
            });
        }

        // Fallback when OrderbookManager is not available: direct REST API call
        let binance_symbol = denormalize_symbol(symbol);
        let endpoint = format!("/fapi/v1/depth?symbol={}&limit={}", binance_symbol, depth);
        let data: serde_json::Value = self.get(&endpoint).await?;
        self.convert_orderbook(symbol, data)
    }

    async fn get_funding_rate(&self, symbol: &str) -> anyhow::Result<FundingRate> {
        let binance_symbol = denormalize_symbol(symbol);
        let endpoint = format!("/fapi/v1/premiumIndex?symbol={}", binance_symbol);
        let data: serde_json::Value = self.get(&endpoint).await?;
        self.convert_funding_rate(data)
    }

    async fn get_funding_rate_history(
        &self,
        symbol: &str,
        start_time: Option<DateTime<Utc>>,
        end_time: Option<DateTime<Utc>>,
        limit: Option<u32>,
    ) -> anyhow::Result<Vec<FundingRate>> {
        let binance_symbol = denormalize_symbol(symbol);
        let mut endpoint = format!("/fapi/v1/fundingRate?symbol={}", binance_symbol);

        if let Some(start) = start_time {
            endpoint.push_str(&format!("&startTime={}", start.timestamp_millis()));
        }
        if let Some(end) = end_time {
            endpoint.push_str(&format!("&endTime={}", end.timestamp_millis()));
        }
        if let Some(lim) = limit {
            endpoint.push_str(&format!("&limit={}", lim));
        }

        let data: serde_json::Value = self.get(&endpoint).await?;
        let rates_array = data
            .as_array()
            .ok_or(BinanceError::InvalidResponse)?;

        let mut rates = Vec::new();
        for rate_data in rates_array {
            // Use convert_funding_rate_history for the /fapi/v1/fundingRate endpoint
            match self.convert_funding_rate_history(rate_data.clone()) {
                Ok(rate) => rates.push(rate),
                Err(e) => warn!("Failed to convert funding rate: {}", e),
            }
        }

        Ok(rates)
    }

    async fn get_open_interest(&self, symbol: &str) -> anyhow::Result<OpenInterest> {
        let binance_symbol = denormalize_symbol(symbol);
        let endpoint = format!("/fapi/v1/openInterest?symbol={}", binance_symbol);
        let data: serde_json::Value = self.get(&endpoint).await?;

        let oi = str_to_decimal(data["openInterest"].as_str().unwrap_or("0"))?;
        let timestamp = data["time"]
            .as_i64()
            .unwrap_or_else(|| Utc::now().timestamp_millis());

        // Get mark price to calculate open value
        let mark_price_endpoint = format!("/fapi/v1/premiumIndex?symbol={}", binance_symbol);
        let mark_data: serde_json::Value = self.get(&mark_price_endpoint).await?;
        let mark_price = str_to_decimal(mark_data["markPrice"].as_str().unwrap_or("0"))?;

        Ok(OpenInterest {
            symbol: normalize_symbol(&binance_symbol),
            open_interest: oi,
            open_value: oi * mark_price,
            timestamp: timestamp_to_datetime(timestamp)?,
        })
    }

    async fn get_klines(
        &self,
        symbol: &str,
        interval: &str,
        start_time: Option<DateTime<Utc>>,
        end_time: Option<DateTime<Utc>>,
        limit: Option<u32>,
    ) -> anyhow::Result<Vec<Kline>> {
        let binance_symbol = denormalize_symbol(symbol);
        let mut endpoint = format!(
            "/fapi/v1/klines?symbol={}&interval={}",
            binance_symbol, interval
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

        let data: serde_json::Value = self.get(&endpoint).await?;
        let klines_array = data
            .as_array()
            .ok_or(BinanceError::InvalidResponse)?;

        let mut klines = Vec::new();
        for kline_data in klines_array {
            if let Some(kline_array) = kline_data.as_array() {
                match self.convert_kline(symbol, interval, kline_array) {
                    Ok(kline) => klines.push(kline),
                    Err(e) => warn!("Failed to convert kline: {}", e),
                }
            }
        }

        Ok(klines)
    }

    async fn get_recent_trades(&self, symbol: &str, limit: u32) -> anyhow::Result<Vec<Trade>> {
        let binance_symbol = denormalize_symbol(symbol);
        let endpoint = format!("/fapi/v1/trades?symbol={}&limit={}", binance_symbol, limit);
        let data: serde_json::Value = self.get(&endpoint).await?;

        let trades_array = data
            .as_array()
            .ok_or(BinanceError::InvalidResponse)?;

        let mut trades = Vec::new();
        for trade_data in trades_array {
            match self.convert_trade(trade_data.clone()) {
                Ok(trade) => trades.push(trade),
                Err(e) => warn!("Failed to convert trade: {}", e),
            }
        }

        Ok(trades)
    }

    async fn get_market_stats(&self, symbol: &str) -> anyhow::Result<MarketStats> {
        // Get ticker for price data
        let ticker = self.get_ticker(symbol).await?;

        // Get funding rate
        let funding = self.get_funding_rate(symbol).await?;

        // Get open interest
        let oi = self.get_open_interest(symbol).await?;

        Ok(MarketStats {
            symbol: ticker.symbol.clone(),
            volume_24h: ticker.volume_24h,
            turnover_24h: ticker.turnover_24h,
            open_interest: oi.open_interest,
            funding_rate: funding.funding_rate,
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

    async fn get_all_market_stats(&self) -> anyhow::Result<Vec<MarketStats>> {
        // This is less efficient but works with public API
        let tickers = self.get_all_tickers().await?;

        let mut stats = Vec::new();
        for ticker in tickers {
            // For efficiency, we'll skip getting OI and funding for each symbol
            // in the all_market_stats call. Users can call get_market_stats for detailed info
            stats.push(MarketStats {
                symbol: ticker.symbol.clone(),
                volume_24h: ticker.volume_24h,
                turnover_24h: ticker.turnover_24h,
                open_interest: Decimal::ZERO, // Would need separate call per symbol
                funding_rate: Decimal::ZERO,   // Would need separate call per symbol
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

        Ok(stats)
    }

    async fn is_supported(&self, symbol: &str) -> anyhow::Result<bool> {
        self.ensure_cache_initialized().await?;
        Ok(self.symbols_cache.contains(symbol).await)
    }
}
