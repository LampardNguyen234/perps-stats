use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use perps_aggregator::aggregator::Aggregator;
use perps_aggregator::IAggregator;
use perps_core::types::{
    FundingRate, Kline, Market, MarketStats, MultiResolutionOrderbook, OpenInterest, Orderbook,
    OrderbookLevel, Ticker, Trade,
};
use perps_core::IPerps;
use rust_decimal::Decimal;
use std::str::FromStr;

struct MockExchange {
    name: String,
    orderbook: Orderbook,
}

#[async_trait]
impl IPerps for MockExchange {
    fn get_name(&self) -> &str {
        &self.name
    }

    fn parse_symbol(&self, symbol: &str) -> String {
        symbol.to_string()
    }

    async fn get_orderbook(&self, _symbol: &str, _depth: u32) -> Result<MultiResolutionOrderbook> {
        Ok(MultiResolutionOrderbook::from_single(self.orderbook.clone()))
    }

    // Unused methods
    async fn get_markets(&self) -> Result<Vec<Market>> {
        unimplemented!()
    }
    async fn get_market(&self, _symbol: &str) -> Result<Market> {
        unimplemented!()
    }
    async fn get_ticker(&self, _symbol: &str) -> Result<Ticker> {
        unimplemented!()
    }
    async fn get_all_tickers(&self) -> Result<Vec<Ticker>> {
        unimplemented!()
    }
    async fn get_funding_rate(&self, _symbol: &str) -> Result<FundingRate> {
        unimplemented!()
    }
    async fn get_funding_rate_history(
        &self,
        _symbol: &str,
        _start_time: Option<chrono::DateTime<Utc>>,
        _end_time: Option<chrono::DateTime<Utc>>,
        _limit: Option<u32>,
    ) -> Result<Vec<FundingRate>> {
        unimplemented!()
    }
    async fn get_open_interest(&self, _symbol: &str) -> Result<OpenInterest> {
        unimplemented!()
    }
    async fn get_klines(
        &self,
        _symbol: &str,
        _interval: &str,
        _start_time: Option<chrono::DateTime<Utc>>,
        _end_time: Option<chrono::DateTime<Utc>>,
        _limit: Option<u32>,
    ) -> Result<Vec<Kline>> {
        unimplemented!()
    }
    async fn get_recent_trades(&self, _symbol: &str, _limit: u32) -> Result<Vec<Trade>> {
        unimplemented!()
    }
    async fn get_market_stats(&self, _symbol: &str) -> Result<MarketStats> {
        unimplemented!()
    }
    async fn get_all_market_stats(&self) -> Result<Vec<MarketStats>> {
        unimplemented!()
    }
    async fn is_supported(&self, _symbol: &str) -> Result<bool> {
        Ok(true)
    }
}

fn create_mock_orderbook() -> Orderbook {
    Orderbook {
        symbol: "BTC-USDT".to_string(),
        bids: vec![
            OrderbookLevel {
                price: Decimal::from_str("10000").unwrap(),
                quantity: Decimal::from_str("1").unwrap(),
            },
            OrderbookLevel {
                price: Decimal::from_str("9999").unwrap(),
                quantity: Decimal::from_str("2").unwrap(),
            },
        ],
        asks: vec![
            OrderbookLevel {
                price: Decimal::from_str("10001").unwrap(),
                quantity: Decimal::from_str("1.5").unwrap(),
            },
            OrderbookLevel {
                price: Decimal::from_str("10002").unwrap(),
                quantity: Decimal::from_str("2.5").unwrap(),
            },
        ],
        timestamp: Utc::now(),
    }
}

#[tokio::test]
async fn test_compute_liquidity_all() -> Result<()> {
    let aggregator = Aggregator::new();
    let orderbook = create_mock_orderbook();

    let mock_binance = MockExchange {
        name: "binance".to_string(),
        orderbook: orderbook.clone(),
    };
    let mock_lighter = MockExchange {
        name: "lighter".to_string(),
        orderbook: orderbook.clone(),
    };

    let exchanges: Vec<Box<dyn IPerps + Send + Sync>> = vec![Box::new(mock_binance), Box::new(mock_lighter)];

    let symbol = "BTC-USDT";
    let results = aggregator
        .calculate_liquidity_depth_all(&exchanges, symbol)
        .await?;

    assert_eq!(results.len(), 2);

    let binance_stats = results.iter().find(|s| s.exchange == "binance").unwrap();
    let lighter_stats = results.iter().find(|s| s.exchange == "lighter").unwrap();

    assert_eq!(binance_stats.symbol, symbol);
    assert_eq!(lighter_stats.symbol, symbol);

    // A simple check to ensure some values were calculated.
    // The core calculation logic is tested in `aggregator.rs` tests.
    assert!(binance_stats.mid_price > Decimal::ZERO);
    assert!(binance_stats.bid_10bps > Decimal::ZERO);
    assert!(binance_stats.ask_10bps > Decimal::ZERO);

    Ok(())
}
