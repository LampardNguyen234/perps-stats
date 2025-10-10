use async_trait::async_trait;
use perps_core::{OrderSide, Orderbook, Trade};
use rust_decimal::Decimal;

use crate::types::MarketDepth;

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use perps_core::Trade;
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
}
