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
}

impl ParadexClient {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::new(),
        }
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
        let response: OrderbookResponse = self.get(&format!("/orderbook/{}", symbol)).await?;
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
        let resolution = match interval {
            "1m" => "60",
            "5m" => "300",
            "15m" => "900",
            "1h" => "3600",
            _ => "60", // Default to 1m
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
                close_time: Utc.timestamp_millis_opt(k.0 as i64 + (resolution.parse::<i64>().unwrap_or(60) * 1000)).unwrap(), // Approximate close time
                open: Decimal::from_str(&k.1)?,
                high: Decimal::from_str(&k.2)?,
                low: Decimal::from_str(&k.3)?,
                close: Decimal::from_str(&k.4)?,
                volume: Decimal::from_str(&k.5)?,
                turnover: Decimal::ZERO, // Not available
            }))
            .collect::<Result<Vec<_>>>()?;
        Ok(klines)
    }

    async fn is_supported(&self, symbol: &str) -> Result<bool> {
        let markets = self.get_markets().await?;
        Ok(markets.iter().any(|m| m.symbol == symbol))
    }

    // --- Partial Implementations using BBO ---

    async fn get_ticker(&self, symbol: &str) -> Result<Ticker> {
        // Use BBO endpoint to construct a simplified ticker
        let response: BboResponse = self.get(&format!("/bbo/{}", symbol)).await?;
        Ok(Ticker {
            symbol: response.market.clone(),
            last_price: Decimal::from_str(&response.bid)?,
            mark_price: Decimal::from_str(&response.bid)?,
            index_price: Decimal::ZERO,
            best_bid_price: Decimal::from_str(&response.bid)?,
            best_bid_qty: Decimal::from_str(&response.bid_size)?,
            best_ask_price: Decimal::from_str(&response.ask)?,
            best_ask_qty: Decimal::from_str(&response.ask_size)?,
            timestamp: Utc.timestamp_millis_opt(response.last_updated_at as i64).unwrap(),
            // Fields not available from BBO
            volume_24h: Decimal::ZERO,
            turnover_24h: Decimal::ZERO,
            price_change_24h: Decimal::ZERO,
            price_change_pct: Decimal::ZERO,
            high_price_24h: Decimal::ZERO,
            low_price_24h: Decimal::ZERO,
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
        unimplemented!("get_market_stats is not available from a public Paradex endpoint.")
    }

    async fn get_all_market_stats(&self) -> Result<Vec<MarketStats>> {
        unimplemented!("get_all_market_stats is not available from a public Paradex endpoint.")
    }
}
