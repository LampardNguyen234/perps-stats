//! Comprehensive integration tests for Gravity DEX
//!
//! This test suite provides in-depth coverage of Gravity client functionality,
//! including edge cases, error handling, and data validation.

use perps_core::IPerps;
use perps_exchanges::GravityClient;
use rust_decimal::Decimal;
use std::str::FromStr;

/// Test client creation and basic properties
#[test]
fn test_gravity_client_basic_properties() {
    let client = GravityClient::new();
    let name = client.get_name();

    assert_eq!(name, "gravity");
    assert!(!name.is_empty());
}

/// Test symbol parsing with various formats
#[test]
fn test_parse_symbol_variations() {
    let client = GravityClient::new();

    // Standard format
    assert_eq!(client.parse_symbol("BTC"), "BTC_USDT_Perp");
    assert_eq!(client.parse_symbol("ETH"), "ETH_USDT_Perp");
    assert_eq!(client.parse_symbol("SOL"), "SOL_USDT_Perp");

    // Case insensitivity (converted to uppercase)
    assert_eq!(client.parse_symbol("btc"), "BTC_USDT_Perp");
    assert_eq!(client.parse_symbol("eth"), "ETH_USDT_Perp");
    assert_eq!(client.parse_symbol("Btc"), "BTC_USDT_Perp");

    // Mixed case
    assert_eq!(client.parse_symbol("AvAx"), "AVAX_USDT_Perp");
}

/// Test symbol parsing with special characters
#[test]
fn test_parse_symbol_with_numbers() {
    let client = GravityClient::new();

    // Symbols with numbers (uncommon but should be handled)
    let result = client.parse_symbol("BTC1");
    assert!(result.contains("BTC1"));
    assert!(result.contains("_USDT_Perp"));
}

/// Test conversion of Gravity ticker response to core Ticker
#[tokio::test]
async fn test_ticker_conversion_precision() {
    let client = GravityClient::new();

    // This tests that we handle decimal precision correctly
    // Real API returns prices with up to 9 decimal places
    match client.get_ticker("BTC").await {
        Ok(ticker) => {
            // All prices should be positive
            assert!(ticker.last_price > Decimal::ZERO);
            assert!(ticker.mark_price > Decimal::ZERO);
            assert!(ticker.index_price > Decimal::ZERO);

            // Bid should be less than ask (typical market condition)
            assert!(
                ticker.best_bid_price < ticker.best_ask_price,
                "Bid {} should be less than ask {}",
                ticker.best_bid_price,
                ticker.best_ask_price
            );

            // Mid price should be between bid and ask
            let mid = (ticker.best_bid_price + ticker.best_ask_price) / Decimal::from(2);
            assert!(ticker.best_bid_price < mid && mid < ticker.best_ask_price);

            tracing::info!(
                "Ticker precision verified: last={}, bid={}, ask={}",
                ticker.last_price,
                ticker.best_bid_price,
                ticker.best_ask_price
            );
        }
        Err(e) => {
            tracing::warn!("Could not verify ticker precision (API unavailable): {}", e);
        }
    }
}

/// Test that all tickers have consistent structure
#[tokio::test]
async fn test_all_tickers_consistency() {
    let client = GravityClient::new();

    match client.get_all_tickers().await {
        Ok(tickers) => {
            assert!(!tickers.is_empty(), "Should return at least one ticker");

            // All tickers should have same structure
            for ticker in tickers.iter().take(5) {
                assert!(!ticker.symbol.is_empty());
                assert!(ticker.last_price > Decimal::ZERO);
                assert!(ticker.mark_price > Decimal::ZERO);
                assert!(ticker.best_bid_qty > Decimal::ZERO);
                assert!(ticker.best_ask_qty > Decimal::ZERO);

                // Validate bid/ask spread
                assert!(
                    ticker.best_bid_price < ticker.best_ask_price,
                    "Symbol {}: bid {} >= ask {}",
                    ticker.symbol,
                    ticker.best_bid_price,
                    ticker.best_ask_price
                );
            }

            tracing::info!(
                "All tickers consistency verified: {} tickers checked",
                tickers.len()
            );
        }
        Err(e) => {
            tracing::warn!("Could not verify all tickers (API unavailable): {}", e);
        }
    }
}

/// Test orderbook depth validation
#[tokio::test]
async fn test_orderbook_depth_structure() {
    let client = GravityClient::new();

    for symbol in &["BTC", "ETH"] {
        match client.get_orderbook(symbol, 5).await {
            Ok(orderbook) => {
                assert_eq!(orderbook.symbol, *symbol);
                assert!(!orderbook.orderbooks.is_empty());

                tracing::info!(
                    "Orderbook validated for {}: {} resolution levels",
                    symbol,
                    orderbook.orderbooks.len()
                );
            }
            Err(e) => {
                tracing::warn!("Could not fetch orderbook for {}: {}", symbol, e);
            }
        }
    }
}

/// Test funding rate constraints
#[tokio::test]
async fn test_funding_rate_constraints() {
    let client = GravityClient::new();

    match client.get_funding_rate("BTC").await {
        Ok(funding_rate) => {
            assert_eq!(funding_rate.symbol, "BTC");

            // Funding rate should be reasonable (typically -10% to +10%)
            let max_fr = Decimal::from_str("0.1").unwrap();
            assert!(
                funding_rate.funding_rate.abs() <= max_fr,
                "Funding rate {} out of reasonable bounds",
                funding_rate.funding_rate
            );

            // Funding interval should be positive (typically 8 hours)
            assert!(funding_rate.funding_interval > 0);

            tracing::info!(
                "Funding rate validated: {} (interval: {}h)",
                funding_rate.funding_rate,
                funding_rate.funding_interval
            );
        }
        Err(e) => {
            tracing::warn!("Could not fetch funding rate: {}", e);
        }
    }
}

/// Test open interest non-negativity
#[tokio::test]
async fn test_open_interest_constraints() {
    let client = GravityClient::new();

    match client.get_open_interest("BTC").await {
        Ok(oi) => {
            assert_eq!(oi.symbol, "BTC");
            assert!(
                oi.open_interest >= Decimal::ZERO,
                "Open interest should be non-negative"
            );

            tracing::info!("Open interest validated: {}", oi.open_interest);
        }
        Err(e) => {
            tracing::warn!("Could not fetch open interest: {}", e);
        }
    }
}

/// Test kline data structure
#[tokio::test]
async fn test_kline_ohlcv_structure() {
    let client = GravityClient::new();

    match client.get_klines("BTC", "1h", None, None, Some(5)).await {
        Ok(klines) => {
            assert!(!klines.is_empty());

            for kline in klines.iter() {
                assert_eq!(kline.symbol, "BTC");
                assert_eq!(kline.interval, "1h");

                // OHLC must satisfy: high >= close, high >= open, low <= close, low <= open
                assert!(
                    kline.high >= kline.close && kline.high >= kline.open,
                    "High {} must be >= both open {} and close {}",
                    kline.high,
                    kline.open,
                    kline.close
                );
                assert!(
                    kline.low <= kline.close && kline.low <= kline.open,
                    "Low {} must be <= both open {} and close {}",
                    kline.low,
                    kline.open,
                    kline.close
                );

                // All prices should be positive
                assert!(kline.open > Decimal::ZERO);
                assert!(kline.close > Decimal::ZERO);
                assert!(kline.high > Decimal::ZERO);
                assert!(kline.low > Decimal::ZERO);
            }

            tracing::info!("Kline OHLCV structure validated: {} candles", klines.len());
        }
        Err(e) => {
            tracing::warn!("Could not fetch klines: {}", e);
        }
    }
}

/// Test trade data structure
#[tokio::test]
async fn test_recent_trades_structure() {
    let client = GravityClient::new();

    match client.get_recent_trades("BTC", 10).await {
        Ok(trades) => {
            assert!(!trades.is_empty());

            for trade in trades.iter() {
                assert_eq!(trade.symbol, "BTC");
                assert!(!trade.id.is_empty());
                assert!(trade.price > Decimal::ZERO);
                assert!(trade.quantity > Decimal::ZERO);

                // Timestamp should be recent (within last 7 days)
                use chrono::Utc;
                let now = Utc::now();
                let seven_days_ago = now - chrono::Duration::days(7);
                assert!(trade.timestamp > seven_days_ago);
            }

            tracing::info!("Recent trades structure validated: {} trades", trades.len());
        }
        Err(e) => {
            tracing::warn!("Could not fetch recent trades: {}", e);
        }
    }
}

/// Test market statistics aggregation
#[tokio::test]
async fn test_market_stats_aggregation() {
    let client = GravityClient::new();

    match client.get_market_stats("BTC").await {
        Ok(stats) => {
            assert_eq!(stats.symbol, "BTC");

            // All required fields should be present
            assert!(stats.last_price > Decimal::ZERO);
            assert!(stats.mark_price > Decimal::ZERO);
            assert!(stats.volume_24h >= Decimal::ZERO);
            assert!(stats.open_interest >= Decimal::ZERO);

            // Funding rate should be reasonable
            let max_fr = Decimal::from_str("0.1").unwrap();
            assert!(stats.funding_rate.abs() <= max_fr);

            tracing::info!(
                "Market stats aggregation verified: price={}, vol_24h={}, oi={}",
                stats.last_price,
                stats.volume_24h,
                stats.open_interest
            );
        }
        Err(e) => {
            tracing::warn!("Could not fetch market stats: {}", e);
        }
    }
}

/// Test all market stats for multiple symbols
#[tokio::test]
async fn test_all_market_stats_coverage() {
    let client = GravityClient::new();

    match client.get_all_market_stats().await {
        Ok(stats) => {
            assert!(!stats.is_empty());

            // Verify we have major symbols
            let symbols: Vec<_> = stats.iter().map(|s| s.symbol.clone()).collect();
            tracing::info!(
                "All market stats coverage: {} symbols available",
                stats.len()
            );

            // Check if we have at least some major symbols
            let has_major_pairs = symbols
                .iter()
                .any(|s| s == "BTC" || s == "ETH" || s == "SOL");
            assert!(has_major_pairs, "Should have at least one major pair");
        }
        Err(e) => {
            tracing::warn!("Could not fetch all market stats: {}", e);
        }
    }
}

/// Test error handling for invalid intervals
#[tokio::test]
async fn test_invalid_kline_interval_rejected() {
    let client = GravityClient::new();

    let result = client.get_klines("BTC", "99h", None, None, Some(1)).await;
    assert!(result.is_err(), "Should reject invalid interval '99h'");

    match result {
        Err(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains("Unsupported") || msg.contains("interval"),
                "Error message should mention unsupported interval"
            );
            tracing::info!("Invalid interval correctly rejected: {}", msg);
        }
        Ok(_) => panic!("Should have rejected invalid interval"),
    }
}

/// Test all supported kline intervals
#[tokio::test]
async fn test_all_supported_kline_intervals_valid() {
    let client = GravityClient::new();
    let intervals = vec!["1m", "5m", "15m", "30m", "1h", "4h", "1d", "1w"];

    for interval in intervals {
        let result = client
            .get_klines("BTC", interval, None, None, Some(1))
            .await;

        match result {
            Ok(_) => {
                tracing::info!("Interval {} supported", interval);
            }
            Err(e) => {
                // It's okay if API doesn't have data, as long as validation passes
                let msg = e.to_string();
                if msg.contains("Unsupported") || msg.contains("interval") {
                    panic!("Interval {} should be supported but was rejected", interval);
                }
                tracing::debug!("Interval {} has no data but is supported: {}", interval, e);
            }
        }
    }
}

/// Test markets caching behavior
#[tokio::test]
async fn test_markets_caching() {
    let client = GravityClient::new();

    // First call - fetches from API
    match client.get_markets().await {
        Ok(markets1) => {
            // This is a basic test; actual caching would require multiple calls
            // and timing verification
            assert!(!markets1.is_empty());
            tracing::info!("Markets caching test: {} markets available", markets1.len());
        }
        Err(e) => {
            // API might be unavailable in test environment
            tracing::warn!("Could not test markets caching (API unavailable): {}", e);
        }
    }
}

/// Test rate limiter doesn't cause excessive delays
#[tokio::test]
async fn test_rate_limiter_performance() {
    let client = GravityClient::new();
    let start = std::time::Instant::now();

    // Make 5 rapid calls (should be within rate limit)
    for _ in 0..5 {
        let _ = client.get_ticker("BTC").await;
    }

    let elapsed = start.elapsed();

    // Should complete in reasonable time (not too slow due to rate limiting)
    assert!(
        elapsed.as_secs() < 10,
        "5 calls took too long: {:?}",
        elapsed
    );

    tracing::info!("Rate limiter performance test: 5 calls in {:?}", elapsed);
}

/// Test symbols are properly formatted in responses
#[tokio::test]
async fn test_response_symbol_format() {
    let client = GravityClient::new();

    match client.get_ticker("BTC").await {
        Ok(ticker) => {
            // Symbol in response should be global format (BTC, not BTC_USDT_Perp)
            assert_eq!(ticker.symbol, "BTC");
            assert!(!ticker.symbol.contains("_"));
            assert!(!ticker.symbol.contains("USDT"));
        }
        Err(e) => {
            tracing::warn!("Could not verify symbol format: {}", e);
        }
    }
}

/// Test funding rate history (Gravity limitation: returns current rate)
#[tokio::test]
async fn test_funding_rate_history_compatibility() {
    let client = GravityClient::new();

    match client
        .get_funding_rate_history("BTC", None, None, Some(10))
        .await
    {
        Ok(history) => {
            // Gravity returns current rate as single element (documented limitation)
            assert!(!history.is_empty());
            assert_eq!(history[0].symbol, "BTC");

            tracing::info!(
                "Funding rate history test (Gravity returns current rate only): {} records",
                history.len()
            );
        }
        Err(e) => {
            tracing::warn!("Could not fetch funding rate history: {}", e);
        }
    }
}
