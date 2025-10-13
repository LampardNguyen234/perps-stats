use async_trait::async_trait;
use chrono::Utc;
use futures::future::join_all;
use perps_core::{FundingRate, IPerps, LiquidityDepthStats, OrderSide, Orderbook, Trade};
use rust_decimal::Decimal;
use std::time::Instant;

use crate::types::{FundingRateStats, MarketDepth};

/// IAggregator trait defines business logic for calculating market metrics
#[async_trait]
pub trait IAggregator: Send + Sync {
    /// Computes the percentage price change that would occur
    /// if a trade of a given size were to be executed against the provided order book.
    /// Returns the slippage as a decimal (e.g., 0.01 for 1%).
    async fn calculate_slippage(
        &self,
        book: &Orderbook,
        trade_size: Decimal,
        side: OrderSide,
    ) -> anyhow::Result<Decimal>;

    /// Analyzes an order book to determine the cumulative volume
    /// available at different percentage-based price points away from the mid-price.
    /// This is useful for understanding market liquidity.
    async fn calculate_market_depth(&self, book: &Orderbook) -> anyhow::Result<MarketDepth>;

    /// Computes the average price of an asset over a series of trades, weighted by volume.
    async fn calculate_vwap(&self, trades: &[Trade]) -> anyhow::Result<Decimal>;

    /// Analyzes an order book to determine the cumulative notional value
    /// available at different basis point spreads from the mid-price.
    async fn calculate_liquidity_depth(
        &self,
        book: &Orderbook,
        exchange: &str,
        global_symbol: &str,
    ) -> anyhow::Result<LiquidityDepthStats>;

    /// Calculates liquidity depth for a given symbol across multiple exchanges concurrently.
    async fn calculate_liquidity_depth_all(
        &self,
        exchanges: &[Box<dyn IPerps + Send + Sync>],
        symbol: &str,
    ) -> anyhow::Result<Vec<LiquidityDepthStats>>;

    /// Calculates statistics for funding rates over a time period.
    /// Includes average, min, max, standard deviation, and trend analysis.
    async fn calculate_funding_rate_stats(
        &self,
        rates: &[FundingRate],
        exchange: &str,
    ) -> anyhow::Result<FundingRateStats>;
}

/// Default implementation of the IAggregator trait
pub struct Aggregator;

impl Aggregator {
    pub fn new() -> Self {
        Self
    }
}

impl Default for Aggregator {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IAggregator for Aggregator {
    async fn calculate_slippage(
        &self,
        _book: &Orderbook,
        _trade_size: Decimal,
        _side: OrderSide,
    ) -> anyhow::Result<Decimal> {
        // TODO: Implement slippage calculation logic
        // Walk through the order book to calculate average execution price
        // Compare to mid-price to get slippage percentage
        anyhow::bail!("calculate_slippage not yet implemented")
    }

    async fn calculate_market_depth(&self, _book: &Orderbook) -> anyhow::Result<MarketDepth> {
        // TODO: Implement market depth calculation
        // Calculate cumulative volumes at different percentage levels
        anyhow::bail!("calculate_market_depth not yet implemented")
    }

    async fn calculate_vwap(&self, trades: &[Trade]) -> anyhow::Result<Decimal> {
        if trades.is_empty() {
            anyhow::bail!("cannot calculate VWAP: no trades provided");
        }

        let mut total_volume = Decimal::ZERO;
        let mut weighted_sum = Decimal::ZERO;

        for trade in trades {
            weighted_sum += trade.price * trade.quantity;
            total_volume += trade.quantity;
        }

        if total_volume == Decimal::ZERO {
            anyhow::bail!("cannot calculate VWAP: total volume is zero");
        }

        Ok(weighted_sum / total_volume)
    }

    async fn calculate_liquidity_depth(
        &self,
        book: &Orderbook,
        exchange: &str,
        global_symbol: &str,
    ) -> anyhow::Result<LiquidityDepthStats> {
        let start = Instant::now();
        if book.bids.is_empty() || book.asks.is_empty() {
            anyhow::bail!("Orderbook is empty or one-sided, cannot calculate liquidity depth.");
        }

        let mid_price = (book.bids[0].price + book.asks[0].price) / Decimal::new(2, 0);
        if mid_price.is_zero() {
            anyhow::bail!("Mid price is zero, cannot calculate percentage-based depth.");
        }

        let bps_levels = [
            Decimal::new(1, 4),   // 1 bps = 0.0001
            Decimal::new(25, 5),  // 2.5 bps = 0.00025
            Decimal::new(5, 4),   // 5 bps = 0.0005
            Decimal::new(10, 4),  // 10 bps = 0.001
            Decimal::new(20, 4),  // 20 bps = 0.002
        ];

        let mut bid_notionals = vec![Decimal::ZERO; bps_levels.len()];
        let mut ask_notionals = vec![Decimal::ZERO; bps_levels.len()];

        // Calculate cumulative bid notionals
        for level in &book.bids {
            let price_delta = mid_price - level.price;
            if price_delta < Decimal::ZERO {
                continue;
            } // Should not happen for sorted bids
            let spread_pct = price_delta / mid_price;
            let notional = level.price * level.quantity;

            for (i, &bps) in bps_levels.iter().enumerate() {
                if spread_pct <= bps {
                    bid_notionals[i] += notional;
                }
            }
        }

        // Calculate cumulative ask notionals
        for level in &book.asks {
            let price_delta = level.price - mid_price;
            if price_delta < Decimal::ZERO {
                continue;
            } // Should not happen for sorted asks
            let spread_pct = price_delta / mid_price;
            let notional = level.price * level.quantity;

            for (i, &bps) in bps_levels.iter().enumerate() {
                if spread_pct <= bps {
                    ask_notionals[i] += notional;
                }
            }
        }

        let duration = start.elapsed();
        tracing::debug!(
            "Calculated liquidity for {} on {} in {:.2?}",
            global_symbol,
            exchange,
            duration
        );

        Ok(LiquidityDepthStats {
            timestamp: Utc::now(),
            exchange: exchange.to_string(),
            symbol: global_symbol.to_string(),
            mid_price,
            bid_1bps: bid_notionals[0],
            bid_2_5bps: bid_notionals[1],
            bid_5bps: bid_notionals[2],
            bid_10bps: bid_notionals[3],
            bid_20bps: bid_notionals[4],
            ask_1bps: ask_notionals[0],
            ask_2_5bps: ask_notionals[1],
            ask_5bps: ask_notionals[2],
            ask_10bps: ask_notionals[3],
            ask_20bps: ask_notionals[4],
        })
    }

    async fn calculate_liquidity_depth_all(
        &self,
        exchanges: &[Box<dyn IPerps + Send + Sync>],
        symbol: &str,
    ) -> anyhow::Result<Vec<LiquidityDepthStats>> {
        let futures = exchanges
            .iter()
            .map(|client| {
                let symbol = symbol.to_string();
                async move {
                    let start = Instant::now();
                    let exchange_name = client.get_name();
                    let parsed_symbol = client.parse_symbol(&symbol);
                    let result = match client.get_orderbook(&parsed_symbol, 1000).await {
                        Ok(orderbook) => self.calculate_liquidity_depth(&orderbook, exchange_name, &symbol).await,
                        Err(e) => {
                            tracing::warn!(
                                "Failed to fetch orderbook for {} on {}: {}",
                                symbol,
                                exchange_name,
                                e
                            );
                            Err(e)
                        }
                    };
                    let duration = start.elapsed();
                    tracing::debug!(
                        "Fetched and processed liquidity for {} on {} in {:.2?}",
                        symbol,
                        exchange_name,
                        duration
                    );
                    result
                }
            })
            .collect::<Vec<_>>();

        let results = join_all(futures).await;

        let stats: Vec<LiquidityDepthStats> = results.into_iter().filter_map(Result::ok).collect();

        Ok(stats)
    }

    async fn calculate_funding_rate_stats(
        &self,
        rates: &[FundingRate],
        exchange: &str,
    ) -> anyhow::Result<FundingRateStats> {
        if rates.is_empty() {
            anyhow::bail!("Cannot calculate funding rate stats: no rates provided");
        }

        let symbol = rates[0].symbol.clone();
        let count = rates.len();

        // Calculate average
        let sum: Decimal = rates.iter().map(|r| r.funding_rate).sum();
        let average_rate = sum / Decimal::from(count);

        // Find min and max
        let mut min_rate = rates[0].funding_rate;
        let mut max_rate = rates[0].funding_rate;
        for rate in rates.iter().skip(1) {
            if rate.funding_rate < min_rate {
                min_rate = rate.funding_rate;
            }
            if rate.funding_rate > max_rate {
                max_rate = rate.funding_rate;
            }
        }

        // Calculate standard deviation
        let variance_sum: Decimal = rates
            .iter()
            .map(|r| {
                let diff = r.funding_rate - average_rate;
                diff * diff
            })
            .sum();
        let variance = variance_sum / Decimal::from(count);

        // sqrt approximation using newton's method for Decimal
        let std_dev = if variance.is_zero() {
            Decimal::ZERO
        } else {
            let mut x = variance / Decimal::TWO;
            for _ in 0..10 {
                x = (x + variance / x) / Decimal::TWO;
            }
            x
        };

        // Calculate trend using simple linear regression slope
        // trend = sum((x - x_mean) * (y - y_mean)) / sum((x - x_mean)^2)
        let mut trend = Decimal::ZERO;
        if count > 1 {
            let x_mean = Decimal::from(count - 1) / Decimal::TWO;
            let y_mean = average_rate;

            let mut numerator = Decimal::ZERO;
            let mut denominator = Decimal::ZERO;

            for (i, rate) in rates.iter().enumerate() {
                let x_diff = Decimal::from(i) - x_mean;
                let y_diff = rate.funding_rate - y_mean;
                numerator += x_diff * y_diff;
                denominator += x_diff * x_diff;
            }

            if !denominator.is_zero() {
                trend = numerator / denominator;
            }
        }

        // Get time range
        let start_time = rates.iter().map(|r| r.funding_time).min().unwrap();
        let end_time = rates.iter().map(|r| r.funding_time).max().unwrap();

        Ok(FundingRateStats {
            symbol,
            exchange: exchange.to_string(),
            start_time,
            end_time,
            average_rate,
            min_rate,
            max_rate,
            std_dev,
            count,
            trend,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use perps_core::{FundingRate, OrderbookLevel, Trade};
    use rust_decimal_macros::dec;

    #[tokio::test]
    async fn test_calculate_vwap() {
        let aggregator = Aggregator::new();

        let trades = vec![
            Trade {
                id: "1".to_string(),
                symbol: "BTCUSDT".to_string(),
                price: dec!(100),
                quantity: dec!(10),
                side: OrderSide::Buy,
                timestamp: Utc::now(),
            },
            Trade {
                id: "2".to_string(),
                symbol: "BTCUSDT".to_string(),
                price: dec!(200),
                quantity: dec!(20),
                side: OrderSide::Buy,
                timestamp: Utc::now(),
            },
        ];

        let vwap = aggregator.calculate_vwap(&trades).await.unwrap();

        // VWAP = (100 * 10 + 200 * 20) / (10 + 20) = 5000 / 30 = 166.666...
        let expected = dec!(5000) / dec!(30);
        assert_eq!(vwap, expected);
    }

    #[tokio::test]
    async fn test_calculate_vwap_empty_trades() {
        let aggregator = Aggregator::new();
        let trades: Vec<Trade> = vec![];

        let result = aggregator.calculate_vwap(&trades).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_calculate_liquidity_depth() {
        let aggregator = Aggregator::new();
        let book = Orderbook {
            symbol: "BTC-USDT-PERP".to_string(),
            timestamp: Utc::now(),
            bids: vec![
                OrderbookLevel { price: dec!(99.99), quantity: dec!(10) }, // 1 bps
                OrderbookLevel { price: dec!(99.95), quantity: dec!(10) }, // 5 bps
                OrderbookLevel { price: dec!(99.80), quantity: dec!(10) }, // 20 bps
            ],
            asks: vec![
                OrderbookLevel { price: dec!(100.01), quantity: dec!(10) }, // 1 bps
                OrderbookLevel { price: dec!(100.05), quantity: dec!(10) }, // 5 bps
                OrderbookLevel { price: dec!(100.20), quantity: dec!(10) }, // 20 bps
            ],
        };

        let stats = aggregator.calculate_liquidity_depth(&book, "binance", "BTC").await.unwrap();

        assert_eq!(stats.symbol, "BTC");
        assert_eq!(stats.mid_price, dec!(100));
        assert_eq!(stats.exchange, "binance");

        // 1 bps bids: 99.99 * 10 = 999.9
        assert_eq!(stats.bid_1bps, dec!(999.9));

        // 5 bps bids: (99.99 * 10) + (99.95 * 10) = 999.9 + 999.5 = 1999.4
        assert_eq!(stats.bid_5bps, dec!(1999.4));

        // 20 bps bids: (99.99 * 10) + (99.95 * 10) + (99.80 * 10) = 1999.4 + 998.0 = 2997.4
        assert_eq!(stats.bid_20bps, dec!(2997.4));

        // 1 bps asks: 100.01 * 10 = 1000.1
        assert_eq!(stats.ask_1bps, dec!(1000.1));

        // 5 bps asks: (100.01 * 10) + (100.05 * 10) = 1000.1 + 1000.5 = 2000.6
        assert_eq!(stats.ask_5bps, dec!(2000.6));

        // 20 bps asks: (100.01 * 10) + (100.05 * 10) + (100.20 * 10) = 2000.6 + 1002.0 = 3002.6
        assert_eq!(stats.ask_20bps, dec!(3002.6));
    }

    #[tokio::test]
    async fn test_calculate_funding_rate_stats() {
        let aggregator = Aggregator::new();
        let now = Utc::now();

        let rates = vec![
            FundingRate {
                symbol: "BTC".to_string(),
                funding_rate: dec!(0.0001),
                predicted_rate: dec!(0.0001),
                funding_time: now,
                next_funding_time: now,
                funding_interval: 8,
                funding_rate_cap_floor: dec!(0.005),
            },
            FundingRate {
                symbol: "BTC".to_string(),
                funding_rate: dec!(0.0002),
                predicted_rate: dec!(0.0002),
                funding_time: now,
                next_funding_time: now,
                funding_interval: 8,
                funding_rate_cap_floor: dec!(0.005),
            },
            FundingRate {
                symbol: "BTC".to_string(),
                funding_rate: dec!(0.0003),
                predicted_rate: dec!(0.0003),
                funding_time: now,
                next_funding_time: now,
                funding_interval: 8,
                funding_rate_cap_floor: dec!(0.005),
            },
        ];

        let stats = aggregator.calculate_funding_rate_stats(&rates, "binance").await.unwrap();

        assert_eq!(stats.symbol, "BTC");
        assert_eq!(stats.exchange, "binance");
        assert_eq!(stats.count, 3);
        assert_eq!(stats.average_rate, dec!(0.0002)); // (0.0001 + 0.0002 + 0.0003) / 3 = 0.0002
        assert_eq!(stats.min_rate, dec!(0.0001));
        assert_eq!(stats.max_rate, dec!(0.0003));
        assert!(stats.trend > Decimal::ZERO); // Should be positive trend (increasing)
    }

    #[tokio::test]
    async fn test_calculate_funding_rate_stats_empty() {
        let aggregator = Aggregator::new();
        let rates: Vec<FundingRate> = vec![];

        let result = aggregator.calculate_funding_rate_stats(&rates, "binance").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_calculate_funding_rate_stats_single_rate() {
        let aggregator = Aggregator::new();
        let now = Utc::now();

        let rates = vec![FundingRate {
            symbol: "BTC".to_string(),
            funding_rate: dec!(0.0001),
            predicted_rate: dec!(0.0001),
            funding_time: now,
            next_funding_time: now,
            funding_interval: 8,
            funding_rate_cap_floor: dec!(0.005),
        }];

        let stats = aggregator.calculate_funding_rate_stats(&rates, "binance").await.unwrap();

        assert_eq!(stats.count, 1);
        assert_eq!(stats.average_rate, dec!(0.0001));
        assert_eq!(stats.min_rate, dec!(0.0001));
        assert_eq!(stats.max_rate, dec!(0.0001));
        assert_eq!(stats.std_dev, Decimal::ZERO);
        assert_eq!(stats.trend, Decimal::ZERO); // No trend with single data point
    }
}
