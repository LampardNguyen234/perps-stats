use crate::cache::SymbolsCache;
use crate::kucoin::types::*;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use perps_core::types::*;
use perps_core::IPerps;
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;
use std::str::FromStr;

const BASE_URL: &str = "https://api-futures.kucoin.com";

/// A client for the KuCoin Futures exchange.
#[derive(Clone)]
pub struct KucoinClient {
    http: reqwest::Client,
    /// Cached set of supported symbols
    symbols_cache: SymbolsCache,
}

impl KucoinClient {
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
}

impl Default for KucoinClient {
    fn default() -> Self {
        Self::new()
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
                let quantity_scale = if c.lot_size > 0 {
                    0
                } else {
                    3
                };

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
        let quantity_scale = if contract.lot_size > 0 {
            0
        } else {
            3
        };

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

    async fn get_orderbook(&self, symbol: &str, _depth: u32) -> Result<Orderbook> {
        let response: KucoinOrderbook = self
            .get(&format!("/api/v1/level2/snapshot?symbol={}", symbol))
            .await?;

        let bids = response
            .bids
            .into_iter()
            .map(|(price, quantity)| {
                Ok(OrderbookLevel {
                    price: Decimal::from_f64(price).unwrap_or(Decimal::ZERO),
                    quantity: Decimal::from(quantity),
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let asks = response
            .asks
            .into_iter()
            .map(|(price, quantity)| {
                Ok(OrderbookLevel {
                    price: Decimal::from_f64(price).unwrap_or(Decimal::ZERO),
                    quantity: Decimal::from(quantity),
                })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(Orderbook {
            symbol: response.symbol,
            bids,
            asks,
            timestamp: Utc.timestamp_nanos(response.timestamp),
        })
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
                    tracing::warn!("Invalid kline data format for KuCoin: expected 7 fields, got {}", k.len());
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
        Ok(self.symbols_cache.contains(symbol).await)
    }

    async fn get_ticker(&self, symbol: &str) -> Result<Ticker> {
        // Get detailed contract info for this specific symbol (includes 24h stats)
        let contract: KucoinContractDetail = self
            .get(&format!("/api/v1/contracts/{}", symbol))
            .await?;

        // Get ticker for real-time best bid/ask
        let ticker: KucoinTicker = self.get(&format!("/api/v1/ticker?symbol={}", symbol)).await?;

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
                tracing::debug!("KuCoin {} mark_price is None, using last_price fallback", symbol);
                last_price
            });
        let index_price = contract
            .index_price
            .and_then(Decimal::from_f64)
            .unwrap_or_else(|| {
                tracing::debug!("KuCoin {} index_price is None, using last_price fallback", symbol);
                last_price
            });

        let best_bid_price = Decimal::from_str(&ticker.best_bid_price)
            .unwrap_or(Decimal::ZERO);
        let best_bid_qty = Decimal::from(ticker.best_bid_size);
        let best_ask_price = Decimal::from_str(&ticker.best_ask_price)
            .unwrap_or(Decimal::ZERO);
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
        Err(anyhow!("get_funding_rate_history is not implemented for KuCoin yet"))
    }

    async fn get_open_interest(&self, symbol: &str) -> Result<OpenInterest> {
        // Try to get from active contracts list
        let contracts: Vec<KucoinContractDetail> = self.get("/api/v1/contracts/active").await?;
        let contract = contracts
            .into_iter()
            .find(|c| c.symbol == symbol)
            .ok_or_else(|| anyhow!("Contract {} not found", symbol))?;

        let open_interest = contract
            .open_interest
            .as_ref()
            .and_then(|oi| Decimal::from_str(oi).ok())
            .unwrap_or(Decimal::ZERO);

        Ok(OpenInterest {
            symbol: symbol.to_string(),
            open_interest,
            open_value: Decimal::ZERO,
            timestamp: Utc::now(),
        })
    }

    async fn get_market_stats(&self, _symbol: &str) -> Result<MarketStats> {
        Err(anyhow!("get_market_stats is not available from KuCoin public API"))
    }

    async fn get_all_market_stats(&self) -> Result<Vec<MarketStats>> {
        Err(anyhow!("get_all_market_stats is not available from KuCoin public API"))
    }
}
