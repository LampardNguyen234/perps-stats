use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use perps_core::{FundingRate, IPerps, Kline, Market, MarketStats, OpenInterest, Orderbook, Ticker, Trade};
use crate::cache::SymbolsCache;
use reqwest::Client;
use std::collections::HashMap;
use tracing;

use super::conversions;
use super::models::*;

const BASE_URL: &str = "https://mainnet.zklighter.elliot.ai/api/v1";

pub struct LighterClient {
    client: Client,
    base_url: String,
    // Cache for market_id lookups
    symbol_to_market_id: HashMap<String, u64>,
    /// Cached set of supported symbols
    symbols_cache: SymbolsCache,
}

impl LighterClient {

    /// Ensure the symbols cache is initialized
    async fn ensure_cache_initialized(&self) -> Result<()> {
        self.symbols_cache
            .get_or_init(|| async {
                let markets = self.get_markets().await?;
                Ok(markets.into_iter().map(|m| m.symbol).collect())
            })
            .await
    }

    pub fn new() -> Self {
        Self {
            client: Client::new(),
            base_url: BASE_URL.to_string(),
            symbol_to_market_id: HashMap::new(),
            symbols_cache: SymbolsCache::new(),
        }
    }

    /// Get market ID for a symbol
    async fn get_market_id(&mut self, symbol: &str) -> Result<u64> {
        // Check cache first
        if let Some(&market_id) = self.symbol_to_market_id.get(symbol) {
            return Ok(market_id);
        }

        // Fetch all markets and find the symbol
        let url = format!("{}/orderBooks", self.base_url);
        let response: LighterResponse<OrderBooksResponse> = self
            .client
            .get(&url)
            .send()
            .await?
            .json()
            .await?;

        if response.code != 200 {
            return Err(anyhow!("API error: code {}", response.code));
        }

        // Build cache
        for orderbook in &response.data.order_books {
            self.symbol_to_market_id
                .insert(orderbook.symbol.clone(), orderbook.market_id);
        }

        // Try again from cache
        self.symbol_to_market_id
            .get(symbol)
            .copied()
            .ok_or_else(|| anyhow!("Symbol {} not found", symbol))
    }

    /// Fetch order book details for a specific market
    async fn fetch_orderbook_detail(&self, symbol: &str) -> Result<OrderBookDetail> {
        let url = format!("{}/orderBookDetails", self.base_url);
        let response: LighterResponse<OrderBookDetailsResponse> = self
            .client
            .get(&url)
            .send()
            .await?
            .json()
            .await?;

        if response.code != 200 {
            return Err(anyhow!("API error: code {}", response.code));
        }

        response
            .data
            .order_book_details
            .into_iter()
            .find(|detail| detail.symbol == symbol)
            .ok_or_else(|| anyhow!("Symbol {} not found in order book details", symbol))
    }
}

impl Default for LighterClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IPerps for LighterClient {
    fn get_name(&self) -> &str {
        "lighter"
    }

    fn parse_symbol(&self, symbol: &str) -> String {
        // Lighter uses simple symbol names like "BTC", "ETH"
        // Handle special cases
        match symbol.to_uppercase().as_str() {
            "BTCUSDT" | "BTC-USDT" | "BTCUSD" => "BTC".to_string(),
            "ETHUSDT" | "ETH-USDT" | "ETHUSD" => "ETH".to_string(),
            "SOLUSDT" | "SOL-USDT" | "SOLUSD" => "SOL".to_string(),
            "AVAXUSDT" | "AVAX-USDT" | "AVAXUSD" => "AVAX".to_string(),
            _ => {
                // Remove common suffixes
                let s = symbol.to_uppercase();
                s.trim_end_matches("USDT")
                    .trim_end_matches("-USDT")
                    .trim_end_matches("USD")
                    .trim_end_matches("-USD")
                    .to_string()
            }
        }
    }

    async fn get_markets(&self) -> Result<Vec<Market>> {
        let url = format!("{}/orderBooks", self.base_url);
        tracing::debug!("Fetching markets from Lighter: {}", url);

        let response: LighterResponse<OrderBooksResponse> = self
            .client
            .get(&url)
            .send()
            .await?
            .json()
            .await?;

        if response.code != 200 {
            return Err(anyhow!("API error: code {}", response.code));
        }

        let markets: Result<Vec<Market>> = response
            .data
            .order_books
            .iter()
            .map(conversions::to_market)
            .collect();

        markets
    }

    async fn get_market(&self, symbol: &str) -> Result<Market> {
        let url = format!("{}/orderBooks", self.base_url);
        tracing::debug!("Fetching market {} from Lighter: {}", symbol, url);

        let response: LighterResponse<OrderBooksResponse> = self
            .client
            .get(&url)
            .send()
            .await?
            .json()
            .await?;

        if response.code != 200 {
            return Err(anyhow!("API error: code {}", response.code));
        }

        let orderbook = response
            .data
            .order_books
            .iter()
            .find(|ob| ob.symbol == symbol)
            .ok_or_else(|| anyhow!("Symbol {} not found", symbol))?;

        conversions::to_market(orderbook)
    }

    async fn get_ticker(&self, symbol: &str) -> Result<Ticker> {
        tracing::debug!("Fetching ticker for {} from Lighter", symbol);

        let detail = self.fetch_orderbook_detail(symbol).await?;
        tracing::debug!("get_ticker detail: {:?}", detail);
        conversions::to_ticker(&detail)
    }

    async fn get_all_tickers(&self) -> Result<Vec<Ticker>> {
        let url = format!("{}/orderBookDetails", self.base_url);
        tracing::debug!("Fetching all tickers from Lighter: {}", url);

        let response: LighterResponse<OrderBookDetailsResponse> = self
            .client
            .get(&url)
            .send()
            .await?
            .json()
            .await?;

        if response.code != 200 {
            return Err(anyhow!("API error: code {}", response.code));
        }

        let tickers: Result<Vec<Ticker>> = response
            .data
            .order_book_details
            .iter()
            .map(conversions::to_ticker)
            .collect();

        tickers
    }

    async fn get_orderbook(&self, symbol: &str, depth: u32) -> Result<Orderbook> {
        // Lighter API has a maximum limit of 100
        let capped_depth = depth.min(100);
        tracing::debug!("Fetching orderbook for {} from Lighter (depth: {}, capped: {})", symbol, depth, capped_depth);

        // Need to get market_id first
        let market_id = self.clone().get_market_id(symbol).await?;

        let url = format!(
            "{}/orderBookOrders?market_id={}&limit={}",
            self.base_url, market_id, capped_depth
        );

        let response: LighterResponse<OrderBookOrdersResponse> = self
            .client
            .get(&url)
            .send()
            .await?
            .json()
            .await?;

        if response.code != 200 {
            return Err(anyhow!("API error: code {}", response.code));
        }

        conversions::to_orderbook(symbol, &response.data.bids, &response.data.asks)
    }

    async fn get_funding_rate(&self, symbol: &str) -> Result<FundingRate> {
        let url = format!("{}/funding-rates", self.base_url);
        tracing::debug!("Fetching funding rate for {} from Lighter: {}", symbol, url);

        let response: LighterResponse<FundingRatesResponse> = self
            .client
            .get(&url)
            .send()
            .await?
            .json()
            .await?;

        if response.code != 200 {
            return Err(anyhow!("API error: code {}", response.code));
        }

        let funding_rate = response
            .data
            .funding_rates
            .iter()
            .find(|fr| fr.symbol == symbol)
            .ok_or_else(|| anyhow!("Funding rate for {} not found", symbol))?;

        conversions::to_funding_rate(funding_rate)
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
        Err(anyhow!("Funding rate history not yet implemented for Lighter"))
    }

    async fn get_open_interest(&self, symbol: &str) -> Result<OpenInterest> {
        tracing::debug!("Fetching open interest for {} from Lighter", symbol);

        let detail = self.fetch_orderbook_detail(symbol).await?;

        Ok(OpenInterest {
            symbol: symbol.to_string(),
            open_interest: rust_decimal::Decimal::from_f64_retain(detail.open_interest)
                .unwrap_or_else(|| rust_decimal::Decimal::from(0)),
            open_value: rust_decimal::Decimal::from_f64_retain(detail.open_interest * detail.last_trade_price)
                .unwrap_or_else(|| rust_decimal::Decimal::from(0)),
            timestamp: Utc::now(),
        })
    }

    async fn get_klines(
        &self,
        _symbol: &str,
        _interval: &str,
        _start_time: Option<DateTime<Utc>>,
        _end_time: Option<DateTime<Utc>>,
        _limit: Option<u32>,
    ) -> Result<Vec<Kline>> {
        // Lighter has /candlesticks endpoint, but implementation would require
        // understanding their interval format and parameters
        Err(anyhow!("Klines not yet implemented for Lighter"))
    }

    async fn get_recent_trades(&self, _symbol: &str, _limit: u32) -> Result<Vec<Trade>> {
        // Lighter has /recentTrades endpoint, but would need implementation
        Err(anyhow!("Recent trades not yet implemented for Lighter"))
    }

    async fn get_market_stats(&self, symbol: &str) -> Result<MarketStats> {
        tracing::debug!("Fetching market stats for {} from Lighter", symbol);

        let detail = self.fetch_orderbook_detail(symbol).await?;

        let last_price = rust_decimal::Decimal::from_f64_retain(detail.last_trade_price)
            .unwrap_or_else(|| rust_decimal::Decimal::from(0));

        // Lighter API returns daily_price_change as a percentage (e.g., -1.19 for -1.19%)
        let price_change_pct_raw = rust_decimal::Decimal::from_f64_retain(detail.daily_price_change)
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
            symbol: symbol.to_string(),
            last_price,
            mark_price: last_price,
            index_price: last_price,
            volume_24h: rust_decimal::Decimal::from_f64_retain(detail.daily_base_token_volume)
                .unwrap_or_else(|| rust_decimal::Decimal::from(0)),
            turnover_24h: rust_decimal::Decimal::from_f64_retain(detail.daily_quote_token_volume)
                .unwrap_or_else(|| rust_decimal::Decimal::from(0)),
            high_price_24h: rust_decimal::Decimal::from_f64_retain(detail.daily_price_high)
                .unwrap_or_else(|| rust_decimal::Decimal::from(0)),
            low_price_24h: rust_decimal::Decimal::from_f64_retain(detail.daily_price_low)
                .unwrap_or_else(|| rust_decimal::Decimal::from(0)),
            price_change_24h,
            price_change_pct,
            open_interest: rust_decimal::Decimal::from_f64_retain(detail.open_interest)
                .unwrap_or_else(|| rust_decimal::Decimal::from(0)),
            funding_rate: rust_decimal::Decimal::ZERO,
            timestamp: Utc::now(),
        })
    }

    async fn get_all_market_stats(&self) -> Result<Vec<MarketStats>> {
        let url = format!("{}/orderBookDetails", self.base_url);
        tracing::debug!("Fetching all market stats from Lighter: {}", url);

        let response: LighterResponse<OrderBookDetailsResponse> = self
            .client
            .get(&url)
            .send()
            .await?
            .json()
            .await?;

        if response.code != 200 {
            return Err(anyhow!("API error: code {}", response.code));
        }

        let stats: Vec<MarketStats> = response
            .data
            .order_book_details
            .iter()
            .map(|detail| {
                let last_price = rust_decimal::Decimal::from_f64_retain(detail.last_trade_price)
                    .unwrap_or_else(|| rust_decimal::Decimal::from(0));

                // Lighter API returns daily_price_change as a percentage
                let price_change_pct = rust_decimal::Decimal::from_f64_retain(detail.daily_price_change)
                    .unwrap_or_else(|| rust_decimal::Decimal::from(0));

                // Calculate absolute price change
                let price_change_24h = if last_price > rust_decimal::Decimal::ZERO {
                    (price_change_pct / rust_decimal::Decimal::from(100)) * last_price
                } else {
                    rust_decimal::Decimal::ZERO
                };

                MarketStats {
                    symbol: detail.symbol.clone(),
                    last_price,
                    mark_price: last_price,
                    index_price: last_price,
                    volume_24h: rust_decimal::Decimal::from_f64_retain(detail.daily_base_token_volume)
                        .unwrap_or_else(|| rust_decimal::Decimal::from(0)),
                    turnover_24h: rust_decimal::Decimal::from_f64_retain(detail.daily_quote_token_volume)
                        .unwrap_or_else(|| rust_decimal::Decimal::from(0)),
                    high_price_24h: rust_decimal::Decimal::from_f64_retain(detail.daily_price_high)
                        .unwrap_or_else(|| rust_decimal::Decimal::from(0)),
                    low_price_24h: rust_decimal::Decimal::from_f64_retain(detail.daily_price_low)
                        .unwrap_or_else(|| rust_decimal::Decimal::from(0)),
                    price_change_24h,
                    price_change_pct,
                    open_interest: rust_decimal::Decimal::from_f64_retain(detail.open_interest)
                        .unwrap_or_else(|| rust_decimal::Decimal::from(0)),
                    funding_rate: rust_decimal::Decimal::ZERO,
                    timestamp: Utc::now(),
                }
            })
            .collect();

        Ok(stats)
    }

    async fn is_supported(&self, symbol: &str) -> Result<bool> {
        self.ensure_cache_initialized().await?;
        Ok(self.symbols_cache.contains(symbol).await)
    }
}

// Need to implement Clone for the mut method
impl Clone for LighterClient {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            base_url: self.base_url.clone(),
            symbol_to_market_id: self.symbol_to_market_id.clone(),
            symbols_cache: self.symbols_cache.clone(),
        }
    }
}
