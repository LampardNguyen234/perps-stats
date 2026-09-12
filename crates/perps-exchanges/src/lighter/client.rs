use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use perps_core::{
    execute_with_retry, FundingRate, IPerps, Kline, Market, MarketStats, MultiResolutionOrderbook,
    OpenInterest, RateLimiter, RetryConfig, Ticker, Trade,
};
use reqwest::Client;
use rust_decimal::prelude::FromPrimitive;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};
use tracing;

use super::conversions;
use super::models::*;
use super::ws_client::LighterWsClient;
use perps_core::{FullOrderbookAdapter, WsOrderbookConfig, WsOrderbookManager};

const ORDER_BOOK_DETAILS_CACHE_TTL: Duration = Duration::from_secs(10);

const BASE_URL: &str = "https://mainnet.zklighter.elliot.ai/api/v1";

struct OrderBookDetailsCache {
    data: Vec<OrderBookDetail>,
    fetched_at: Instant,
}

pub struct LighterClient {
    client: Client,
    base_url: String,
    /// symbol → market_id; shared with the WS manager so it can resolve IDs
    /// without a separate REST call when the manager starts.
    market_id_cache: Arc<RwLock<HashMap<String, u64>>>,
    /// Rate limiter for API requests
    rate_limiter: Arc<RateLimiter>,
    /// TTL cache for orderBookDetails (shared across clones)
    order_book_details_cache: Arc<Mutex<Option<OrderBookDetailsCache>>>,
    /// Present when ENABLE_ORDERBOOK_STREAMING=true.  get_orderbook delegates
    /// to this instead of making a REST call.
    orderbook_manager: Option<Arc<WsOrderbookManager>>,
}

impl LighterClient {
    pub fn new() -> Self {
        let market_id_cache: Arc<RwLock<HashMap<String, u64>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let orderbook_manager = if std::env::var("DATABASE_URL").is_ok()
            && std::env::var("ENABLE_ORDERBOOK_STREAMING")
                .unwrap_or_default()
                .to_lowercase()
                == "true"
        {
            Some(Arc::new(WsOrderbookManager::new(
                Arc::new(FullOrderbookAdapter(Arc::new(LighterWsClient::new()))),
                WsOrderbookConfig {
                    reconnect_delay: Duration::from_secs(2),
                    ..Default::default()
                },
                vec![],
            )))
        } else {
            None
        };
        Self {
            client: Client::new(),
            base_url: BASE_URL.to_string(),
            market_id_cache,
            rate_limiter: Arc::new(RateLimiter::lighter()),
            order_book_details_cache: Arc::new(Mutex::new(None)),
            orderbook_manager,
        }
    }

    /// Fetch all order book details, using the TTL cache when fresh enough.
    async fn get_order_book_details(&self) -> Result<Vec<OrderBookDetail>> {
        // Hold the lock through refill so concurrent commands share one snapshot.
        let mut cache = self.order_book_details_cache.lock().await;
        if let Some(ref c) = *cache {
            if c.fetched_at.elapsed() < ORDER_BOOK_DETAILS_CACHE_TTL {
                return Ok(c.data.clone());
            }
        }

        let url = format!("{}/orderBookDetails", self.base_url);
        let response: LighterResponse<OrderBookDetailsResponse> = self.get(&url).await?;

        if response.code != 200 {
            return Err(anyhow!("API error: code {}", response.code));
        }

        let details = response.data.order_book_details;
        // Cache exact API names: never run user-input aliases over metadata names.
        *self.market_id_cache.write().await = details
            .iter()
            .filter(|d| d.is_collectable())
            .map(|d| (d.symbol.clone(), d.market_id))
            .collect();
        *cache = Some(OrderBookDetailsCache {
            data: details.clone(),
            fetched_at: Instant::now(),
        });

        Ok(details)
    }

    /// Helper method to make rate-limited GET requests with retry
    async fn get<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        let config = RetryConfig::default();
        let url = url.to_string();
        let client = self.client.clone();
        let rate_limiter = self.rate_limiter.clone();

        execute_with_retry(&config, || {
            let url = url.clone();
            let client = client.clone();
            let rate_limiter = rate_limiter.clone();
            async move {
                rate_limiter
                    .execute(|| {
                        let url = url.clone();
                        let client = client.clone();
                        async move {
                            tracing::trace!("Requesting: {}", url);
                            let response = client.get(&url).send().await?;

                            // Check HTTP status first
                            if !response.status().is_success() {
                                let status = response.status();
                                let text = response
                                    .text()
                                    .await
                                    .unwrap_or_else(|_| "Unable to read response body".to_string());
                                return Err(anyhow!("HTTP {}: {}", status, text));
                            }

                            // Try to decode as the expected type
                            let data = response.json::<T>().await?;
                            Ok(data)
                        }
                    })
                    .await
            }
        })
        .await
    }

    /// Resolve history requests too, including markets currently reduce-only/inactive.
    async fn get_market_id(&self, symbol: &str) -> Result<u64> {
        Ok(self.find_orderbook_detail(symbol).await?.market_id)
    }

    async fn find_orderbook_detail(&self, symbol: &str) -> Result<OrderBookDetail> {
        let sym = self.parse_symbol(symbol);
        self.get_order_book_details()
            .await?
            .into_iter()
            .find(|d| d.symbol == sym && d.market_type == "perp")
            .ok_or_else(|| anyhow!("Symbol {} not found in Lighter perpetual markets", symbol))
    }

    /// Current-data requests must obey the same policy as market discovery.
    async fn fetch_orderbook_detail(&self, symbol: &str) -> Result<OrderBookDetail> {
        let detail = self.find_orderbook_detail(symbol).await?;
        if let Some(reason) = detail.exclusion_reason() {
            return Err(anyhow!("Lighter market {} excluded: {}", symbol, reason));
        }
        Ok(detail)
    }
}

impl Default for LighterClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IPerps for LighterClient {
    async fn prewarm_streams(&self, symbols: &[String]) -> Result<()> {
        if let Some(manager) = &self.orderbook_manager {
            self.get_order_book_details().await?;
            manager
                .prewarm(
                    symbols
                        .iter()
                        .map(|s| self.normalize_symbol(&self.parse_symbol(s)))
                        .collect(),
                )
                .await?;
        }
        Ok(())
    }

    fn get_name(&self) -> &str {
        "lighter"
    }

    fn normalize_symbol(&self, exchange_symbol: &str) -> String {
        super::symbols::to_global_symbol(exchange_symbol)
    }

    fn parse_symbol(&self, symbol: &str) -> String {
        super::symbols::to_exchange_symbol(symbol)
    }

    async fn get_markets(&self) -> Result<Vec<Market>> {
        self.get_order_book_details()
            .await?
            .iter()
            .filter(|d| d.is_collectable())
            .map(|d| {
                let mut market = conversions::to_market_from_detail(d)?;
                market.symbol = self.normalize_symbol(&market.symbol);
                Ok(market)
            })
            .collect()
    }

    async fn get_market(&self, symbol: &str) -> Result<Market> {
        let detail = self.fetch_orderbook_detail(symbol).await?;
        let mut market = conversions::to_market_from_detail(&detail)?;
        market.symbol = self.normalize_symbol(&market.symbol);
        Ok(market)
    }

    async fn get_ticker(&self, symbol: &str) -> Result<Ticker> {
        let symbol = self.parse_symbol(symbol);
        tracing::debug!("Fetching ticker for {} from Lighter", symbol);

        let detail = self.fetch_orderbook_detail(&symbol).await?;
        tracing::debug!("get_ticker detail: {:?}", detail);

        // Also fetch orderbook to get best bid/ask quantities
        // Use a small depth (5) to minimize API load
        let multi_orderbook = self.get_orderbook(&symbol, 5).await?;
        let orderbook = multi_orderbook
            .best_for_tight_spreads()
            .ok_or_else(|| anyhow!("No orderbook available for {}", symbol))?;

        let mut ticker = conversions::to_ticker_with_orderbook(&detail, orderbook)?;
        ticker.symbol = self.normalize_symbol(&ticker.symbol);
        Ok(ticker)
    }

    async fn get_all_tickers(&self) -> Result<Vec<Ticker>> {
        tracing::debug!("Fetching all tickers from Lighter");

        let tickers: Result<Vec<Ticker>> = self
            .get_order_book_details()
            .await?
            .iter()
            .filter(|d| d.is_collectable())
            .map(|d| {
                let mut t = conversions::to_ticker(d)?;
                t.symbol = self.normalize_symbol(&t.symbol);
                Ok(t)
            })
            .collect();

        tickers
    }

    async fn get_orderbook(&self, symbol: &str, depth: u32) -> Result<MultiResolutionOrderbook> {
        let symbol = self.parse_symbol(symbol);

        let detail = self.fetch_orderbook_detail(&symbol).await?;

        if let Some(mgr) = &self.orderbook_manager {
            let normalized = self.normalize_symbol(&symbol);
            let mut ob = mgr
                .get_orderbook(&normalized, depth, || async {
                    let capped_depth = depth.min(100);
                    let market_id = detail.market_id;
                    let url = format!(
                        "{}/orderBookOrders?market_id={}&limit={}",
                        self.base_url, market_id, capped_depth
                    );
                    let response: LighterResponse<OrderBookOrdersResponse> =
                        self.get(&url).await?;
                    if response.code != 200 {
                        return Err(anyhow!("API error: code {}", response.code));
                    }
                    let orderbook = conversions::to_orderbook(
                        &normalized,
                        &response.data.bids,
                        &response.data.asks,
                    )?;
                    Ok((orderbook, 0))
                })
                .await?;
            // symbol passed to manager is exchange-level (post parse_symbol, e.g. "WTI").
            // Normalize back to global symbol ("CL") so stored data is consistent with REST path.
            ob.symbol = normalized.clone();
            let mut mob = MultiResolutionOrderbook::from_single(ob);
            for level in &mut mob.orderbooks {
                level.symbol = normalized.clone();
            }
            return Ok(mob);
        }

        // Lighter API has a maximum limit of 100
        let capped_depth = depth.min(100);
        tracing::debug!(
            "Fetching orderbook for {} from Lighter (depth: {}, capped: {})",
            symbol,
            depth,
            capped_depth
        );

        let market_id = detail.market_id;

        let url = format!(
            "{}/orderBookOrders?market_id={}&limit={}",
            self.base_url, market_id, capped_depth
        );

        let response: LighterResponse<OrderBookOrdersResponse> = self.get(&url).await?;

        if response.code != 200 {
            return Err(anyhow!("API error: code {}", response.code));
        }

        let normalized = self.normalize_symbol(&symbol);
        let orderbook =
            conversions::to_orderbook(&normalized, &response.data.bids, &response.data.asks)?;
        Ok(MultiResolutionOrderbook::from_single(orderbook))
    }

    async fn get_funding_rate(&self, symbol: &str) -> Result<FundingRate> {
        let symbol = self.parse_symbol(symbol);
        let detail = self.fetch_orderbook_detail(&symbol).await?;
        let url = format!("{}/funding-rates", self.base_url);
        tracing::debug!("Fetching funding rate for {} from Lighter: {}", symbol, url);

        let response: LighterResponse<FundingRatesResponse> = self.get(&url).await?;

        if response.code != 200 {
            return Err(anyhow!("API error: code {}", response.code));
        }

        let funding_rate = response
            .data
            .funding_rates
            .iter()
            .find(|fr| fr.exchange == "lighter" && fr.market_id == detail.market_id)
            .ok_or_else(|| anyhow!("Funding rate for {} not found", symbol))?;

        let mut fr = conversions::to_funding_rate(funding_rate)?;
        fr.symbol = self.normalize_symbol(&fr.symbol);
        Ok(fr)
    }

    async fn get_funding_rate_history(
        &self,
        _symbol: &str,
        _start_time: Option<DateTime<Utc>>,
        _end_time: Option<DateTime<Utc>>,
        _limit: Option<u32>,
    ) -> Result<Vec<FundingRate>> {
        // Lighter API doesn't provide historical funding rates endpoint
        // Would need /fundings endpoint with proper filtering
        Err(anyhow!(
            "Funding rate history not yet implemented for Lighter"
        ))
    }

    async fn get_open_interest(&self, symbol: &str) -> Result<OpenInterest> {
        let symbol = self.parse_symbol(symbol);
        tracing::debug!("Fetching open interest for {} from Lighter", symbol);

        let detail = self.fetch_orderbook_detail(&symbol).await?;

        Ok(OpenInterest {
            symbol: self.normalize_symbol(&symbol),
            open_interest: rust_decimal::Decimal::from_f64(detail.open_interest)
                .unwrap_or_else(|| rust_decimal::Decimal::from(0)),
            open_value: rust_decimal::Decimal::from_f64(
                detail.open_interest * detail.last_trade_price,
            )
            .unwrap_or_else(|| rust_decimal::Decimal::from(0)),
            timestamp: Utc::now(),
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
        tracing::debug!(
            "Fetching klines for {} from Lighter (interval: {})",
            symbol,
            interval
        );

        let symbol = self.parse_symbol(symbol);
        // Get market_id for the symbol
        let market_id = self.get_market_id(&symbol).await?;

        // Lighter API requires start_timestamp, end_timestamp, AND count_back
        // If not provided, use sensible defaults
        let now = Utc::now();
        let start = start_time.unwrap_or_else(|| now - chrono::Duration::hours(24));
        let end = end_time.unwrap_or(now);
        let count_back = limit.unwrap_or(1000); // Default to 1000 if not specified

        // Build the URL with all required parameters
        let url = format!(
            "{}/candlesticks?market_id={}&resolution={}&start_timestamp={}&end_timestamp={}&count_back={}",
            self.base_url,
            market_id,
            interval,
            start.timestamp_millis(),
            end.timestamp_millis(),
            count_back
        );

        tracing::debug!("Lighter klines URL: {}", url);

        let response: LighterResponse<CandlesticksResponse> = self.get(&url).await?;

        if response.code != 200 {
            return Err(anyhow!("Lighter API error: code {} - This may indicate an unsupported interval or invalid parameters", response.code));
        }

        // Convert candlesticks to Kline format
        let normalized = self.normalize_symbol(&symbol);
        let klines: Result<Vec<Kline>> = response
            .data
            .candlesticks
            .iter()
            .map(|cs| conversions::to_kline(&normalized, interval, cs))
            .collect();

        klines
    }

    async fn get_recent_trades(&self, _symbol: &str, _limit: u32) -> Result<Vec<Trade>> {
        // Lighter has /recentTrades endpoint, but would need implementation
        Err(anyhow!("Recent trades not yet implemented for Lighter"))
    }

    async fn get_market_stats(&self, symbol: &str) -> Result<MarketStats> {
        let symbol = self.parse_symbol(symbol);
        tracing::debug!("Fetching market stats for {} from Lighter", symbol);

        let detail = self.fetch_orderbook_detail(&symbol).await?;

        let last_price = rust_decimal::Decimal::from_f64(detail.last_trade_price)
            .unwrap_or_else(|| rust_decimal::Decimal::from(0));

        // Lighter API returns daily_price_change as a percentage (e.g., -1.19 for -1.19%)
        let price_change_pct_raw = rust_decimal::Decimal::from_f64(detail.daily_price_change)
            .unwrap_or_else(|| rust_decimal::Decimal::from(0));

        // Convert to decimal to match other exchanges (e.g., -1.19% -> -0.0119)
        let price_change_pct = price_change_pct_raw / rust_decimal::Decimal::from(100);

        // Calculate absolute price change: (percentage decimal) * current_price
        let price_change_24h = if last_price > rust_decimal::Decimal::ZERO {
            price_change_pct * last_price
        } else {
            rust_decimal::Decimal::ZERO
        };

        Ok(MarketStats {
            symbol: self.normalize_symbol(&symbol),
            last_price,
            mark_price: last_price,
            index_price: last_price,
            volume_24h: rust_decimal::Decimal::from_f64(detail.daily_base_token_volume)
                .unwrap_or_else(|| rust_decimal::Decimal::from(0)),
            turnover_24h: rust_decimal::Decimal::from_f64(detail.daily_quote_token_volume)
                .unwrap_or_else(|| rust_decimal::Decimal::from(0)),
            high_price_24h: rust_decimal::Decimal::from_f64(detail.daily_price_high)
                .unwrap_or_else(|| rust_decimal::Decimal::from(0)),
            low_price_24h: rust_decimal::Decimal::from_f64(detail.daily_price_low)
                .unwrap_or_else(|| rust_decimal::Decimal::from(0)),
            price_change_24h,
            price_change_pct,
            open_interest: rust_decimal::Decimal::from_f64(detail.open_interest)
                .unwrap_or_else(|| rust_decimal::Decimal::from(0)),
            funding_rate: rust_decimal::Decimal::ZERO,
            timestamp: Utc::now(),
        })
    }

    async fn get_all_market_stats(&self) -> Result<Vec<MarketStats>> {
        tracing::debug!("Fetching all market stats from Lighter");

        let stats: Vec<MarketStats> = self
            .get_order_book_details()
            .await?
            .iter()
            .filter(|d| d.is_collectable())
            .map(|detail| {
                let last_price = rust_decimal::Decimal::from_f64(detail.last_trade_price)
                    .unwrap_or_else(|| rust_decimal::Decimal::from(0));

                // Lighter API returns daily_price_change as a percentage
                let price_change_pct = rust_decimal::Decimal::from_f64(detail.daily_price_change)
                    .unwrap_or_else(|| rust_decimal::Decimal::from(0));

                // Calculate absolute price change
                let price_change_24h = if last_price > rust_decimal::Decimal::ZERO {
                    (price_change_pct / rust_decimal::Decimal::from(100)) * last_price
                } else {
                    rust_decimal::Decimal::ZERO
                };

                MarketStats {
                    symbol: self.normalize_symbol(&detail.symbol),
                    last_price,
                    mark_price: last_price,
                    index_price: last_price,
                    volume_24h: rust_decimal::Decimal::from_f64(detail.daily_base_token_volume)
                        .unwrap_or_else(|| rust_decimal::Decimal::from(0)),
                    turnover_24h: rust_decimal::Decimal::from_f64(detail.daily_quote_token_volume)
                        .unwrap_or_else(|| rust_decimal::Decimal::from(0)),
                    high_price_24h: rust_decimal::Decimal::from_f64(detail.daily_price_high)
                        .unwrap_or_else(|| rust_decimal::Decimal::from(0)),
                    low_price_24h: rust_decimal::Decimal::from_f64(detail.daily_price_low)
                        .unwrap_or_else(|| rust_decimal::Decimal::from(0)),
                    price_change_24h,
                    price_change_pct,
                    open_interest: rust_decimal::Decimal::from_f64(detail.open_interest)
                        .unwrap_or_else(|| rust_decimal::Decimal::from(0)),
                    funding_rate: rust_decimal::Decimal::ZERO,
                    timestamp: Utc::now(),
                }
            })
            .collect();

        Ok(stats)
    }

    async fn is_supported(&self, symbol: &str) -> Result<bool> {
        let sym = self.parse_symbol(symbol);
        let details = self.get_order_book_details().await?;
        match details
            .iter()
            .find(|d| d.symbol == sym && d.market_type == "perp")
        {
            Some(detail) => {
                if let Some(reason) = detail.exclusion_reason() {
                    tracing::info!("Lighter market {} excluded: {}", symbol, reason);
                    Ok(false)
                } else {
                    Ok(true)
                }
            }
            None => Ok(false),
        }
    }
}

impl Clone for LighterClient {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            base_url: self.base_url.clone(),
            market_id_cache: self.market_id_cache.clone(),
            rate_limiter: self.rate_limiter.clone(),
            order_book_details_cache: self.order_book_details_cache.clone(),
            orderbook_manager: self.orderbook_manager.clone(),
        }
    }
}
