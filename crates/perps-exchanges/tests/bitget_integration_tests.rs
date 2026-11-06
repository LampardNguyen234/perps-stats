//! Integration tests for the Bitget exchange client.
//!
//! These tests make real API calls to `api.bitget.com` (USDT-FUTURES only) and require
//! network access. Following the project-wide pattern (see `edgex_integration_tests.rs`),
//! each test matches `Ok`/`Err` and prints a skip message on failure rather than panicking,
//! so a transient network/API outage does not fail the suite.
//!
//! Run with: `cargo test --test bitget_integration_tests`

use perps_core::IPerps;
use perps_exchanges::BitgetClient;
use rust_decimal::Decimal;

const TEST_SYMBOL: &str = "BTC";
const UNSUPPORTED_SYMBOL: &str = "THIS_SYMBOL_DOES_NOT_EXIST_XYZ";

#[test]
fn test_bitget_client_creation() {
    let client = BitgetClient::new();
    assert_eq!(client.get_name(), "bitget");
}

#[tokio::test]
async fn test_get_markets() {
    let client = BitgetClient::new();
    match client.get_markets().await {
        Ok(markets) => {
            assert!(
                markets.len() >= 100,
                "expected at least 100 active USDT-FUTURES markets, got {}",
                markets.len()
            );
            assert!(markets.iter().any(|m| m.symbol == "BTC"));
            for m in markets.iter().take(3) {
                assert!(!m.symbol.is_empty());
                assert!(m.contract.ends_with("USDT"));
                assert!(m.max_leverage > Decimal::ZERO);
            }
            println!("Fetched {} active Bitget markets", markets.len());
        }
        Err(e) => println!("Bitget get_markets skipped (API unavailable): {}", e),
    }
}

#[tokio::test]
async fn test_get_market() {
    let client = BitgetClient::new();
    match client.get_market(TEST_SYMBOL).await {
        Ok(market) => {
            assert_eq!(market.symbol, "BTC");
            assert!(market.max_leverage > Decimal::ZERO);
            assert!(market.min_order_qty > Decimal::ZERO);
        }
        Err(e) => println!("Bitget get_market skipped (API unavailable): {}", e),
    }
}

#[tokio::test]
async fn test_get_ticker() {
    let client = BitgetClient::new();
    match client.get_ticker(TEST_SYMBOL).await {
        Ok(ticker) => {
            assert_eq!(ticker.symbol, "BTC");
            assert!(ticker.last_price > Decimal::ZERO);
            assert!(ticker.mark_price > Decimal::ZERO);
            assert!(ticker.volume_24h > Decimal::ZERO);
            assert!(ticker.turnover_24h > Decimal::ZERO);
            assert!(ticker.open_interest > Decimal::ZERO);
            assert!(
                ticker.price_change_pct.abs() < Decimal::ONE,
                "price_change_pct {} should be a decimal ratio, not a percentage",
                ticker.price_change_pct
            );
        }
        Err(e) => println!("Bitget get_ticker skipped (API unavailable): {}", e),
    }
}

#[tokio::test]
async fn test_get_all_tickers() {
    let client = BitgetClient::new();
    match client.get_all_tickers().await {
        Ok(tickers) => {
            assert!(tickers.len() >= 100, "expected many tickers");
            assert!(tickers.iter().any(|t| t.symbol == "BTC"));
        }
        Err(e) => println!("Bitget get_all_tickers skipped (API unavailable): {}", e),
    }
}

#[tokio::test]
async fn test_get_orderbook() {
    let client = BitgetClient::new();
    match client.get_orderbook(TEST_SYMBOL, 50).await {
        Ok(book) => {
            let ob = book.best_for_tight_spreads().unwrap();
            assert!(!ob.bids.is_empty());
            assert!(!ob.asks.is_empty());
            // sorted + no crossed book
            assert!(ob.bids.windows(2).all(|w| w[0].price >= w[1].price));
            assert!(ob.asks.windows(2).all(|w| w[0].price <= w[1].price));
            assert!(ob.bids[0].price < ob.asks[0].price);
        }
        Err(e) => println!("Bitget get_orderbook skipped (API unavailable): {}", e),
    }
}

#[tokio::test]
async fn test_get_funding_rate() {
    let client = BitgetClient::new();
    match client.get_funding_rate(TEST_SYMBOL).await {
        Ok(fr) => {
            assert_eq!(fr.symbol, "BTC");
            assert!(
                fr.funding_rate.abs() < Decimal::ONE,
                "funding_rate {} should be a decimal ratio, not a percentage",
                fr.funding_rate
            );
            assert!(fr.funding_interval > 0);
        }
        Err(e) => println!("Bitget get_funding_rate skipped (API unavailable): {}", e),
    }
}

#[tokio::test]
async fn test_get_funding_rate_history() {
    let client = BitgetClient::new();
    match client
        .get_funding_rate_history(TEST_SYMBOL, None, None, Some(10))
        .await
    {
        Ok(rates) => {
            assert!(!rates.is_empty());
            assert!(rates.len() <= 10);
            for r in &rates {
                assert_eq!(r.symbol, "BTC");
            }
        }
        Err(e) => println!(
            "Bitget get_funding_rate_history skipped (API unavailable): {}",
            e
        ),
    }
}

#[tokio::test]
async fn test_get_open_interest() {
    let client = BitgetClient::new();
    match client.get_open_interest(TEST_SYMBOL).await {
        Ok(oi) => {
            assert_eq!(oi.symbol, "BTC");
            assert!(oi.open_interest > Decimal::ZERO);
            assert!(oi.open_value > Decimal::ZERO);
        }
        Err(e) => println!("Bitget get_open_interest skipped (API unavailable): {}", e),
    }
}

#[tokio::test]
async fn test_get_klines() {
    let client = BitgetClient::new();
    match client
        .get_klines(TEST_SYMBOL, "1h", None, None, Some(10))
        .await
    {
        Ok(klines) => {
            assert!(!klines.is_empty());
            for k in &klines {
                assert!(k.high >= k.low);
                assert!(k.volume >= Decimal::ZERO);
            }
        }
        Err(e) => println!("Bitget get_klines skipped (API unavailable): {}", e),
    }
}

#[tokio::test]
async fn test_get_klines_rejects_unsupported_interval() {
    let client = BitgetClient::new();
    let result = client.get_klines(TEST_SYMBOL, "1w", None, None, None).await;
    assert!(result.is_err(), "1w should be rejected (no Bitget enum)");
}

#[tokio::test]
async fn test_get_recent_trades() {
    let client = BitgetClient::new();
    match client.get_recent_trades(TEST_SYMBOL, 10).await {
        Ok(trades) => {
            assert!(!trades.is_empty());
            for t in &trades {
                assert!(t.price > Decimal::ZERO);
                assert!(t.quantity > Decimal::ZERO);
            }
        }
        Err(e) => println!("Bitget get_recent_trades skipped (API unavailable): {}", e),
    }
}

#[tokio::test]
async fn test_get_market_stats() {
    let client = BitgetClient::new();
    match client.get_market_stats(TEST_SYMBOL).await {
        Ok(stats) => {
            assert_eq!(stats.symbol, "BTC");
            assert!(stats.last_price > Decimal::ZERO);
            assert!(stats.volume_24h > Decimal::ZERO);
            assert!(stats.open_interest > Decimal::ZERO);
        }
        Err(e) => println!("Bitget get_market_stats skipped (API unavailable): {}", e),
    }
}

#[tokio::test]
#[ignore] // one call for all symbols, but still slow enough in CI to opt in explicitly
async fn test_get_all_market_stats() {
    let client = BitgetClient::new();
    match client.get_all_market_stats().await {
        Ok(stats) => {
            assert!(!stats.is_empty());
            assert!(stats.iter().any(|s| s.symbol == "BTC"));
        }
        Err(e) => println!(
            "Bitget get_all_market_stats skipped (API unavailable): {}",
            e
        ),
    }
}

#[tokio::test]
async fn test_is_supported() {
    let client = BitgetClient::new();
    match client.is_supported(TEST_SYMBOL).await {
        Ok(supported) => assert!(supported, "BTC should be supported on Bitget"),
        Err(e) => println!("Bitget is_supported skipped (API unavailable): {}", e),
    }
    match client.is_supported(UNSUPPORTED_SYMBOL).await {
        Ok(supported) => assert!(!supported, "bogus symbol should not be supported"),
        Err(e) => println!("Bitget is_supported skipped (API unavailable): {}", e),
    }
}
