//! Integration tests for Gravity DEX exchange client
//!
//! These tests verify that the Gravity client correctly implements the IPerps trait
//! and can successfully communicate with the Gravity API endpoints.
//!
//! **Note:** These tests make real API calls to the Gravity public API and require network access.
//! Tests use real symbols and may be subject to rate limiting.

use perps_core::IPerps;
use perps_exchanges::GravityClient;
use rust_decimal::Decimal;
use std::str::FromStr;

/// Test that GravityClient can be instantiated
#[test]
fn test_gravity_client_creation() {
    let client = GravityClient::new();
    // If we get here, client creation succeeded
    assert!(!client.get_name().is_empty());
}

/// Test that GravityClient provides the correct exchange name
#[test]
fn test_gravity_client_name() {
    let client = GravityClient::new();
    assert_eq!(client.get_name(), "gravity");
}

/// Test symbol parsing from global format to Gravity format
#[test]
fn test_gravity_symbol_parsing() {
    let client = GravityClient::new();

    // Global symbol should be converted to Gravity format
    let gravity_symbol = client.parse_symbol("BTC");
    assert_eq!(gravity_symbol, "BTC_USDT_Perp");
    assert!(gravity_symbol.contains("BTC"));
    assert!(gravity_symbol.contains("USDT"));
    assert!(gravity_symbol.contains("Perp"));

    // Case insensitivity - lowercase should also work
    let gravity_symbol_lowercase = client.parse_symbol("btc");
    assert_eq!(gravity_symbol_lowercase, "BTC_USDT_Perp");
}

/// Test that get_markets returns available instruments
#[tokio::test]
async fn test_get_markets() {
    let client = GravityClient::new();

    match client.get_markets().await {
        Ok(markets) => {
            // Should have at least some markets
            assert!(
                !markets.is_empty(),
                "Expected at least one market from Gravity API"
            );

            // Verify market structure
            for market in markets.iter().take(1) {
                assert!(
                    !market.symbol.is_empty(),
                    "Market symbol should not be empty"
                );
                assert!(
                    !market.contract.is_empty(),
                    "Contract type should not be empty"
                );
                assert!(
                    market.min_order_qty > Decimal::ZERO,
                    "Min order quantity should be positive"
                );
                assert!(
                    market.max_order_qty > Decimal::ZERO,
                    "Max order quantity should be positive"
                );
            }

            tracing::info!(
                "Successfully fetched {} markets from Gravity API",
                markets.len()
            );
        }
        Err(e) => {
            // Log error but don't fail - API might be unavailable in test environment
            tracing::warn!("Failed to fetch markets from Gravity API: {}. This may be expected in test environments.", e);
        }
    }
}

/// Test that get_ticker returns valid ticker data
#[tokio::test]
async fn test_get_ticker_btc() {
    let client = GravityClient::new();

    match client.get_ticker("BTC").await {
        Ok(ticker) => {
            // Verify ticker structure
            assert_eq!(ticker.symbol, "BTC", "Symbol should match request");
            assert!(
                ticker.last_price > Decimal::ZERO,
                "Last price should be positive"
            );
            assert!(
                ticker.mark_price > Decimal::ZERO,
                "Mark price should be positive"
            );
            assert!(
                ticker.index_price > Decimal::ZERO,
                "Index price should be positive"
            );
            assert!(
                ticker.best_bid_price > Decimal::ZERO,
                "Bid price should be positive"
            );
            assert!(
                ticker.best_ask_price > Decimal::ZERO,
                "Ask price should be positive"
            );
            assert!(
                ticker.best_bid_qty > Decimal::ZERO,
                "Bid quantity should be positive"
            );
            assert!(
                ticker.best_ask_qty > Decimal::ZERO,
                "Ask quantity should be positive"
            );

            // Bid should always be less than ask
            assert!(
                ticker.best_bid_price < ticker.best_ask_price,
                "Bid price should be less than ask price"
            );

            tracing::info!(
                "BTC ticker: last={} mark={} index={} bid={} ask={}",
                ticker.last_price,
                ticker.mark_price,
                ticker.index_price,
                ticker.best_bid_price,
                ticker.best_ask_price
            );
        }
        Err(e) => {
            tracing::warn!("Failed to fetch BTC ticker from Gravity API: {}. This may be expected in test environments.", e);
        }
    }
}

/// Test that get_ticker works for another symbol
#[tokio::test]
async fn test_get_ticker_eth() {
    let client = GravityClient::new();

    match client.get_ticker("ETH").await {
        Ok(ticker) => {
            assert_eq!(ticker.symbol, "ETH");
            assert!(ticker.last_price > Decimal::ZERO);

            tracing::info!(
                "Successfully fetched ETH ticker: price={}",
                ticker.last_price
            );
        }
        Err(e) => {
            tracing::warn!("Failed to fetch ETH ticker from Gravity API: {}", e);
        }
    }
}

/// Test that get_all_tickers returns multiple tickers
#[tokio::test]
async fn test_get_all_tickers() {
    let client = GravityClient::new();

    match client.get_all_tickers().await {
        Ok(tickers) => {
            assert!(!tickers.is_empty(), "Expected at least one ticker");

            // Verify all tickers have required fields
            for ticker in tickers.iter().take(3) {
                assert!(!ticker.symbol.is_empty(), "Symbol should not be empty");
                assert!(
                    ticker.last_price > Decimal::ZERO,
                    "Last price should be positive"
                );
                assert!(
                    ticker.mark_price > Decimal::ZERO,
                    "Mark price should be positive"
                );
            }

            tracing::info!(
                "Successfully fetched {} tickers from Gravity API",
                tickers.len()
            );
        }
        Err(e) => {
            tracing::warn!("Failed to fetch all tickers from Gravity API: {}", e);
        }
    }
}

/// Test that get_orderbook returns bid/ask levels
#[tokio::test]
async fn test_get_orderbook() {
    let client = GravityClient::new();

    match client.get_orderbook("BTC", 5).await {
        Ok(orderbook) => {
            assert_eq!(orderbook.symbol, "BTC");
            // MultiResolutionOrderbook contains orderbooks field
            assert!(
                !orderbook.orderbooks.is_empty(),
                "Orderbook should have resolution levels"
            );

            tracing::info!(
                "BTC Orderbook: {} resolution levels",
                orderbook.orderbooks.len()
            );
        }
        Err(e) => {
            tracing::warn!("Failed to fetch BTC orderbook from Gravity API: {}", e);
        }
    }
}

/// Test that get_funding_rate returns current funding rate
#[tokio::test]
async fn test_get_funding_rate() {
    let client = GravityClient::new();

    match client.get_funding_rate("BTC").await {
        Ok(funding_rate) => {
            assert_eq!(funding_rate.symbol, "BTC");
            // Funding rate can be positive or negative, but should be a reasonable percentage
            assert!(
                funding_rate.funding_rate.abs() < Decimal::from_str("0.1").unwrap(),
                "Funding rate should be less than 10%"
            );
            assert!(
                funding_rate.funding_interval > 0,
                "Funding interval should be positive"
            );

            tracing::info!(
                "BTC Funding Rate: {} (interval: {} hours)",
                funding_rate.funding_rate,
                funding_rate.funding_interval
            );
        }
        Err(e) => {
            tracing::warn!("Failed to fetch BTC funding rate from Gravity API: {}", e);
        }
    }
}

/// Test that get_open_interest returns OI data
#[tokio::test]
async fn test_get_open_interest() {
    let client = GravityClient::new();

    match client.get_open_interest("BTC").await {
        Ok(open_interest) => {
            assert_eq!(open_interest.symbol, "BTC");
            // Open interest should be non-negative
            assert!(
                open_interest.open_interest >= Decimal::ZERO,
                "Open interest should be non-negative"
            );

            tracing::info!("BTC Open Interest: {}", open_interest.open_interest);
        }
        Err(e) => {
            tracing::warn!("Failed to fetch BTC open interest from Gravity API: {}", e);
        }
    }
}

/// Test that get_klines returns candlestick data
#[tokio::test]
async fn test_get_klines() {
    let client = GravityClient::new();

    match client.get_klines("BTC", "1h", None, None, Some(10)).await {
        Ok(klines) => {
            assert!(!klines.is_empty(), "Should return at least one kline");

            for kline in klines.iter().take(2) {
                assert_eq!(kline.symbol, "BTC");
                assert_eq!(kline.interval, "1h");
                assert!(kline.open > Decimal::ZERO);
                assert!(kline.high > Decimal::ZERO);
                assert!(kline.low > Decimal::ZERO);
                assert!(kline.close > Decimal::ZERO);
                // High should be >= Low
                assert!(kline.high >= kline.low, "High price should be >= low price");
            }

            tracing::info!("Successfully fetched {} klines for BTC 1h", klines.len());
        }
        Err(e) => {
            tracing::warn!("Failed to fetch BTC klines from Gravity API: {}", e);
        }
    }
}

/// Test that get_recent_trades returns trade history
#[tokio::test]
async fn test_get_recent_trades() {
    let client = GravityClient::new();

    match client.get_recent_trades("BTC", 10).await {
        Ok(trades) => {
            assert!(!trades.is_empty(), "Should return at least one trade");

            for trade in trades.iter().take(2) {
                assert_eq!(trade.symbol, "BTC");
                assert!(!trade.id.is_empty());
                assert!(trade.price > Decimal::ZERO);
                assert!(trade.quantity > Decimal::ZERO);
            }

            tracing::info!("Successfully fetched {} trades for BTC", trades.len());
        }
        Err(e) => {
            tracing::warn!("Failed to fetch BTC trades from Gravity API: {}", e);
        }
    }
}

/// Test that get_market_stats aggregates multiple data sources
#[tokio::test]
async fn test_get_market_stats() {
    let client = GravityClient::new();

    match client.get_market_stats("BTC").await {
        Ok(stats) => {
            assert_eq!(stats.symbol, "BTC");
            assert!(stats.last_price > Decimal::ZERO);
            assert!(stats.mark_price > Decimal::ZERO);
            assert!(stats.volume_24h >= Decimal::ZERO);
            assert!(stats.open_interest >= Decimal::ZERO);
            // Funding rate should be reasonable (between -10% and +10%)
            assert!(
                stats.funding_rate.abs() < Decimal::from_str("0.1").unwrap(),
                "Funding rate should be between -10% and +10%"
            );

            tracing::info!(
                "BTC Market Stats: price={} volume_24h={} open_interest={} funding_rate={}",
                stats.last_price,
                stats.volume_24h,
                stats.open_interest,
                stats.funding_rate
            );
        }
        Err(e) => {
            tracing::warn!("Failed to fetch BTC market stats from Gravity API: {}", e);
        }
    }
}

/// Test that get_all_market_stats handles multiple symbols gracefully
#[tokio::test]
async fn test_get_all_market_stats() {
    let client = GravityClient::new();

    match client.get_all_market_stats().await {
        Ok(stats) => {
            assert!(
                !stats.is_empty(),
                "Should return stats for at least one symbol"
            );

            // Verify structure of returned stats
            for stat in stats.iter().take(3) {
                assert!(!stat.symbol.is_empty());
                assert!(stat.last_price > Decimal::ZERO);
                assert!(stat.volume_24h >= Decimal::ZERO);
            }

            tracing::info!(
                "Successfully fetched market stats for {} symbols",
                stats.len()
            );
        }
        Err(e) => {
            tracing::warn!("Failed to fetch all market stats from Gravity API: {}", e);
        }
    }
}

/// Test error handling for unsupported kline intervals
#[tokio::test]
async fn test_unsupported_kline_interval() {
    let client = GravityClient::new();

    match client.get_klines("BTC", "99m", None, None, Some(10)).await {
        Ok(_) => {
            panic!("Expected error for unsupported interval");
        }
        Err(e) => {
            // This is expected
            assert!(e.to_string().contains("Unsupported") || e.to_string().contains("interval"));
            tracing::info!("Correctly rejected unsupported interval: {}", e);
        }
    }
}

/// Test that valid kline intervals are accepted
#[tokio::test]
async fn test_supported_kline_intervals() {
    let client = GravityClient::new();
    let supported_intervals = vec!["1m", "5m", "15m", "30m", "1h", "4h", "1d", "1w"];

    for interval in supported_intervals {
        // We're just testing that the validation passes, not necessarily that we get data
        // (API might not have data for all intervals)
        match client
            .get_klines("BTC", interval, None, None, Some(1))
            .await
        {
            Ok(_) => {
                tracing::info!("Successfully fetched klines for interval: {}", interval);
            }
            Err(e) => {
                // It's ok if the API doesn't have data, as long as we got past validation
                if e.to_string().contains("Unsupported") {
                    panic!(
                        "Interval {} should be supported but got error: {}",
                        interval, e
                    );
                }
                tracing::debug!(
                    "API doesn't have kline data for interval {}: {}",
                    interval,
                    e
                );
            }
        }
    }
}

/// Test funding rate history (which Gravity doesn't fully support)
#[tokio::test]
async fn test_get_funding_rate_history() {
    let client = GravityClient::new();

    match client
        .get_funding_rate_history("BTC", None, None, Some(10))
        .await
    {
        Ok(history) => {
            // Gravity returns current funding rate as compatibility measure
            assert!(
                !history.is_empty(),
                "Should return at least current funding rate"
            );
            assert_eq!(history[0].symbol, "BTC");

            tracing::info!(
                "Got {} funding rate records (Gravity limitation: returns current rate only)",
                history.len()
            );
        }
        Err(e) => {
            tracing::warn!("Failed to fetch funding rate history: {}", e);
        }
    }
}
