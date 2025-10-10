use crate::hyperliquid::types::*;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use perps_core::types::*;
use perps_core::IPerps;
use rust_decimal::Decimal;
use serde_json::Value;
use std::str::FromStr;

const INFO_URL: &str = "https://api.hyperliquid.xyz/info";

/// A client for the Hyperliquid exchange.
#[derive(Clone)]
pub struct HyperliquidClient {
    http: reqwest::Client,
}

impl HyperliquidClient {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::new(),
        }
    }

    async fn post<T: serde::de::DeserializeOwned>(&self, body: Value) -> Result<T> {
        let response = self.http.post(INFO_URL).json(&body).send().await?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_else(|_| "Failed to read error body".to_string());
            return Err(anyhow!("info request failed with status: {}. Body: {}", status, text));
        }
        let data = response.json().await?;
        Ok(data)
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
                price_scale: 0, // Not provided
                quantity_scale: u.sz_decimals as i32,
                min_order_qty: Decimal::ZERO, // Not provided
                max_order_qty: Decimal::ZERO, // Not provided
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

    async fn get_ticker(&self, _symbol: &str) -> Result<Ticker> {
        // Hyperliquid does not have a dedicated ticker endpoint in the same way as other exchanges.
        // This would need to be constructed from multiple sources (e.g., orderbook, trades).
        unimplemented!("Ticker data is not directly available from the Hyperliquid API.")
    }

    async fn get_all_tickers(&self) -> Result<Vec<Ticker>> {
        unimplemented!("Ticker data is not directly available from the Hyperliquid API.")
    }

    async fn get_orderbook(&self, symbol: &str, _depth: u32) -> Result<Orderbook> {
        let body = serde_json::json!({ "type": "l2Book", "coin": symbol });
        let book: L2Book = self.post(body).await?;
        let bids = book
            .levels[0]
            .iter()
            .map(|l| {
                Ok(OrderbookLevel {
                    price: Decimal::from_str(&l.px)?,
                    quantity: Decimal::from_str(&l.sz)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let asks = book
            .levels[1]
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

    async fn get_funding_rate(&self, symbol: &str) -> Result<FundingRate> {
        let body = serde_json::json!({ "type": "fundingHistory", "coin": symbol, "startTime": Utc::now().timestamp_millis() - 1000 * 60 * 60 * 8 });
        let history: Vec<FundingHistory> = self.post(body).await?;
        let last_rate = history.last().ok_or_else(|| anyhow!("No funding history found"))?;
        Ok(FundingRate {
            symbol: symbol.to_string(),
            funding_rate: Decimal::from_str(&last_rate.funding_rate)?,
            predicted_rate: Decimal::ZERO, // Not available
            funding_time: Utc.timestamp_millis_opt(last_rate.time as i64).unwrap(),
            next_funding_time: Utc.timestamp_millis_opt(0).unwrap(), // Not available
            funding_interval: 0, // Not available
            funding_rate_cap_floor: Decimal::ZERO, // Not available
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
        let end = end_time.unwrap_or_else(|| Utc::now());
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

    async fn get_open_interest(&self, _symbol: &str) -> Result<OpenInterest> {
        unimplemented!("Open interest is not directly available from a public endpoint.")
    }

    async fn get_klines(
        &self,
        symbol: &str,
        interval: &str,
        start_time: Option<DateTime<Utc>>,
        _end_time: Option<DateTime<Utc>>,
        _limit: Option<u32>,
    ) -> Result<Vec<Kline>> {
        let body = serde_json::json!({ "type": "candleSnapshot", "coin": symbol, "interval": interval, "startTime": start_time.unwrap().timestamp_millis() });
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
        unimplemented!("Recent trades are not available from a simple public endpoint.")
    }

    async fn get_market_stats(&self, _symbol: &str) -> Result<MarketStats> {
        unimplemented!("Market stats must be constructed from multiple endpoints.")
    }

    async fn get_all_market_stats(&self) -> Result<Vec<MarketStats>> {
        unimplemented!("Market stats must be constructed from multiple endpoints.")
    }

    async fn is_supported(&self, symbol: &str) -> Result<bool> {
        let markets = self.get_markets().await?;
        Ok(markets.iter().any(|m| m.symbol == symbol))
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
        assert!(!orderbook.bids.is_empty());
        assert!(!orderbook.asks.is_empty());
    }

    #[tokio::test]
    async fn test_get_funding_rate() {
        let client = HyperliquidClient::default();
        let funding_rate = client.get_funding_rate("BTC").await.unwrap();
        assert_eq!(funding_rate.symbol, "BTC");
    }
}
