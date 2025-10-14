use crate::cache::SymbolsCache;
use crate::pacifica::types::*;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use perps_core::types::*;
use perps_core::{execute_with_retry, IPerps, RateLimiter, RetryConfig};
use rust_decimal::Decimal;
use std::str::FromStr;
use std::sync::Arc;

const BASE_URL: &str = "https://api.pacifica.fi/api/v1";

/// A client for the Pacifica exchange (StarkEx-based L2 DEX).
#[derive(Clone)]
pub struct PacificaClient {
    http: reqwest::Client,
    /// Cached set of supported symbols
    symbols_cache: SymbolsCache,
    /// Rate limiter for API requests
    rate_limiter: Arc<RateLimiter>,
}

impl PacificaClient {
    pub fn new() -> Self {
        // Build HTTP client with headers to work with Cloudflare
        let http = reqwest::Client::builder()
            .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
            .build()
            .expect("Failed to build HTTP client");

        Self {
            http,
            symbols_cache: SymbolsCache::new(),
            rate_limiter: Arc::new(RateLimiter::pacifica()),
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
                            let wrapper: PacificaResponse<T> = response.json().await?;
                            if !wrapper.success {
                                let error = wrapper
                                    .error
                                    .unwrap_or(String::from("Unknown error"));
                                let code = wrapper
                                    .code
                                    .unwrap_or(String::from("UNKNOWN"));
                                return Err(anyhow!(
                                    "Pacifica API error: {} - {}",
                                    code,
                                    error
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
}

impl Default for PacificaClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IPerps for PacificaClient {
    fn get_name(&self) -> &str {
        "pacifica"
    }

    fn parse_symbol(&self, symbol: &str) -> String {
        // Pacifica uses uppercase symbols like "BTC", "ETH"
        symbol.to_uppercase()
    }

    async fn get_markets(&self) -> Result<Vec<Market>> {
        let markets: Vec<PacificaMarket> = self.get("/info").await?;

        let markets = markets
            .into_iter()
            .map(|m| {
                let price_scale = m.tick_size.find('.').map(|pos| {
                    (m.tick_size.len() - pos - 1) as i32
                }).unwrap_or(0);

                let quantity_scale = m.lot_size.find('.').map(|pos| {
                    (m.lot_size.len() - pos - 1) as i32
                }).unwrap_or(0);

                Market {
                    symbol: m.symbol.clone(),
                    contract: m.symbol,
                    price_scale,
                    quantity_scale,
                    min_order_qty: Decimal::from_str(&m.lot_size).unwrap_or(Decimal::ZERO),
                    contract_size: Decimal::ONE,
                    max_order_qty: Decimal::from_str(&m.max_order_size).unwrap_or(Decimal::ZERO),
                    min_order_value: Decimal::from_str(&m.min_order_size).unwrap_or(Decimal::ZERO),
                    max_leverage: Decimal::from(m.max_leverage),
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
        // Get all prices and filter for the symbol
        let prices: Vec<PacificaPrice> = self.get("/info/prices").await?;

        let price = prices
            .into_iter()
            .find(|p| p.symbol == symbol)
            .ok_or_else(|| anyhow!("Price data for {} not found", symbol))?;

        let last_price = Decimal::from_str(&price.mid)?;
        let mark_price = Decimal::from_str(&price.mark)?;
        let index_price = Decimal::from_str(&price.oracle)?;
        let open_interest = Decimal::from_str(&price.open_interest)?;
        let volume_24h = Decimal::from_str(&price.volume_24h)?;
        let yesterday_price = Decimal::from_str(&price.yesterday_price)?;

        let price_change_24h = last_price - yesterday_price;
        let price_change_pct = if yesterday_price > Decimal::ZERO {
            price_change_24h / yesterday_price
        } else {
            Decimal::ZERO
        };

        // Get orderbook for best bid/ask with quantities
        let orderbook = self.get_orderbook(symbol, 1).await?;
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

        // Calculate 24h high/low from price change
        // This is an approximation since Pacifica doesn't provide these directly
        let high_price_24h = if price_change_24h > Decimal::ZERO {
            last_price
        } else {
            yesterday_price
        };
        let low_price_24h = if price_change_24h < Decimal::ZERO {
            last_price
        } else {
            yesterday_price
        };

        Ok(Ticker {
            symbol: symbol.to_string(),
            last_price,
            mark_price,
            index_price,
            best_bid_price,
            best_bid_qty,
            best_ask_price,
            best_ask_qty,
            volume_24h,
            turnover_24h: Decimal::ZERO, // Not provided by Pacifica
            open_interest,
            open_interest_notional: open_interest * mark_price,
            price_change_24h,
            price_change_pct,
            high_price_24h,
            low_price_24h,
            timestamp: Utc.timestamp_millis_opt(price.timestamp).unwrap(),
        })
    }

    async fn get_all_tickers(&self) -> Result<Vec<Ticker>> {
        let prices: Vec<PacificaPrice> = self.get("/info/prices").await?;
        let mut tickers = Vec::new();

        for price in prices {
            let last_price = Decimal::from_str(&price.mid).unwrap_or(Decimal::ZERO);
            let mark_price = Decimal::from_str(&price.mark).unwrap_or(Decimal::ZERO);
            let index_price = Decimal::from_str(&price.oracle).unwrap_or(Decimal::ZERO);
            let open_interest = Decimal::from_str(&price.open_interest).unwrap_or(Decimal::ZERO);
            let volume_24h = Decimal::from_str(&price.volume_24h).unwrap_or(Decimal::ZERO);
            let yesterday_price = Decimal::from_str(&price.yesterday_price).unwrap_or(Decimal::ZERO);

            let price_change_24h = last_price - yesterday_price;
            let price_change_pct = if yesterday_price > Decimal::ZERO {
                price_change_24h / yesterday_price
            } else {
                Decimal::ZERO
            };

            // For all tickers, skip orderbook fetch for performance
            let high_price_24h = if price_change_24h > Decimal::ZERO {
                last_price
            } else {
                yesterday_price
            };
            let low_price_24h = if price_change_24h < Decimal::ZERO {
                last_price
            } else {
                yesterday_price
            };

            tickers.push(Ticker {
                symbol: price.symbol.clone(),
                last_price,
                mark_price,
                index_price,
                best_bid_price: last_price, // Approximation
                best_bid_qty: Decimal::ZERO,
                best_ask_price: last_price, // Approximation
                best_ask_qty: Decimal::ZERO,
                volume_24h,
                turnover_24h: Decimal::ZERO,
                open_interest,
                open_interest_notional: open_interest * mark_price,
                price_change_24h,
                price_change_pct,
                high_price_24h,
                low_price_24h,
                timestamp: Utc.timestamp_millis_opt(price.timestamp).unwrap(),
            });
        }

        Ok(tickers)
    }

    async fn get_orderbook(&self, symbol: &str, _depth: u32) -> Result<Orderbook> {
        let exchange_symbol = self.parse_symbol(symbol);
        // Pacifica uses agg_level parameter for orderbook depth
        let orderbook: PacificaOrderbookData = self
            .get(&format!("/book?symbol={}&agg_level=20", exchange_symbol))
            .await?;

        // Parse nested orderbook structure
        // l[0] = bids, l[1] = asks
        let bids = if orderbook.l.is_empty() {
            Vec::new()
        } else {
            orderbook.l[0]
                .iter()
                .map(|level| {
                    Ok(OrderbookLevel {
                        price: Decimal::from_str(&level.p)?,
                        quantity: Decimal::from_str(&level.a)?,
                    })
                })
                .collect::<Result<Vec<_>>>()?
        };

        let asks = if orderbook.l.len() < 2 {
            Vec::new()
        } else {
            orderbook.l[1]
                .iter()
                .map(|level| {
                    Ok(OrderbookLevel {
                        price: Decimal::from_str(&level.p)?,
                        quantity: Decimal::from_str(&level.a)?,
                    })
                })
                .collect::<Result<Vec<_>>>()?
        };

        Ok(Orderbook {
            symbol: symbol.to_string(),
            bids,
            asks,
            timestamp: Utc.timestamp_millis_opt(orderbook.t).unwrap(),
        })
    }

    async fn get_recent_trades(&self, symbol: &str, _limit: u32) -> Result<Vec<Trade>> {
        let exchange_symbol = self.parse_symbol(symbol);
        let trades: Vec<PacificaTrade> = self
            .get(&format!("/trades?symbol={}", exchange_symbol))
            .await?;

        let trades = trades
            .into_iter()
            .map(|t| {
                // Map Pacifica's position-based side format to Buy/Sell
                let side = match t.side.as_str() {
                    "open_long" | "close_short" => OrderSide::Buy,
                    "open_short" | "close_long" => OrderSide::Sell,
                    _ => OrderSide::Buy, // Default
                };

                Ok(Trade {
                    id: t.created_at.to_string(), // Use timestamp as ID
                    symbol: symbol.to_string(),
                    price: Decimal::from_str(&t.price)?,
                    quantity: Decimal::from_str(&t.amount)?,
                    side,
                    timestamp: Utc.timestamp_millis_opt(t.created_at).unwrap(),
                })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(trades)
    }

    async fn get_funding_rate(&self, symbol: &str) -> Result<FundingRate> {
        // Funding rate is included in the prices endpoint
        let prices: Vec<PacificaPrice> = self.get("/info/prices").await?;

        let price = prices
            .into_iter()
            .find(|p| p.symbol == symbol)
            .ok_or_else(|| anyhow!("Price data for {} not found", symbol))?;

        let funding_rate = Decimal::from_str(&price.funding)?;
        let next_funding_rate = Decimal::from_str(&price.next_funding)?;

        // Estimate next funding time (8 hours from now, standard for perpetuals)
        let next_funding_time = Utc::now() + chrono::Duration::hours(8);

        Ok(FundingRate {
            symbol: symbol.to_string(),
            funding_rate,
            predicted_rate: next_funding_rate,
            funding_time: Utc.timestamp_millis_opt(price.timestamp).unwrap(),
            next_funding_time,
            funding_interval: 8, // 8 hours (standard)
            funding_rate_cap_floor: Decimal::ZERO, // Not provided
        })
    }

    async fn get_funding_rate_history(
        &self,
        symbol: &str,
        _start_time: Option<DateTime<Utc>>,
        _end_time: Option<DateTime<Utc>>,
        _limit: Option<u32>,
    ) -> Result<Vec<FundingRate>> {
        // Pacifica has a funding rate history endpoint, but we need to verify params
        // For now, return error indicating it needs implementation
        Err(anyhow!(
            "get_funding_rate_history for {} is not yet fully implemented for Pacifica (endpoint structure needs verification)",
            symbol
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
        start_time: Option<DateTime<Utc>>,
        end_time: Option<DateTime<Utc>>,
        _limit: Option<u32>,
    ) -> Result<Vec<Kline>> {
        let exchange_symbol = self.parse_symbol(symbol);

        // Build endpoint with query parameters
        let mut endpoint = format!("/kline?symbol={}&interval={}", exchange_symbol, interval);

        if let Some(start) = start_time {
            endpoint.push_str(&format!("&start_time={}", start.timestamp_millis()));
        }
        if let Some(end) = end_time {
            endpoint.push_str(&format!("&end_time={}", end.timestamp_millis()));
        }

        let klines: Vec<PacificaKline> = self.get(&endpoint).await?;

        // Calculate interval duration in seconds for close_time
        let interval_seconds = match interval {
            "1m" => 60,
            "5m" => 300,
            "15m" => 900,
            "30m" => 1800,
            "1h" => 3600,
            "4h" => 14400,
            "1d" => 86400,
            _ => 3600, // Default to 1 hour
        };

        let klines = klines
            .into_iter()
            .map(|k| {
                let open_time = Utc.timestamp_millis_opt(k.t).unwrap();
                let close_time = open_time + chrono::Duration::seconds(interval_seconds);

                Ok(Kline {
                    symbol: symbol.to_string(),
                    interval: interval.to_string(),
                    open_time,
                    close_time,
                    open: Decimal::from_str(&k.o)?,
                    high: Decimal::from_str(&k.h)?,
                    low: Decimal::from_str(&k.l)?,
                    close: Decimal::from_str(&k.c)?,
                    volume: Decimal::from_str(&k.v)?,
                    turnover: Decimal::ZERO, // Calculate if needed
                })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(klines)
    }

    async fn is_supported(&self, symbol: &str) -> Result<bool> {
        self.ensure_cache_initialized().await?;
        Ok(self.symbols_cache.contains(symbol).await)
    }

    async fn get_market_stats(&self, symbol: &str) -> Result<MarketStats> {
        let ticker = self.get_ticker(symbol).await?;
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
            open_interest: ticker.open_interest,
            funding_rate: funding.funding_rate,
            timestamp: Utc::now(),
        })
    }

    async fn get_all_market_stats(&self) -> Result<Vec<MarketStats>> {
        let tickers = self.get_all_tickers().await?;
        let mut stats = Vec::new();

        for ticker in tickers {
            match self.get_funding_rate(&ticker.symbol).await {
                Ok(funding) => {
                    stats.push(MarketStats {
                        symbol: ticker.symbol.clone(),
                        last_price: ticker.last_price,
                        mark_price: ticker.mark_price,
                        index_price: ticker.index_price,
                        high_price_24h: ticker.high_price_24h,
                        low_price_24h: ticker.low_price_24h,
                        volume_24h: ticker.volume_24h,
                        turnover_24h: ticker.turnover_24h,
                        price_change_24h: ticker.price_change_24h,
                        price_change_pct: ticker.price_change_pct,
                        open_interest: ticker.open_interest,
                        funding_rate: funding.funding_rate,
                        timestamp: Utc::now(),
                    });
                }
                Err(e) => {
                    tracing::warn!("Failed to get funding rate for {}: {}", ticker.symbol, e);
                }
            }
        }

        Ok(stats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_symbol() {
        let client = PacificaClient::default();
        assert_eq!(client.parse_symbol("btc"), "BTC");
        assert_eq!(client.parse_symbol("BTC"), "BTC");
        assert_eq!(client.parse_symbol("eth"), "ETH");
    }

    #[tokio::test]
    async fn test_get_markets() {
        let client = PacificaClient::default();
        let markets = client.get_markets().await;
        assert!(markets.is_ok(), "Failed to get markets: {:?}", markets.err());
        let markets = markets.unwrap();
        assert!(!markets.is_empty(), "Markets list should not be empty");
    }

    #[tokio::test]
    async fn test_get_ticker() {
        let client = PacificaClient::default();
        let ticker = client.get_ticker("BTC").await;
        assert!(ticker.is_ok(), "Failed to get ticker: {:?}", ticker.err());
        let ticker = ticker.unwrap();
        assert_eq!(ticker.symbol, "BTC");
        assert!(ticker.last_price > Decimal::ZERO);
    }

    #[tokio::test]
    async fn test_get_orderbook() {
        let client = PacificaClient::default();
        let orderbook = client.get_orderbook("BTC", 10).await;
        assert!(orderbook.is_ok(), "Failed to get orderbook: {:?}", orderbook.err());
        let orderbook = orderbook.unwrap();
        assert_eq!(orderbook.symbol, "BTC");
        assert!(!orderbook.bids.is_empty());
        assert!(!orderbook.asks.is_empty());
    }
}
