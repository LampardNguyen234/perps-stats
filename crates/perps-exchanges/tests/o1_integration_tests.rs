//! Integration tests for 01.xyz (Nord) exchange client
//!
//! These tests verify that the O1Client correctly implements the IPerps trait
//! and can successfully communicate with the Nord API endpoints.
//!
//! **Note:** These tests make real API calls to the Nord public API and require network access.
//! Tests use real symbols and may be subject to rate limiting.

use perps_core::IPerps;
use perps_exchanges::O1Client;
use rust_decimal::Decimal;

/// Test that get_markets returns available instruments with non-empty symbols
#[tokio::test]
async fn test_get_markets() {
    let client = O1Client::new();

    match client.get_markets().await {
        Ok(markets) => {
            assert!(
                !markets.is_empty(),
                "Expected at least one market from 01 API"
            );

            for market in &markets {
                assert!(
                    !market.symbol.is_empty(),
                    "Market symbol should not be empty"
                );
            }

            tracing::info!("Successfully fetched {} markets from 01 API", markets.len());
        }
        Err(e) => {
            eprintln!("01.xyz API unavailable (expected in CI): {}", e);
        }
    }
}

/// Test that get_ticker returns valid ticker data for BTC
#[tokio::test]
async fn test_get_ticker_btc() {
    let client = O1Client::new();

    match client.get_ticker("BTC").await {
        Ok(ticker) => {
            assert_eq!(ticker.symbol, "BTC", "Symbol should match request");
            assert!(
                ticker.last_price > Decimal::ZERO,
                "Last price should be positive"
            );
        }
        Err(e) => {
            eprintln!("01.xyz API unavailable (expected in CI): {}", e);
        }
    }
}

/// Test that get_all_tickers returns multiple tickers
#[tokio::test]
async fn test_get_all_tickers() {
    let client = O1Client::new();

    match client.get_all_tickers().await {
        Ok(tickers) => {
            assert!(!tickers.is_empty(), "Expected at least one ticker");

            tracing::info!("Successfully fetched {} tickers from 01 API", tickers.len());
        }
        Err(e) => {
            eprintln!("01.xyz API unavailable (expected in CI): {}", e);
        }
    }
}

/// Test that get_orderbook returns bid/ask levels with correct ordering
#[tokio::test]
async fn test_get_orderbook_btc() {
    let client = O1Client::new();

    match client.get_orderbook("BTC", 20).await {
        Ok(orderbook) => {
            assert_eq!(orderbook.symbol, "BTC");
            assert!(
                !orderbook.orderbooks.is_empty(),
                "Orderbook should have at least one resolution level"
            );

            // Check the finest resolution orderbook
            let finest = &orderbook.orderbooks[0];
            assert!(!finest.bids.is_empty(), "Should have bids");
            assert!(!finest.asks.is_empty(), "Should have asks");
            assert!(
                finest.bids[0].price < finest.asks[0].price,
                "Best bid should be less than best ask"
            );

            tracing::info!(
                "BTC Orderbook: {} resolution levels, {} bids, {} asks",
                orderbook.orderbooks.len(),
                finest.bids.len(),
                finest.asks.len()
            );
        }
        Err(e) => {
            eprintln!("01.xyz API unavailable (expected in CI): {}", e);
        }
    }
}

/// Test that get_funding_rate returns valid funding rate for BTC
#[tokio::test]
async fn test_get_funding_rate_btc() {
    let client = O1Client::new();

    match client.get_funding_rate("BTC").await {
        Ok(funding_rate) => {
            assert_eq!(funding_rate.symbol, "BTC", "Symbol should match request");

            tracing::info!("BTC Funding Rate: {}", funding_rate.funding_rate);
        }
        Err(e) => {
            eprintln!("01.xyz API unavailable (expected in CI): {}", e);
        }
    }
}

/// Test that get_open_interest returns positive OI for BTC
#[tokio::test]
async fn test_get_open_interest_btc() {
    let client = O1Client::new();

    match client.get_open_interest("BTC").await {
        Ok(open_interest) => {
            assert_eq!(open_interest.symbol, "BTC", "Symbol should match request");
            assert!(
                open_interest.open_interest > Decimal::ZERO,
                "Open interest should be positive"
            );

            tracing::info!("BTC Open Interest: {}", open_interest.open_interest);
        }
        Err(e) => {
            eprintln!("01.xyz API unavailable (expected in CI): {}", e);
        }
    }
}

/// Test that get_klines returns 1h kline data for BTC
#[tokio::test]
async fn test_get_klines_btc_1h() {
    let client = O1Client::new();

    match client.get_klines("BTC", "1h", None, None, Some(5)).await {
        Ok(klines) => {
            assert!(!klines.is_empty(), "Should return at least one kline");

            for kline in &klines {
                assert_eq!(kline.interval, "1h", "Interval should be 1h");
            }

            tracing::info!("Successfully fetched {} klines for BTC 1h", klines.len());
        }
        Err(e) => {
            eprintln!("01.xyz API unavailable (expected in CI): {}", e);
        }
    }
}

/// Test that get_klines rejects unsupported intervals (client-side validation)
#[tokio::test]
async fn test_get_klines_rejects_5m() {
    let client = O1Client::new();

    let result = client.get_klines("BTC", "5m", None, None, None).await;
    assert!(
        result.is_err(),
        "Expected error for unsupported interval '5m'"
    );
}

/// Test that get_recent_trades returns trade data for BTC
#[tokio::test]
async fn test_get_recent_trades_btc() {
    let client = O1Client::new();

    match client.get_recent_trades("BTC", 10).await {
        Ok(trades) => {
            assert!(!trades.is_empty(), "Should return at least one trade");

            for trade in &trades {
                assert!(!trade.symbol.is_empty(), "Trade symbol should not be empty");
            }

            tracing::info!("Successfully fetched {} trades for BTC", trades.len());
        }
        Err(e) => {
            eprintln!("01.xyz API unavailable (expected in CI): {}", e);
        }
    }
}

/// Test that get_market_stats returns valid stats for BTC
#[tokio::test]
async fn test_get_market_stats_btc() {
    let client = O1Client::new();

    match client.get_market_stats("BTC").await {
        Ok(stats) => {
            assert_eq!(stats.symbol, "BTC", "Symbol should match request");

            tracing::info!(
                "BTC Market Stats: price={} volume_24h={}",
                stats.last_price,
                stats.volume_24h
            );
        }
        Err(e) => {
            eprintln!("01.xyz API unavailable (expected in CI): {}", e);
        }
    }
}

/// Test that get_all_market_stats returns stats for multiple symbols
#[tokio::test]
async fn test_get_all_market_stats() {
    let client = O1Client::new();

    match client.get_all_market_stats().await {
        Ok(stats) => {
            assert!(
                !stats.is_empty(),
                "Should return stats for at least one symbol"
            );

            tracing::info!(
                "Successfully fetched market stats for {} symbols",
                stats.len()
            );
        }
        Err(e) => {
            eprintln!("01.xyz API unavailable (expected in CI): {}", e);
        }
    }
}

/// Test that is_supported returns true for BTC
#[tokio::test]
async fn test_is_supported_btc() {
    let client = O1Client::new();

    match client.is_supported("BTC").await {
        Ok(supported) => {
            assert!(supported, "BTC should be supported on 01");
        }
        Err(e) => {
            eprintln!("01.xyz API unavailable (expected in CI): {}", e);
        }
    }
}

/// Test that is_supported returns false for an invalid symbol
#[tokio::test]
async fn test_is_supported_invalid() {
    let client = O1Client::new();

    match client.is_supported("ZZZZZ").await {
        Ok(supported) => {
            assert!(!supported, "ZZZZZ should not be supported on 01");
        }
        Err(e) => {
            eprintln!("01.xyz API unavailable (expected in CI): {}", e);
        }
    }
}

/// Test symbol parsing from global format to Nord API format
#[test]
fn test_parse_symbol() {
    let client = O1Client::new();

    assert_eq!(client.parse_symbol("BTC"), "BTCUSD");
    assert_eq!(client.parse_symbol("BTCUSD"), "BTCUSD");
}
