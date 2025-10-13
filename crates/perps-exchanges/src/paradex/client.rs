use crate::cache::SymbolsCache;
use crate::paradex::types::*;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use perps_core::types::*;
use perps_core::IPerps;
use rust_decimal::Decimal;
use std::str::FromStr;

const BASE_URL: &str = "https://api.prod.paradex.trade/v1";

/// A client for the Paradex exchange.
#[derive(Clone)]
pub struct ParadexClient {
    http: reqwest::Client,
    /// Cached set of supported symbols
    symbols_cache: SymbolsCache,
}

impl ParadexClient {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::new(),
            symbols_cache: SymbolsCache::new(),
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

    async fn get<T: serde::de::DeserializeOwned>(&self, endpoint: &str) -> Result<T> {
        let url = format!("{}{}", BASE_URL, endpoint);
        let response = self.http.get(&url).send().await?;
        if !response.status().is_success() {
            return Err(anyhow!("GET request to {} failed with status: {}", url, response.status()));
        }
        let data = response.json().await?;
        Ok(data)
    }
}

impl Default for ParadexClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IPerps for ParadexClient {
    fn get_name(&self) -> &str {
        "paradex"
    }

    fn parse_symbol(&self, symbol: &str) -> String {
        format!("{}-USD-PERP", symbol.to_uppercase())
    }

    async fn get_markets(&self) -> Result<Vec<Market>> {
        let response: MarketsResponse = self.get("/markets").await?;
        let markets = response
            .results
            .into_iter()
            .map(|m| {
                let price_tick_size = Decimal::from_str(&m.price_tick_size).unwrap_or(Decimal::new(1, 2));
                let order_size_increment = Decimal::from_str(&m.order_size_increment).unwrap_or(Decimal::new(1, 3));

                // Calculate precision from tick size
                let price_scale = if price_tick_size > Decimal::ZERO {
                    price_tick_size.scale() as i32
                } else {
                    2
                };
                let quantity_scale = if order_size_increment > Decimal::ZERO {
                    order_size_increment.scale() as i32
                } else {
                    3
                };

                Market {
                    symbol: m.symbol.clone(),
                    contract: m.symbol,
                    price_scale,
                    quantity_scale,
                    min_order_qty: order_size_increment,
                    // Fields not provided by the API
                    contract_size: Decimal::ONE,
                    max_order_qty: Decimal::ZERO,
                    min_order_value: Decimal::from_str(&m.min_notional).unwrap_or_default(),
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

    async fn get_orderbook(&self, symbol: &str, _depth: u32) -> Result<Orderbook> {
        let response: OrderbookResponse = self.get(&format!("/orderbook/{}?depth=100", symbol)).await?;
        let bids = response
            .bids
            .into_iter()
            .map(|(price, quantity)| Ok(OrderbookLevel { price: Decimal::from_str(&price)?, quantity: Decimal::from_str(&quantity)? }))
            .collect::<Result<Vec<_>>>()?;
        let asks = response
            .asks
            .into_iter()
            .map(|(price, quantity)| Ok(OrderbookLevel { price: Decimal::from_str(&price)?, quantity: Decimal::from_str(&quantity)? }))
            .collect::<Result<Vec<_>>>()?;

        Ok(Orderbook {
            symbol: response.market,
            bids,
            asks,
            timestamp: Utc.timestamp_millis_opt(response.last_updated_at as i64).unwrap(),
        })
    }

    async fn get_recent_trades(&self, symbol: &str, _limit: u32) -> Result<Vec<Trade>> {
        let response: TradesResponse = self.get(&format!("/trades?symbol={}", symbol)).await?;
        let trades = response
            .trades
            .into_iter()
            .map(|t| Ok(Trade {
                id: t.timestamp.to_string(), // No unique trade ID provided
                symbol: symbol.to_string(),
                price: Decimal::from_str(&t.price)?,
                quantity: Decimal::from_str(&t.size)?,
                side: if t.side == "buy" { OrderSide::Buy } else { OrderSide::Sell },
                timestamp: Utc.timestamp_opt(t.timestamp, 0).unwrap(),
            }))
            .collect::<Result<Vec<_>>>()?;
        Ok(trades)
    }

    async fn get_funding_rate(&self, symbol: &str) -> Result<FundingRate> {
        let response: FundingRateResponse = self.get(&format!("/funding/data?market={}", symbol)).await?;
        let rate = response.results.first().ok_or_else(|| anyhow!("No funding rate data found for {}", symbol))?;
        Ok(FundingRate {
            symbol: rate.market.clone(),
            funding_rate: Decimal::from_str(&rate.funding_rate)?,
            funding_time: Utc.timestamp_millis_opt(rate.created_at as i64).unwrap(),
            // Fields not provided by the API
            predicted_rate: Decimal::ZERO,
            next_funding_time: Utc.timestamp_millis_opt(0).unwrap(),
            funding_interval: 0,
            funding_rate_cap_floor: Decimal::ZERO,
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
        // Paradex expects resolution in minutes: [1, 3, 5, 15, 30, 60]
        let (resolution, resolution_seconds) = match interval {
            "1m" => ("1", 60),
            "3m" => ("3", 180),
            "5m" => ("5", 300),
            "15m" => ("15", 900),
            "30m" => ("30", 1800),
            "1h" => ("60", 3600),
            _ => ("1", 60), // Default to 1m
        };
        let start = start_time.unwrap_or_else(|| Utc::now() - chrono::Duration::days(1)).timestamp_millis();
        let end = end_time.unwrap_or_else(Utc::now).timestamp_millis();
        let endpoint = format!("/markets/klines?symbol={}&resolution={}&start_at={}&end_at={}", symbol, resolution, start, end);
        let response: KlinesResponse = self.get(&endpoint).await?;
        let klines = response
            .results
            .into_iter()
            .map(|k| Ok(Kline {
                symbol: symbol.to_string(),
                interval: interval.to_string(),
                open_time: Utc.timestamp_millis_opt(k.0 as i64).unwrap(),
                close_time: Utc.timestamp_millis_opt(k.0 as i64 + (resolution_seconds * 1000)).unwrap(),
                open: Decimal::try_from(k.1).unwrap_or(Decimal::ZERO),
                high: Decimal::try_from(k.2).unwrap_or(Decimal::ZERO),
                low: Decimal::try_from(k.3).unwrap_or(Decimal::ZERO),
                close: Decimal::try_from(k.4).unwrap_or(Decimal::ZERO),
                volume: Decimal::try_from(k.5).unwrap_or(Decimal::ZERO),
                turnover: Decimal::ZERO, // Not available
            }))
            .collect::<Result<Vec<_>>>()?;
        Ok(klines)
    }

    async fn is_supported(&self, symbol: &str) -> Result<bool> {
        self.ensure_cache_initialized().await?;
        Ok(self.symbols_cache.contains(symbol).await)
    }

    // --- Partial Implementations using BBO ---

    async fn get_ticker(&self, symbol: &str) -> Result<Ticker> {
        // Fetch market summary for 24h statistics
        let summary_response: MarketSummaryResponse = self.get(&format!("/markets/summary?market={}", symbol)).await?;
        let summary = summary_response.results.first().ok_or_else(|| anyhow!("No summary data found for {}", symbol))?;

        // Fetch BBO for best bid/ask with quantities
        let bbo: BboResponse = self.get(&format!("/bbo/{}", symbol)).await?;

        // Parse prices
        let last_price = Decimal::from_str(&summary.last_traded_price)?;
        let mark_price = Decimal::from_str(&summary.mark_price)?;
        let index_price = Decimal::from_str(&summary.underlying_price)?;

        // Calculate 24h price change
        let price_change_rate = Decimal::from_str(&summary.price_change_rate_24h)?;
        // price_change_rate is already a percentage (e.g., -0.074055 means -7.4055%)
        // Calculate absolute price change: last_price * price_change_rate / (1 + price_change_rate)
        let price_change_24h = if price_change_rate != Decimal::ZERO {
            last_price * price_change_rate / (Decimal::ONE + price_change_rate)
        } else {
            Decimal::ZERO
        };

        // Convert volume to turnover (volume * average price approximation)
        let volume_24h = Decimal::from_str(&summary.volume_24h)?;
        let turnover_24h = volume_24h; // Paradex volume_24h is already in USD notional

        // Estimate 24h high/low from current price and price change
        // This is an approximation since Paradex doesn't provide high/low directly
        let price_change_abs = price_change_24h.abs();
        let high_price_24h = last_price + price_change_abs;
        let low_price_24h = if last_price > price_change_abs {
            last_price - price_change_abs
        } else {
            Decimal::ZERO
        };

        Ok(Ticker {
            symbol: summary.symbol.clone(),
            last_price,
            mark_price,
            index_price,
            best_bid_price: Decimal::from_str(&bbo.bid)?,
            best_bid_qty: Decimal::from_str(&bbo.bid_size)?,
            best_ask_price: Decimal::from_str(&bbo.ask)?,
            best_ask_qty: Decimal::from_str(&bbo.ask_size)?,
            volume_24h: Decimal::ZERO, // Paradex provides notional volume, not base volume
            turnover_24h,
            price_change_24h,
            price_change_pct: price_change_rate,
            high_price_24h,
            low_price_24h,
            timestamp: Utc.timestamp_millis_opt(summary.created_at as i64).unwrap(),
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

    async fn get_funding_rate_history(&self, _symbol: &str, _start_time: Option<DateTime<Utc>>, _end_time: Option<DateTime<Utc>>, _limit: Option<u32>) -> Result<Vec<FundingRate>> {
        unimplemented!("get_funding_rate_history is not implemented for Paradex yet.")
    }

    async fn get_open_interest(&self, symbol: &str) -> Result<OpenInterest> {
        // Paradex doesn't provide open interest via public endpoints
        // Return zero values to maintain compatibility
        Ok(OpenInterest {
            symbol: symbol.to_string(),
            open_interest: Decimal::ZERO,
            open_value: Decimal::ZERO,
            timestamp: Utc::now(),
        })
    }

    async fn get_market_stats(&self, _symbol: &str) -> Result<MarketStats> {
        unimplemented!("get_market_stats is not available from a public Hyperliquid endpoint.")
    }

    async fn get_all_market_stats(&self) -> Result<Vec<MarketStats>> {
        unimplemented!("get_all_market_stats is not available from a public Hyperliquid endpoint.")
    }
}
