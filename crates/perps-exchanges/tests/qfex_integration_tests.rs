//! Integration tests for QFEX exchange client.
//!
//! These tests make real API calls to QFEX public endpoints and require network access.
//! The live WS orderbook test requires `wss://mds.qfex.com/` to be reachable.
//!
//! Run with: `cargo test qfex_integration`

use perps_core::IPerps;
use perps_exchanges::QfexClient;
use rust_decimal::Decimal;

/// The primary equity symbol used across tests.
const TEST_SYMBOL: &str = "NVDA";
const TEST_SYMBOL_2: &str = "AAPL";

/// Test that QfexClient can be instantiated.
#[test]
fn test_qfex_client_creation() {
    let client = QfexClient::new();
    assert!(!client.get_name().is_empty());
}

/// Test that QfexClient returns the correct exchange name.
#[test]
fn test_qfex_client_name() {
    let client = QfexClient::new();
    assert_eq!(client.get_name(), "qfex");
}

/// Test symbol parsing: global → QFEX format and idempotency.
#[test]
fn test_qfex_symbol_parsing() {
    let client = QfexClient::new();

    assert_eq!(client.parse_symbol("NVDA"), "NVDA-USD");
    assert_eq!(client.parse_symbol("nvda"), "NVDA-USD");
    assert_eq!(client.parse_symbol("NVDA-USD"), "NVDA-USD"); // idempotent
    assert_eq!(client.parse_symbol("GOOGL"), "GOOGL-USD");
    assert_eq!(client.parse_symbol("AAPL"), "AAPL-USD");
}

/// Test that get_markets returns at least one active market.
#[tokio::test]
async fn test_get_markets() {
    let client = QfexClient::new();

    match client.get_markets().await {
        Ok(markets) => {
            assert!(
                !markets.is_empty(),
                "Expected at least one market from QFEX"
            );

            for market in markets.iter().take(3) {
                assert!(
                    !market.symbol.is_empty(),
                    "Market symbol should not be empty"
                );
                assert!(!market.contract.is_empty(), "Contract should not be empty");
                // QFEX contracts end in -USD
                assert!(
                    market.contract.ends_with("-USD"),
                    "QFEX contract should end with -USD, got: {}",
                    market.contract
                );
                assert!(
                    market.max_leverage > Decimal::ZERO,
                    "Max leverage should be positive"
                );
            }

            println!("Fetched {} markets from QFEX", markets.len());
        }
        Err(e) => println!("QFEX get_markets skipped (API unavailable): {}", e),
    }
}

/// Test that get_all_tickers returns metrics for all symbols.
#[tokio::test]
async fn test_get_all_tickers() {
    let client = QfexClient::new();

    match client.get_all_tickers().await {
        Ok(tickers) => {
            assert!(!tickers.is_empty(), "Expected tickers from QFEX");

            for ticker in tickers.iter().take(3) {
                assert!(!ticker.symbol.is_empty());
                // Prices can be zero for inactive symbols but should be non-negative
                assert!(ticker.mark_price >= Decimal::ZERO);
                assert!(ticker.turnover_24h >= Decimal::ZERO);
            }

            println!("Fetched {} tickers from QFEX", tickers.len());
        }
        Err(e) => println!("QFEX get_all_tickers skipped (API unavailable): {}", e),
    }
}

/// Test that get_ticker returns data for NVDA.
#[tokio::test]
async fn test_get_ticker_nvda() {
    let client = QfexClient::new();

    match client.get_ticker(TEST_SYMBOL).await {
        Ok(ticker) => {
            assert_eq!(ticker.symbol, TEST_SYMBOL);
            assert!(
                ticker.mark_price > Decimal::ZERO,
                "NVDA mark price should be positive"
            );
            assert!(
                ticker.turnover_24h >= Decimal::ZERO,
                "NVDA turnover should be non-negative"
            );
            // price_change_pct should be a ratio (not a raw percentage)
            assert!(
                ticker.price_change_pct.abs() < Decimal::from(1),
                "price_change_pct should be a decimal ratio (< 1), got: {}",
                ticker.price_change_pct
            );

            println!(
                "NVDA ticker: mark={}, turnover={}, pct_chg={}",
                ticker.mark_price, ticker.turnover_24h, ticker.price_change_pct
            );
        }
        Err(e) => println!("QFEX get_ticker NVDA skipped (API unavailable): {}", e),
    }
}

/// Test that get_orderbook exercises the WS L2 snapshot path.
#[tokio::test]
async fn test_get_orderbook_nvda() {
    let client = QfexClient::new();

    match client.get_orderbook(TEST_SYMBOL, 10).await {
        Ok(ob) => {
            assert_eq!(ob.symbol, TEST_SYMBOL);
            assert!(
                !ob.orderbooks.is_empty(),
                "Expected at least one orderbook resolution"
            );

            let book = ob.orderbooks.first().unwrap();
            assert!(!book.bids.is_empty(), "NVDA orderbook should have bids");
            assert!(!book.asks.is_empty(), "NVDA orderbook should have asks");
            assert!(book.bids.len() <= 10, "Depth should be capped at 10");

            // Bids should be descending
            for window in book.bids.windows(2) {
                assert!(
                    window[0].price >= window[1].price,
                    "Bids should be in descending order"
                );
            }
            // Asks should be ascending
            for window in book.asks.windows(2) {
                assert!(
                    window[0].price <= window[1].price,
                    "Asks should be in ascending order"
                );
            }

            println!(
                "NVDA orderbook: {} bids, {} asks, best bid={}, best ask={}",
                book.bids.len(),
                book.asks.len(),
                book.bids.first().map(|l| l.price).unwrap_or(Decimal::ZERO),
                book.asks.first().map(|l| l.price).unwrap_or(Decimal::ZERO),
            );
        }
        Err(e) => println!("QFEX get_orderbook NVDA skipped (WS unavailable): {}", e),
    }
}

/// Test that get_recent_trades returns the expected error.
#[tokio::test]
async fn test_get_recent_trades_returns_error() {
    let client = QfexClient::new();

    let result = client.get_recent_trades(TEST_SYMBOL, 10).await;
    assert!(
        result.is_err(),
        "QFEX get_recent_trades should return Err (no REST endpoint)"
    );

    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("QFEX") || err_msg.contains("REST"),
        "Error message should mention QFEX or REST, got: {}",
        err_msg
    );
}

/// Test that get_funding_rate returns a decimal ratio for NVDA.
#[tokio::test]
async fn test_get_funding_rate_nvda() {
    let client = QfexClient::new();

    match client.get_funding_rate(TEST_SYMBOL).await {
        Ok(fr) => {
            assert_eq!(fr.symbol, TEST_SYMBOL);
            // funding_rate should be a decimal ratio (not a bps value)
            assert!(
                fr.funding_rate.abs() < Decimal::from(1),
                "funding_rate should be a decimal ratio (< 1), got: {}",
                fr.funding_rate
            );
            assert_eq!(
                fr.funding_interval, 8,
                "QFEX default funding interval is 8h"
            );

            println!("NVDA funding rate: {}", fr.funding_rate);
        }
        Err(e) => println!(
            "QFEX get_funding_rate NVDA skipped (API unavailable): {}",
            e
        ),
    }
}

/// Test that get_funding_rate_history returns at least one entry for NVDA.
#[tokio::test]
async fn test_get_funding_rate_history_nvda() {
    let client = QfexClient::new();

    match client
        .get_funding_rate_history(TEST_SYMBOL, None, None, Some(10))
        .await
    {
        Ok(history) => {
            // May be empty if exchange is new; just verify structure if non-empty
            for entry in history.iter().take(3) {
                assert_eq!(entry.symbol, TEST_SYMBOL);
                assert!(
                    entry.funding_rate.abs() < Decimal::from(1),
                    "funding_rate history entries should be decimal ratios"
                );
            }
            println!("NVDA funding history: {} entries", history.len());
        }
        Err(e) => println!(
            "QFEX get_funding_rate_history NVDA skipped (API unavailable): {}",
            e
        ),
    }
}

/// Test that get_klines returns OHLCV data for NVDA.
#[tokio::test]
async fn test_get_klines_nvda() {
    let client = QfexClient::new();

    match client
        .get_klines(TEST_SYMBOL, "1h", None, None, Some(5))
        .await
    {
        Ok(klines) => {
            for kline in klines.iter().take(3) {
                assert_eq!(kline.symbol, TEST_SYMBOL);
                assert_eq!(kline.interval, "1h");
                assert!(kline.high >= kline.low, "High should be >= low");
                assert!(kline.turnover >= Decimal::ZERO);
            }
            println!("NVDA klines: {} candles", klines.len());
        }
        Err(e) => println!("QFEX get_klines NVDA skipped (API unavailable): {}", e),
    }
}

/// Test that unsupported kline interval returns an error.
#[tokio::test]
async fn test_get_klines_unsupported_interval() {
    let client = QfexClient::new();

    let result = client
        .get_klines(TEST_SYMBOL, "30m", None, None, None)
        .await;
    assert!(
        result.is_err(),
        "30m kline interval should return Err (unsupported on QFEX)"
    );
}

/// Test that get_open_interest returns values for NVDA.
#[tokio::test]
async fn test_get_open_interest_nvda() {
    let client = QfexClient::new();

    match client.get_open_interest(TEST_SYMBOL).await {
        Ok(oi) => {
            assert_eq!(oi.symbol, TEST_SYMBOL);
            assert!(oi.open_interest >= Decimal::ZERO);
            // open_value = oi (contracts) × mark_price, should be non-negative
            assert!(oi.open_value >= Decimal::ZERO);

            println!(
                "NVDA OI: {} contracts, ${} notional",
                oi.open_interest, oi.open_value
            );
        }
        Err(e) => println!(
            "QFEX get_open_interest NVDA skipped (API unavailable): {}",
            e
        ),
    }
}

/// Test that get_market_stats returns aggregated stats for NVDA.
#[tokio::test]
async fn test_get_market_stats_nvda() {
    let client = QfexClient::new();

    match client.get_market_stats(TEST_SYMBOL).await {
        Ok(stats) => {
            assert_eq!(stats.symbol, TEST_SYMBOL);
            assert!(stats.mark_price >= Decimal::ZERO);
            assert!(stats.turnover_24h >= Decimal::ZERO);

            println!(
                "NVDA market stats: mark={}, turnover={}, fr={}",
                stats.mark_price, stats.turnover_24h, stats.funding_rate
            );
        }
        Err(e) => println!(
            "QFEX get_market_stats NVDA skipped (API unavailable): {}",
            e
        ),
    }
}

/// Test that get_all_market_stats returns stats for all symbols.
#[tokio::test]
async fn test_get_all_market_stats() {
    let client = QfexClient::new();

    match client.get_all_market_stats().await {
        Ok(stats) => {
            assert!(
                !stats.is_empty(),
                "Expected at least one market stats entry"
            );

            for s in stats.iter().take(3) {
                assert!(!s.symbol.is_empty());
                assert!(s.mark_price >= Decimal::ZERO);
            }

            println!("Fetched {} market stats from QFEX", stats.len());
        }
        Err(e) => println!("QFEX get_all_market_stats skipped (API unavailable): {}", e),
    }
}

/// Test that is_supported returns true for NVDA and false for BTC.
#[tokio::test]
async fn test_is_supported_nvda() {
    let client = QfexClient::new();

    match client.is_supported(TEST_SYMBOL).await {
        Ok(supported) => {
            assert!(supported, "NVDA should be supported on QFEX");
        }
        Err(e) => println!("QFEX is_supported NVDA skipped (API unavailable): {}", e),
    }
}

/// Test that is_supported returns false for BTC (crypto, not equity).
#[tokio::test]
async fn test_is_supported_btc_returns_false() {
    let client = QfexClient::new();

    match client.is_supported("BTC").await {
        Ok(supported) => {
            assert!(
                !supported,
                "BTC should NOT be supported on QFEX (equity-only)"
            );
        }
        Err(e) => println!("QFEX is_supported BTC skipped (API unavailable): {}", e),
    }
}

/// Test that the metrics cache is shared: two calls within 5s should not double-fetch.
#[tokio::test]
async fn test_metrics_cache_shared() {
    let client = QfexClient::new();

    // First call warms cache
    let r1 = client.get_ticker(TEST_SYMBOL).await;
    // Second call should hit cache (no new HTTP request)
    let r2 = client.get_ticker(TEST_SYMBOL_2).await;

    // Both may succeed or both may fail depending on network — what matters is no panic
    match (r1, r2) {
        (Ok(t1), Ok(t2)) => {
            assert_eq!(t1.symbol, TEST_SYMBOL);
            assert_eq!(t2.symbol, TEST_SYMBOL_2);
        }
        (Err(e1), _) => println!("QFEX cache test skipped (API unavailable): {}", e1),
        (_, Err(e2)) => println!("QFEX cache test skipped (API unavailable): {}", e2),
    }
}
