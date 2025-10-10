use crate::bybit::types::*;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use perps_core::types::*;
use perps_core::IPerps;
use rust_decimal::Decimal;
use std::str::FromStr;

const BASE_URL: &str = "https://api.bybit.com";

/// A client for the Bybit exchange.
#[derive(Clone)]
pub struct BybitClient {
    http: reqwest::Client,
}

impl BybitClient {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::new(),
        }
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, endpoint: &str) -> Result<T> {
        let url = format!("{}{}", BASE_URL, endpoint);
        let response = self.http.get(&url).send().await?;
        if !response.status().is_success() {
            return Err(anyhow!(
                "GET request to {} failed with status: {}",
                url,
                response.status()
            ));
        }
        let wrapper: BybitResponse<T> = response.json().await?;
        if wrapper.ret_code != 0 {
            return Err(anyhow!(
                "Bybit API error: code {} - {}",
                wrapper.ret_code,
                wrapper.ret_msg
            ));
        }
        Ok(wrapper.result)
    }
}

impl Default for BybitClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IPerps for BybitClient {
    fn get_name(&self) -> &str {
        "bybit"
    }

    fn parse_symbol(&self, symbol: &str) -> String {
        // Convert BTC -> BTCUSDT, ETH -> ETHUSDT
        format!("{}USDT", symbol.to_uppercase())
    }

    async fn get_markets(&self) -> Result<Vec<Market>> {
        let result: InstrumentsResult = self
            .get("/v5/market/instruments-info?category=linear")
            .await?;

        let markets = result
            .list
            .into_iter()
            .filter(|i| i.status == "Trading")
            .map(|i| {
                let tick_size = Decimal::from_str(&i.price_filter.tick_size).unwrap_or(Decimal::new(1, 2));
                let qty_step = Decimal::from_str(&i.lot_size_filter.qty_step).unwrap_or(Decimal::new(1, 3));

                let price_scale = if tick_size > Decimal::ZERO {
                    tick_size.scale() as i32
                } else {
                    2
                };
                let quantity_scale = if qty_step > Decimal::ZERO {
                    qty_step.scale() as i32
                } else {
                    3
                };

                Market {
                    symbol: i.symbol.clone(),
                    contract: i.symbol,
                    price_scale,
                    quantity_scale,
                    min_order_qty: Decimal::from_str(&i.lot_size_filter.min_order_qty).unwrap_or_default(),
                    contract_size: Decimal::ONE,
                    max_order_qty: Decimal::from_str(&i.lot_size_filter.max_order_qty).unwrap_or_default(),
                    min_order_value: Decimal::from_str(&i.lot_size_filter.min_notional_value).unwrap_or_default(),
                    max_leverage: Decimal::from_str(&i.leverage_filter.max_leverage).unwrap_or_default(),
                }
            })
            .collect();
        Ok(markets)
    }

    async fn get_market(&self, symbol: &str) -> Result<Market> {
        let result: InstrumentsResult = self
            .get(&format!("/v5/market/instruments-info?category=linear&symbol={}", symbol))
            .await?;

        let instrument = result
            .list
            .into_iter()
            .find(|i| i.symbol == symbol)
            .ok_or_else(|| anyhow!("Market {} not found", symbol))?;

        let tick_size = Decimal::from_str(&instrument.price_filter.tick_size).unwrap_or(Decimal::new(1, 2));
        let qty_step = Decimal::from_str(&instrument.lot_size_filter.qty_step).unwrap_or(Decimal::new(1, 3));

        let price_scale = if tick_size > Decimal::ZERO {
            tick_size.scale() as i32
        } else {
            2
        };
        let quantity_scale = if qty_step > Decimal::ZERO {
            qty_step.scale() as i32
        } else {
            3
        };

        Ok(Market {
            symbol: instrument.symbol.clone(),
            contract: instrument.symbol,
            price_scale,
            quantity_scale,
            min_order_qty: Decimal::from_str(&instrument.lot_size_filter.min_order_qty).unwrap_or_default(),
            contract_size: Decimal::ONE,
            max_order_qty: Decimal::from_str(&instrument.lot_size_filter.max_order_qty).unwrap_or_default(),
            min_order_value: Decimal::from_str(&instrument.lot_size_filter.min_notional_value).unwrap_or_default(),
            max_leverage: Decimal::from_str(&instrument.leverage_filter.max_leverage).unwrap_or_default(),
        })
    }

    async fn get_orderbook(&self, symbol: &str, depth: u32) -> Result<Orderbook> {
        let result: OrderbookResult = self
            .get(&format!(
                "/v5/market/orderbook?category=linear&symbol={}&limit={}",
                symbol, depth
            ))
            .await?;

        let bids = result
            .b
            .into_iter()
            .map(|(price, quantity)| {
                Ok(OrderbookLevel {
                    price: Decimal::from_str(&price)?,
                    quantity: Decimal::from_str(&quantity)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let asks = result
            .a
            .into_iter()
            .map(|(price, quantity)| {
                Ok(OrderbookLevel {
                    price: Decimal::from_str(&price)?,
                    quantity: Decimal::from_str(&quantity)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(Orderbook {
            symbol: result.s,
            bids,
            asks,
            timestamp: Utc.timestamp_millis_opt(result.ts).unwrap(),
        })
    }

    async fn get_recent_trades(&self, symbol: &str, limit: u32) -> Result<Vec<Trade>> {
        let result: TradesResult = self
            .get(&format!(
                "/v5/market/recent-trade?category=linear&symbol={}&limit={}",
                symbol, limit
            ))
            .await?;

        let trades = result
            .list
            .into_iter()
            .map(|t| {
                Ok(Trade {
                    id: t.exec_id,
                    symbol: t.symbol.clone(),
                    price: Decimal::from_str(&t.price)?,
                    quantity: Decimal::from_str(&t.size)?,
                    side: if t.side == "Buy" {
                        OrderSide::Buy
                    } else {
                        OrderSide::Sell
                    },
                    timestamp: Utc
                        .timestamp_millis_opt(t.time.parse::<i64>()?)
                        .unwrap(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(trades)
    }

    async fn get_funding_rate(&self, symbol: &str) -> Result<FundingRate> {
        // Get current funding rate from ticker
        let result: TickersResult = self
            .get(&format!("/v5/market/tickers?category=linear&symbol={}", symbol))
            .await?;

        let ticker = result
            .list
            .into_iter()
            .find(|t| t.symbol == symbol)
            .ok_or_else(|| anyhow!("Ticker for {} not found", symbol))?;

        let funding_rate = Decimal::from_str(&ticker.funding_rate)?;
        let next_funding_time = ticker.next_funding_time.parse::<i64>()?;

        // Get instrument info for funding interval
        let instrument_result: InstrumentsResult = self
            .get(&format!("/v5/market/instruments-info?category=linear&symbol={}", symbol))
            .await?;
        let instrument = instrument_result
            .list
            .into_iter()
            .find(|i| i.symbol == symbol)
            .ok_or_else(|| anyhow!("Instrument {} not found", symbol))?;

        Ok(FundingRate {
            symbol: symbol.to_string(),
            funding_rate,
            funding_time: Utc::now(),
            predicted_rate: Decimal::ZERO,
            next_funding_time: Utc.timestamp_millis_opt(next_funding_time).unwrap(),
            funding_interval: (instrument.funding_interval / 60) as i32,
            funding_rate_cap_floor: Decimal::ZERO,
        })
    }

    async fn get_klines(
        &self,
        symbol: &str,
        interval: &str,
        start_time: Option<chrono::DateTime<chrono::Utc>>,
        end_time: Option<chrono::DateTime<chrono::Utc>>,
        limit: Option<u32>,
    ) -> Result<Vec<Kline>> {
        let bybit_interval = match interval {
            "1m" => "1",
            "5m" => "5",
            "15m" => "15",
            "1h" => "60",
            "4h" => "240",
            "1d" => "D",
            _ => "60",
        };

        let mut endpoint = format!(
            "/v5/market/kline?category=linear&symbol={}&interval={}",
            symbol, bybit_interval
        );

        if let Some(start) = start_time {
            endpoint.push_str(&format!("&start={}", start.timestamp_millis()));
        }
        if let Some(end) = end_time {
            endpoint.push_str(&format!("&end={}", end.timestamp_millis()));
        }
        if let Some(lim) = limit {
            endpoint.push_str(&format!("&limit={}", lim));
        }

        let result: KlinesResult = self.get(&endpoint).await?;

        let klines = result
            .list
            .into_iter()
            .map(|k| {
                Ok(Kline {
                    symbol: symbol.to_string(),
                    interval: interval.to_string(),
                    open_time: Utc.timestamp_millis_opt(k[0].parse::<i64>()?).unwrap(),
                    close_time: Utc.timestamp_millis_opt(k[0].parse::<i64>()? + 60000).unwrap(),
                    open: Decimal::from_str(&k[1])?,
                    high: Decimal::from_str(&k[2])?,
                    low: Decimal::from_str(&k[3])?,
                    close: Decimal::from_str(&k[4])?,
                    volume: Decimal::from_str(&k[5])?,
                    turnover: Decimal::from_str(&k[6])?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(klines)
    }

    async fn is_supported(&self, symbol: &str) -> Result<bool> {
        let markets = self.get_markets().await?;
        Ok(markets.iter().any(|m| m.symbol == symbol))
    }

    async fn get_ticker(&self, symbol: &str) -> Result<Ticker> {
        let result: TickersResult = self
            .get(&format!("/v5/market/tickers?category=linear&symbol={}", symbol))
            .await?;

        let ticker = result
            .list
            .into_iter()
            .find(|t| t.symbol == symbol)
            .ok_or_else(|| anyhow!("Ticker for {} not found", symbol))?;

        let last_price = Decimal::from_str(&ticker.last_price)?;
        let mark_price = Decimal::from_str(&ticker.mark_price)?;
        let index_price = Decimal::from_str(&ticker.index_price)?;
        let best_bid_price = Decimal::from_str(&ticker.bid1_price)?;
        let best_bid_qty = Decimal::from_str(&ticker.bid1_size)?;
        let best_ask_price = Decimal::from_str(&ticker.ask1_price)?;
        let best_ask_qty = Decimal::from_str(&ticker.ask1_size)?;
        let volume_24h = Decimal::from_str(&ticker.volume_24h)?;
        let turnover_24h = Decimal::from_str(&ticker.turnover_24h)?;
        let prev_price_24h = Decimal::from_str(&ticker.prev_price_24h)?;
        let price_change_24h = last_price - prev_price_24h;
        let price_change_pct = Decimal::from_str(&ticker.price_24h_pcnt)?;
        let high_price_24h = Decimal::from_str(&ticker.high_price_24h)?;
        let low_price_24h = Decimal::from_str(&ticker.low_price_24h)?;

        Ok(Ticker {
            symbol: ticker.symbol,
            last_price,
            mark_price,
            index_price,
            best_bid_price,
            best_bid_qty,
            best_ask_price,
            best_ask_qty,
            timestamp: Utc::now(),
            volume_24h,
            turnover_24h,
            price_change_24h,
            price_change_pct,
            high_price_24h,
            low_price_24h,
        })
    }

    async fn get_all_tickers(&self) -> Result<Vec<Ticker>> {
        let result: TickersResult = self.get("/v5/market/tickers?category=linear").await?;

        let tickers = result
            .list
            .into_iter()
            .filter_map(|ticker| {
                let last_price = Decimal::from_str(&ticker.last_price).ok()?;
                let mark_price = Decimal::from_str(&ticker.mark_price).ok()?;
                let index_price = Decimal::from_str(&ticker.index_price).ok()?;
                let best_bid_price = Decimal::from_str(&ticker.bid1_price).ok()?;
                let best_bid_qty = Decimal::from_str(&ticker.bid1_size).ok()?;
                let best_ask_price = Decimal::from_str(&ticker.ask1_price).ok()?;
                let best_ask_qty = Decimal::from_str(&ticker.ask1_size).ok()?;
                let volume_24h = Decimal::from_str(&ticker.volume_24h).ok()?;
                let turnover_24h = Decimal::from_str(&ticker.turnover_24h).ok()?;
                let prev_price_24h = Decimal::from_str(&ticker.prev_price_24h).ok()?;
                let price_change_24h = last_price - prev_price_24h;
                let price_change_pct = Decimal::from_str(&ticker.price_24h_pcnt).ok()?;
                let high_price_24h = Decimal::from_str(&ticker.high_price_24h).ok()?;
                let low_price_24h = Decimal::from_str(&ticker.low_price_24h).ok()?;

                Some(Ticker {
                    symbol: ticker.symbol,
                    last_price,
                    mark_price,
                    index_price,
                    best_bid_price,
                    best_bid_qty,
                    best_ask_price,
                    best_ask_qty,
                    timestamp: Utc::now(),
                    volume_24h,
                    turnover_24h,
                    price_change_24h,
                    price_change_pct,
                    high_price_24h,
                    low_price_24h,
                })
            })
            .collect();
        Ok(tickers)
    }

    async fn get_funding_rate_history(
        &self,
        symbol: &str,
        start_time: Option<chrono::DateTime<chrono::Utc>>,
        end_time: Option<chrono::DateTime<chrono::Utc>>,
        limit: Option<u32>,
    ) -> Result<Vec<FundingRate>> {
        let mut endpoint = format!(
            "/v5/market/funding/history?category=linear&symbol={}",
            symbol
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

        let result: FundingHistoryResult = self.get(&endpoint).await?;

        let funding_rates = result
            .list
            .into_iter()
            .map(|fr| {
                Ok(FundingRate {
                    symbol: fr.symbol.clone(),
                    funding_rate: Decimal::from_str(&fr.funding_rate)?,
                    funding_time: Utc
                        .timestamp_millis_opt(fr.funding_rate_timestamp.parse::<i64>()?)
                        .unwrap(),
                    predicted_rate: Decimal::ZERO,
                    next_funding_time: Utc.timestamp_millis_opt(0).unwrap(),
                    funding_interval: 0,
                    funding_rate_cap_floor: Decimal::ZERO,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(funding_rates)
    }

    async fn get_open_interest(&self, symbol: &str) -> Result<OpenInterest> {
        // Get from ticker which includes open interest
        let result: TickersResult = self
            .get(&format!("/v5/market/tickers?category=linear&symbol={}", symbol))
            .await?;

        let ticker = result
            .list
            .into_iter()
            .find(|t| t.symbol == symbol)
            .ok_or_else(|| anyhow!("Ticker for {} not found", symbol))?;

        let open_interest = Decimal::from_str(&ticker.open_interest)?;
        let open_value = Decimal::from_str(&ticker.open_interest_value)?;

        Ok(OpenInterest {
            symbol: symbol.to_string(),
            open_interest,
            open_value,
            timestamp: Utc::now(),
        })
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
        let tickers = self.get_all_tickers().await?;
        let mut stats = Vec::new();

        for ticker in tickers {
            match self.get_market_stats(&ticker.symbol).await {
                Ok(stat) => stats.push(stat),
                Err(e) => tracing::warn!("Failed to get stats for {}: {}", ticker.symbol, e),
            }
        }

        Ok(stats)
    }
}
