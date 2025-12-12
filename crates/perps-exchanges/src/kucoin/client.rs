use crate::cache::ContractCache;
use crate::kucoin::types::*;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use perps_core::types::*;
use perps_core::{execute_with_retry, IPerps, RateLimiter, RetryConfig};
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

const BASE_URL: &str = "https://api-futures.kucoin.com";

/// A client for the KuCoin Futures exchange.
#[derive(Clone)]
pub struct KucoinClient {
    http: reqwest::Client,
    /// Cached contract information (includes lotSize, tickSize, multiplier, fees, margins)
    contract_cache: ContractCache<KucoinContract>,
    /// Rate limiter for API calls
    rate_limiter: Arc<RateLimiter>,
    /// Optional stream manager for WebSocket-based orderbook streaming
    #[cfg(feature = "streaming")]
    stream_manager: Option<Arc<perps_core::StreamManager>>,
}

impl KucoinClient {
    /// Create a new KuCoin client
    ///
    /// Automatically enables WebSocket streaming if:
    /// - DATABASE_URL environment variable is set
    /// - `streaming` feature is enabled
    ///
    /// Falls back to REST-only mode if streaming initialization fails.
    pub async fn new() -> anyhow::Result<Self> {
        match Self::try_init_streaming().await {
            Ok(client) => {
                tracing::info!("✓ KucoinClient initialized with WebSocket streaming");
                Ok(client)
            }
            Err(e) => {
                tracing::warn!("Failed to initialize streaming for KucoinClient: {}", e);
                tracing::warn!("Falling back to REST-only mode");
                Ok(Self::new_rest_only())
            }
        }
    }

    /// Create a REST-only client (no streaming)
    pub fn new_rest_only() -> Self {
        Self {
            http: reqwest::Client::new(),
            contract_cache: ContractCache::new(),
            rate_limiter: Arc::new(RateLimiter::kucoin()),
            #[cfg(feature = "streaming")]
            stream_manager: None,
        }
    }

    /// Try to initialize with streaming support using StreamManager
    #[cfg(feature = "streaming")]
    async fn try_init_streaming() -> anyhow::Result<Self> {
        use super::ws_client::KuCoinWsClient;
        use perps_core::{StreamConfig, StreamManager};

        tracing::info!("Initializing KucoinClient with StreamManager");

        // Create contract cache and initialize it immediately
        let contract_cache = ContractCache::new();

        // Pre-fetch contracts to populate cache before WebSocket starts
        let http = reqwest::Client::new();
        let rate_limiter = Arc::new(RateLimiter::kucoin());

        // Fetch contracts using a temporary client-like structure
        let url = format!("{}/api/v1/contracts/active", BASE_URL);
        let response = rate_limiter
            .execute(|| {
                let url = url.clone();
                let http = http.clone();
                async move {
                    let response = http.get(&url).send().await?;
                    let wrapper: KucoinResponse<Vec<KucoinContract>> = response.json().await?;
                    if wrapper.code != "200000" {
                        return Err(anyhow!("KuCoin API error: code {}", wrapper.code));
                    }
                    Ok(wrapper.data)
                }
            })
            .await?;

        // Build contract map
        let contract_map: HashMap<String, KucoinContract> = response
            .into_iter()
            .map(|contract| (contract.symbol.clone(), contract))
            .collect();

        tracing::info!(
            "[KuCoin] Pre-initialized contract cache with {} contracts",
            contract_map.len()
        );
        contract_cache.initialize(contract_map);

        // Create WebSocket client with shared and initialized contract cache
        let ws_client = Arc::new(KuCoinWsClient::new_with_cache(contract_cache.clone()));

        // Create StreamManager with default config
        let stream_manager = Arc::new(StreamManager::new(
            ws_client as Arc<dyn perps_core::OrderbookStreamer>,
            StreamConfig::default(),
        ));

        Ok(Self {
            http,
            contract_cache,
            rate_limiter,
            stream_manager: Some(stream_manager),
        })
    }

    /// Fallback when streaming feature is disabled
    #[cfg(not(feature = "streaming"))]
    async fn try_init_streaming() -> anyhow::Result<Self> {
        Err(anyhow::anyhow!("Streaming feature not enabled"))
    }

    /// Ensure the contract cache is initialized by fetching all contracts from /api/v1/contracts/active
    async fn ensure_cache_initialized(&self) -> Result<()> {
        self.contract_cache
            .get_or_init(|| async {
                tracing::debug!("[KuCoin] Fetching all contracts for cache initialization");
                let contracts: Vec<KucoinContract> = self.get("/api/v1/contracts/active").await?;

                // Build HashMap: symbol -> contract
                let contract_map: HashMap<String, KucoinContract> = contracts
                    .into_iter()
                    .map(|contract| (contract.symbol.clone(), contract))
                    .collect();

                tracing::info!(
                    "[KuCoin] Initialized contract cache with {} contracts",
                    contract_map.len()
                );
                Ok(contract_map)
            })
            .await
    }

    /// Get contract info for a symbol (initializes cache if needed)
    async fn get_contract_info(&self, symbol: &str) -> Result<KucoinContract> {
        self.ensure_cache_initialized().await?;

        self.contract_cache
            .get(symbol)
            .await
            .ok_or_else(|| anyhow!("Contract info not found for symbol: {}", symbol))
    }

    /// Convert lot-based quantity to real quantity
    /// Formula: real_quantity = lot_quantity × lot_size × multiplier
    fn convert_lot_to_real_quantity(&self, lot_qty: i64, contract: &KucoinContract) -> Decimal {
        let lot_qty_decimal = Decimal::from(lot_qty);
        let lot_size_decimal = Decimal::from(contract.lot_size);
        let multiplier_decimal = Decimal::from_f64(contract.multiplier).unwrap_or(Decimal::ONE);

        lot_qty_decimal * lot_size_decimal * multiplier_decimal
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
                            let wrapper: KucoinResponse<T> = response.json().await?;
                            if wrapper.code != "200000" {
                                return Err(anyhow!("KuCoin API error: code {}", wrapper.code));
                            }
                            Ok(wrapper.data)
                        }
                    })
                    .await
            }
        })
        .await
    }

    /// Fetch orderbook snapshot with update ID (sequence number) for StreamManager
    #[cfg(feature = "streaming")]
    async fn get_orderbook_snapshot_with_update_id(
        &self,
        symbol: &str,
        _depth: u32,
    ) -> anyhow::Result<(Orderbook, u64)> {
        let response: KucoinOrderbook = self
            .get(&format!("/api/v1/level2/snapshot?symbol={}", symbol))
            .await?;

        // Get contract info for quantity conversion
        let contract = self.get_contract_info(&response.symbol).await?;

        let bids = response
            .bids
            .iter()
            .map(|(price, lot_qty)| {
                let real_qty = self.convert_lot_to_real_quantity(*lot_qty, &contract);
                Ok(OrderbookLevel {
                    price: Decimal::from_f64(*price).unwrap_or(Decimal::ZERO),
                    quantity: real_qty,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let asks = response
            .asks
            .iter()
            .map(|(price, lot_qty)| {
                let real_qty = self.convert_lot_to_real_quantity(*lot_qty, &contract);
                Ok(OrderbookLevel {
                    price: Decimal::from_f64(*price).unwrap_or(Decimal::ZERO),
                    quantity: real_qty,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let orderbook = Orderbook {
            symbol: response.symbol.clone(),
            bids,
            asks,
            timestamp: Utc.timestamp_nanos(response.timestamp),
        };

        Ok((orderbook, response.sequence))
    }
}

impl Default for KucoinClient {
    fn default() -> Self {
        Self::new_rest_only()
    }
}

#[async_trait]
impl IPerps for KucoinClient {
    fn get_name(&self) -> &str {
        "kucoin"
    }

    fn parse_symbol(&self, symbol: &str) -> String {
        // Convert BTC -> XBTUSDTM, ETH -> ETHUSDTM
        let upper = symbol.to_uppercase();
        if upper.contains("USDTM") {
            return upper;
        }

        if upper == "BTC" {
            "XBTUSDTM".to_string()
        } else {
            format!("{}USDTM", upper)
        }
    }

    async fn get_markets(&self) -> Result<Vec<Market>> {
        let contracts: Vec<KucoinContract> = self.get("/api/v1/contracts/active").await?;
        let markets = contracts
            .into_iter()
            .filter(|c| c.status == "Open")
            .map(|c| {
                let price_scale = if c.tick_size > 0.0 {
                    (-c.tick_size.log10()).ceil() as i32
                } else {
                    2
                };
                let quantity_scale = if c.lot_size > 0 { 0 } else { 3 };

                Market {
                    symbol: c.symbol.clone(),
                    contract: c.symbol,
                    price_scale,
                    quantity_scale,
                    min_order_qty: Decimal::from(c.lot_size),
                    contract_size: Decimal::from_f64(c.multiplier).unwrap_or(Decimal::ONE),
                    max_order_qty: Decimal::from(c.max_order_qty),
                    min_order_value: Decimal::ZERO,
                    max_leverage: Decimal::from(c.max_leverage),
                }
            })
            .collect();
        Ok(markets)
    }

    async fn get_market(&self, symbol: &str) -> Result<Market> {
        let contracts: Vec<KucoinContractDetail> = self.get("/api/v1/contracts/active").await?;
        let contract = contracts
            .into_iter()
            .find(|c| c.symbol == symbol)
            .ok_or_else(|| anyhow!("Market {} not found", symbol))?;

        let price_scale = if contract.tick_size > 0.0 {
            (-contract.tick_size.log10()).ceil() as i32
        } else {
            2
        };
        let quantity_scale = if contract.lot_size > 0 { 0 } else { 3 };

        Ok(Market {
            symbol: contract.symbol.clone(),
            contract: contract.symbol,
            price_scale,
            quantity_scale,
            min_order_qty: Decimal::from(contract.lot_size),
            contract_size: Decimal::from_f64(contract.multiplier).unwrap_or(Decimal::ONE),
            max_order_qty: Decimal::from(contract.max_order_qty),
            min_order_value: Decimal::ZERO,
            max_leverage: Decimal::from(contract.max_leverage),
        })
    }

    async fn get_orderbook(&self, symbol: &str, depth: u32) -> Result<MultiResolutionOrderbook> {
        // Check if StreamManager is available
        #[cfg(feature = "streaming")]
        if let Some(ref manager) = self.stream_manager {
            let kucoin_symbol = self.parse_symbol(symbol);

            // Subscribe (idempotent, auto-starts streaming)
            manager.subscribe(kucoin_symbol.clone()).await?;

            // Get orderbook (auto cache + fallback)
            let client_clone = self.clone();
            let kucoin_symbol_clone = kucoin_symbol.clone();
            let orderbook = manager
                .get_orderbook(&kucoin_symbol, depth, || async move {
                    client_clone
                        .get_orderbook_snapshot_with_update_id(&kucoin_symbol_clone, depth)
                        .await
                })
                .await?;

            return Ok(MultiResolutionOrderbook::from_single(orderbook));
        }

        // Fallback: direct REST API call with quantity conversion
        let response: KucoinOrderbook = self
            .get(&format!("/api/v1/level2/snapshot?symbol={}", symbol))
            .await?;

        // Get contract info for quantity conversion
        let contract = self.get_contract_info(&response.symbol).await?;

        let bids = response
            .bids
            .into_iter()
            .map(|(price, lot_qty)| {
                let real_qty = self.convert_lot_to_real_quantity(lot_qty, &contract);
                Ok(OrderbookLevel {
                    price: Decimal::from_f64(price).unwrap_or(Decimal::ZERO),
                    quantity: real_qty,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let asks = response
            .asks
            .into_iter()
            .map(|(price, lot_qty)| {
                let real_qty = self.convert_lot_to_real_quantity(lot_qty, &contract);
                Ok(OrderbookLevel {
                    price: Decimal::from_f64(price).unwrap_or(Decimal::ZERO),
                    quantity: real_qty,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let orderbook = Orderbook {
            symbol: response.symbol,
            bids,
            asks,
            timestamp: Utc.timestamp_nanos(response.timestamp),
        };

        Ok(MultiResolutionOrderbook::from_single(orderbook))
    }

    async fn get_recent_trades(&self, symbol: &str, _limit: u32) -> Result<Vec<Trade>> {
        let trades: Vec<KucoinTrade> = self
            .get(&format!("/api/v1/trade/history?symbol={}", symbol))
            .await?;

        let trades = trades
            .into_iter()
            .map(|t| {
                Ok(Trade {
                    id: t.trade_id,
                    symbol: symbol.to_string(),
                    price: Decimal::from_str(&t.price)?,
                    quantity: Decimal::from(t.size),
                    side: if t.side == "buy" {
                        OrderSide::Buy
                    } else {
                        OrderSide::Sell
                    },
                    timestamp: Utc.timestamp_nanos(t.timestamp),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(trades)
    }

    async fn get_funding_rate(&self, symbol: &str) -> Result<FundingRate> {
        let rate: KucoinFundingRate = self
            .get(&format!("/api/v1/funding-rate/{}/current", symbol))
            .await?;

        Ok(FundingRate {
            symbol: rate.symbol.clone(),
            funding_rate: Decimal::from_f64(rate.value).unwrap_or(Decimal::ZERO),
            funding_time: Utc.timestamp_millis_opt(rate.time_point).unwrap(),
            predicted_rate: rate
                .predicted_value
                .and_then(Decimal::from_f64)
                .unwrap_or(Decimal::ZERO),
            next_funding_time: Utc
                .timestamp_millis_opt(rate.time_point + rate.granularity)
                .unwrap(),
            funding_interval: (rate.granularity / 3600000) as i32,
            funding_rate_cap_floor: Decimal::ZERO,
        })
    }

    async fn get_klines(
        &self,
        symbol: &str,
        interval: &str,
        start_time: Option<chrono::DateTime<chrono::Utc>>,
        end_time: Option<chrono::DateTime<chrono::Utc>>,
        _limit: Option<u32>,
    ) -> Result<Vec<Kline>> {
        // Convert interval to granularity (minutes)
        // KuCoin accepts: 15, 30, 60, 120, 240, 480, 720, 1440, 10080
        let granularity = match interval {
            "1m" => 1,
            "5m" => 5,
            "15m" => 15,
            "30m" => 30,
            "1h" => 60,
            "2h" => 120,
            "4h" => 240,
            "8h" => 480,
            "12h" => 720,
            "1d" => 1440,
            "1w" => 10080,
            _ => return Err(anyhow!("Unsupported interval '{}' for KuCoin. Supported: 15m, 30m, 1h, 2h, 4h, 8h, 12h, 1d, 1w", interval)),
        };

        // Build query parameters
        let mut params = vec![
            ("symbol", symbol.to_string()),
            ("granularity", granularity.to_string()),
        ];

        if let Some(start) = start_time {
            params.push(("from", start.timestamp_millis().to_string()));
        }
        if let Some(end) = end_time {
            params.push(("to", end.timestamp_millis().to_string()));
        }

        let query_string = params
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect::<Vec<_>>()
            .join("&");

        let endpoint = format!("/api/v1/kline/query?{}", query_string);

        // KuCoin returns an array of arrays: [[timestamp, open, high, low, close, volume1, volume2], ...]
        let klines: Vec<Vec<serde_json::Value>> = self.get(&endpoint).await?;

        let klines = klines
            .into_iter()
            .filter_map(|k| {
                if k.len() < 7 {
                    tracing::warn!(
                        "Invalid kline data format for KuCoin: expected 7 fields, got {}",
                        k.len()
                    );
                    return None;
                }

                // Parse each field with error handling
                // KuCoin returns: [timestamp(int), open(float), high(float), low(float), close(float), volume(int), quote_volume(float)]
                let timestamp = k[0].as_i64()?;
                let open = k[1].as_f64().and_then(Decimal::from_f64)?;
                let high = k[2].as_f64().and_then(Decimal::from_f64)?;
                let low = k[3].as_f64().and_then(Decimal::from_f64)?;
                let close = k[4].as_f64().and_then(Decimal::from_f64)?;
                let volume = k[5].as_i64().and_then(Decimal::from_i64)?;
                let quote_volume = k[6].as_f64().and_then(Decimal::from_f64)?;

                let open_time = Utc.timestamp_millis_opt(timestamp).single()?;
                // Calculate close_time based on interval
                let close_time = open_time + chrono::Duration::minutes(granularity as i64);

                Some(Kline {
                    symbol: symbol.to_string(),
                    interval: interval.to_string(),
                    open_time,
                    close_time,
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
        Ok(self.contract_cache.contains(symbol).await)
    }

    async fn get_ticker(&self, symbol: &str) -> Result<Ticker> {
        // Get detailed contract info for this specific symbol (includes 24h stats)
        let contract: KucoinContractDetail =
            self.get(&format!("/api/v1/contracts/{}", symbol)).await?;

        // Get ticker for real-time best bid/ask
        let ticker: KucoinTicker = self
            .get(&format!("/api/v1/ticker?symbol={}", symbol))
            .await?;

        // Parse prices with fallback logging
        let last_price = contract
            .last_trade_price
            .and_then(Decimal::from_f64)
            .unwrap_or_else(|| {
                tracing::debug!("KuCoin {} last_trade_price is None, using ZERO", symbol);
                Decimal::ZERO
            });
        let mark_price = contract
            .mark_price
            .and_then(Decimal::from_f64)
            .unwrap_or_else(|| {
                tracing::debug!(
                    "KuCoin {} mark_price is None, using last_price fallback",
                    symbol
                );
                last_price
            });
        let index_price = contract
            .index_price
            .and_then(Decimal::from_f64)
            .unwrap_or_else(|| {
                tracing::debug!(
                    "KuCoin {} index_price is None, using last_price fallback",
                    symbol
                );
                last_price
            });

        let best_bid_price = Decimal::from_str(&ticker.best_bid_price).unwrap_or(Decimal::ZERO);
        let best_bid_qty = Decimal::from(ticker.best_bid_size);
        let best_ask_price = Decimal::from_str(&ticker.best_ask_price).unwrap_or(Decimal::ZERO);
        let best_ask_qty = Decimal::from(ticker.best_ask_size);

        // Parse 24h statistics with fallback logging
        let volume_24h = contract
            .volume_of_24h
            .and_then(Decimal::from_f64)
            .unwrap_or_else(|| {
                tracing::debug!("KuCoin {} volume_of_24h is None, using ZERO", symbol);
                Decimal::ZERO
            });
        let turnover_24h = contract
            .turnover_of_24h
            .and_then(Decimal::from_f64)
            .unwrap_or_else(|| {
                tracing::debug!("KuCoin {} turnover_of_24h is None, using ZERO", symbol);
                Decimal::ZERO
            });
        let price_change_24h = contract
            .price_chg
            .and_then(Decimal::from_f64)
            .unwrap_or_else(|| {
                tracing::debug!("KuCoin {} price_chg is None, using ZERO", symbol);
                Decimal::ZERO
            });
        let price_change_pct = contract
            .price_chg_pct
            .and_then(Decimal::from_f64)
            .unwrap_or_else(|| {
                tracing::debug!("KuCoin {} price_chg_pct is None, using ZERO", symbol);
                Decimal::ZERO
            });
        let high_price_24h = contract
            .high_price
            .and_then(Decimal::from_f64)
            .unwrap_or_else(|| {
                tracing::debug!("KuCoin {} high_price is None, using ZERO", symbol);
                Decimal::ZERO
            });
        let low_price_24h = contract
            .low_price
            .and_then(Decimal::from_f64)
            .unwrap_or_else(|| {
                tracing::debug!("KuCoin {} low_price is None, using ZERO", symbol);
                Decimal::ZERO
            });
        let mut open_interest = Decimal::from_str(&contract.open_interest).unwrap_or(Decimal::ZERO);
        open_interest *= Decimal::from_f64(contract.multiplier).unwrap_or(Decimal::ZERO);

        Ok(Ticker {
            symbol: contract.symbol,
            last_price,
            mark_price,
            index_price,
            best_bid_price,
            best_bid_qty,
            best_ask_price,
            best_ask_qty,
            timestamp: Utc.timestamp_nanos(ticker.timestamp),
            volume_24h,
            turnover_24h,
            open_interest,
            open_interest_notional: open_interest * mark_price,
            price_change_24h,
            price_change_pct,
            high_price_24h,
            low_price_24h,
        })
    }

    async fn get_all_tickers(&self) -> Result<Vec<Ticker>> {
        let markets = self.get_markets().await?;
        let mut tickers = Vec::new();
        for market in markets {
            match self.get_ticker(&market.symbol).await {
                Ok(ticker) => tickers.push(ticker),
                Err(e) => tracing::warn!("Failed to fetch ticker for {}: {}", market.symbol, e),
            }
        }
        Ok(tickers)
    }

    async fn get_funding_rate_history(
        &self,
        _symbol: &str,
        _start_time: Option<chrono::DateTime<chrono::Utc>>,
        _end_time: Option<chrono::DateTime<chrono::Utc>>,
        _limit: Option<u32>,
    ) -> Result<Vec<FundingRate>> {
        Err(anyhow!(
            "get_funding_rate_history is not implemented for KuCoin yet"
        ))
    }

    async fn get_open_interest(&self, symbol: &str) -> Result<OpenInterest> {
        // Try to get from active contracts list
        let contracts: Vec<KucoinContractDetail> = self.get("/api/v1/contracts/active").await?;
        let contract = contracts
            .into_iter()
            .find(|c| c.symbol == symbol)
            .ok_or_else(|| anyhow!("Contract {} not found", symbol))?;

        let open_interest = Decimal::from_str(&contract.open_interest).unwrap_or(Decimal::ZERO);

        Ok(OpenInterest {
            symbol: symbol.to_string(),
            open_interest,
            open_value: Decimal::ZERO,
            timestamp: Utc::now(),
        })
    }

    async fn get_market_stats(&self, _symbol: &str) -> Result<MarketStats> {
        Err(anyhow!(
            "get_market_stats is not available from KuCoin public API"
        ))
    }

    async fn get_all_market_stats(&self) -> Result<Vec<MarketStats>> {
        Err(anyhow!(
            "get_all_market_stats is not available from KuCoin public API"
        ))
    }
}
