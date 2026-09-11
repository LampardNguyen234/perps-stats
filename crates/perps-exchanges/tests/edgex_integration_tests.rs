//! Integration tests for the EdgeX exchange client.
//!
//! These tests make real API calls to `edgex-prod-v2.edgex.exchange` and require network
//! access. Following the project-wide pattern (see `qfex_integration_tests.rs`), each test
//! matches `Ok`/`Err` and prints a skip message on failure rather than panicking, so a
//! transient network/API outage does not fail the suite.
//!
//! `test_get_all_tickers`/`test_get_all_market_stats` are `#[ignore]`d: a cold run costs
//! ~340 requests (~34s at the assumed 10 req/s limiter) across EdgeX's ~170 active markets.
//! Run explicitly with `cargo test --test edgex_integration_tests -- --ignored`.
//!
//! Run with: `cargo test --test edgex_integration_tests`

use perps_core::IPerps;
use perps_exchanges::EdgexClient;
use rust_decimal::Decimal;

const TEST_SYMBOL: &str = "BTC";
const TEST_SYMBOL_COMMODITY: &str = "XAU";
const TEST_SYMBOL_EQUITY: &str = "AAPL";
const UNSUPPORTED_SYMBOL: &str = "BOTZ";

#[test]
fn test_edgex_client_creation() {
    let client = EdgexClient::new();
    assert_eq!(client.get_name(), "edgex");
}

#[test]
fn test_edgex_symbol_parsing() {
    let client = EdgexClient::new();
    assert_eq!(client.parse_symbol("BTC"), "BTCUSDC");
    assert_eq!(client.parse_symbol("btc"), "BTCUSDC");
    assert_eq!(client.parse_symbol("BTCUSDC"), "BTCUSDC");
}

#[tokio::test]
async fn test_get_markets() {
    let client = EdgexClient::new();
    match client.get_markets().await {
        Ok(markets) => {
            assert!(
                markets.len() >= 100,
                "expected at least 100 active EdgeX markets, got {}",
                markets.len()
            );
            assert!(markets.iter().any(|m| m.symbol == "BTC"));
            assert!(
                markets.iter().any(|m| m.symbol == "XAU"),
                "commodity market XAU should be present (full-breadth scope)"
            );
            for m in markets.iter().take(3) {
                assert!(!m.symbol.is_empty());
                assert!(m.contract.ends_with("USDC"));
                assert!(m.max_leverage > Decimal::ZERO);
            }
            println!("Fetched {} active EdgeX markets", markets.len());
        }
        Err(e) => println!("EdgeX get_markets skipped (API unavailable): {}", e),
    }
}

#[tokio::test]
async fn test_get_market_btc() {
    let client = EdgexClient::new();
    match client.get_market(TEST_SYMBOL).await {
        Ok(m) => {
            assert_eq!(m.symbol, "BTC");
            assert_eq!(m.contract, "BTCUSDC");
        }
        Err(e) => println!("EdgeX get_market skipped (API unavailable): {}", e),
    }
}

/// Non-zero-value assertions per `docs/new_exchange_requirements.md` pitfall #21 — a ticker
/// with all-zero prices/volumes would pass a purely-structural check.
#[tokio::test]
async fn test_get_ticker_btc() {
    let client = EdgexClient::new();
    match client.get_ticker(TEST_SYMBOL).await {
        Ok(t) => {
            assert_eq!(t.symbol, "BTC");
            assert!(t.last_price > Decimal::ZERO);
            assert!(
                t.turnover_24h > t.volume_24h,
                "USDC-margined perp: turnover should exceed base volume"
            );
            assert!(
                t.best_bid_price > Decimal::ZERO && t.best_ask_price > Decimal::ZERO,
                "top-of-book must be merged in from getDepth, not left at zero"
            );
            assert!(
                t.price_change_pct.abs() < Decimal::ONE,
                "price_change_pct must be a ratio, not a percentage/bps"
            );
            println!(
                "BTC ticker: last={} bid={} ask={} vol={} turnover={}",
                t.last_price, t.best_bid_price, t.best_ask_price, t.volume_24h, t.turnover_24h
            );
        }
        Err(e) => println!("EdgeX get_ticker skipped (API unavailable): {}", e),
    }
}

#[tokio::test]
async fn test_get_ticker_commodity_and_equity() {
    let client = EdgexClient::new();
    for symbol in [TEST_SYMBOL_COMMODITY, TEST_SYMBOL_EQUITY] {
        match client.get_ticker(symbol).await {
            Ok(t) => {
                assert!(
                    t.last_price > Decimal::ZERO,
                    "{} last_price should be non-zero",
                    symbol
                );
            }
            Err(e) => println!(
                "EdgeX get_ticker({}) skipped (API unavailable): {}",
                symbol, e
            ),
        }
    }
}

#[tokio::test]
async fn test_get_orderbook_levels() {
    let client = EdgexClient::new();
    for depth in [15u32, 50u32] {
        match client.get_orderbook(TEST_SYMBOL, depth).await {
            Ok(ob) => {
                let book = ob.orderbooks.first().expect("at least one resolution");
                assert!(!book.bids.is_empty());
                assert!(!book.asks.is_empty());
                assert!(book.bids.len() <= depth as usize);
                assert!(book.asks.len() <= depth as usize);
                for w in book.bids.windows(2) {
                    assert!(w[0].price >= w[1].price, "bids must be descending");
                }
                for w in book.asks.windows(2) {
                    assert!(w[0].price <= w[1].price, "asks must be ascending");
                }
                assert!(
                    book.asks[0].price > book.bids[0].price,
                    "book must not be crossed"
                );
            }
            Err(e) => println!(
                "EdgeX get_orderbook(depth={}) skipped (API unavailable): {}",
                depth, e
            ),
        }
    }
}

#[tokio::test]
async fn test_get_funding_rate() {
    let client = EdgexClient::new();
    match client.get_funding_rate(TEST_SYMBOL).await {
        Ok(fr) => {
            assert_eq!(fr.symbol, "BTC");
            assert!(
                fr.funding_rate.abs() < Decimal::from_str_exact("0.01").unwrap(),
                "funding_rate should be a small decimal ratio"
            );
            assert!(fr.next_funding_time > fr.funding_time);
            assert!(fr.funding_interval > 0);
        }
        Err(e) => println!("EdgeX get_funding_rate skipped (API unavailable): {}", e),
    }
}

#[tokio::test]
async fn test_get_funding_rate_history() {
    let client = EdgexClient::new();
    match client
        .get_funding_rate_history(TEST_SYMBOL, None, None, Some(10))
        .await
    {
        Ok(rates) => {
            assert!(!rates.is_empty());
            assert!(rates.len() <= 10);
            for w in rates.windows(2) {
                assert!(
                    w[0].funding_time >= w[1].funding_time,
                    "expected newest-first ordering"
                );
            }
        }
        Err(e) => println!(
            "EdgeX get_funding_rate_history skipped (API unavailable): {}",
            e
        ),
    }
}

#[tokio::test]
async fn test_get_open_interest() {
    let client = EdgexClient::new();
    match client.get_open_interest(TEST_SYMBOL).await {
        Ok(oi) => {
            assert!(oi.open_interest > Decimal::ZERO);
            assert!(
                oi.open_value > oi.open_interest,
                "BTC price should exceed $1"
            );
        }
        Err(e) => println!("EdgeX get_open_interest skipped (API unavailable): {}", e),
    }
}

#[tokio::test]
async fn test_get_klines() {
    let client = EdgexClient::new();
    match client
        .get_klines(TEST_SYMBOL, "1h", None, None, Some(5))
        .await
    {
        Ok(klines) => {
            assert!(!klines.is_empty());
            assert!(klines.len() <= 5);
            for k in &klines {
                assert!(k.high >= k.low);
                assert!(k.volume > Decimal::ZERO);
                assert!(k.close_time > k.open_time);
            }
            for w in klines.windows(2) {
                assert!(
                    w[0].open_time <= w[1].open_time,
                    "expected oldest-first ordering"
                );
            }
        }
        Err(e) => println!("EdgeX get_klines skipped (API unavailable): {}", e),
    }
}

#[tokio::test]
async fn test_get_klines_rejects_unsupported_interval() {
    let client = EdgexClient::new();
    let result = client
        .get_klines(TEST_SYMBOL, "3m", None, None, Some(1))
        .await;
    assert!(
        result.is_err(),
        "unsupported interval should error, not silently succeed"
    );
}

#[tokio::test]
async fn test_get_market_stats() {
    let client = EdgexClient::new();
    match client.get_market_stats(TEST_SYMBOL).await {
        Ok(ms) => {
            assert!(ms.last_price > Decimal::ZERO);
            assert!(ms.turnover_24h > Decimal::ZERO);
        }
        Err(e) => println!("EdgeX get_market_stats skipped (API unavailable): {}", e),
    }
}

/// EdgeX has no REST trades endpoint — must error, never panic or return an empty `Ok`.
#[tokio::test]
async fn test_get_recent_trades_unsupported() {
    let client = EdgexClient::new();
    let result = client.get_recent_trades(TEST_SYMBOL, 10).await;
    assert!(
        result.is_err(),
        "EdgeX get_recent_trades must return Err (no REST trades endpoint)"
    );
}

#[tokio::test]
async fn test_is_supported() {
    let client = EdgexClient::new();
    match client.is_supported(TEST_SYMBOL).await {
        Ok(supported) => assert!(supported, "BTC should be supported"),
        Err(e) => println!("EdgeX is_supported(BTC) skipped (API unavailable): {}", e),
    }
    match client.is_supported(UNSUPPORTED_SYMBOL).await {
        Ok(supported) => assert!(!supported, "BOTZ has no EdgeX equivalent"),
        Err(e) => println!("EdgeX is_supported(BOTZ) skipped (API unavailable): {}", e),
    }
}

/// Expensive: fans out to ~170 active markets (~340 requests via the internal getTicker +
/// getDepth merge). Run explicitly: `cargo test --test edgex_integration_tests -- --ignored`.
#[tokio::test]
#[ignore]
async fn test_get_all_tickers_soak() {
    let client = EdgexClient::new();
    match client.get_all_tickers().await {
        Ok(tickers) => {
            assert!(tickers.len() >= 100);
            let unique_symbols: std::collections::HashSet<_> =
                tickers.iter().map(|t| t.symbol.clone()).collect();
            assert_eq!(
                unique_symbols.len(),
                tickers.len(),
                "no duplicate symbols expected"
            );
            assert!(
                tickers.iter().all(|t| t.last_price > Decimal::ZERO),
                "no all-zero tickers expected"
            );
            println!("Fetched {} tickers from EdgeX", tickers.len());
        }
        Err(e) => println!("EdgeX get_all_tickers skipped (API unavailable): {}", e),
    }
}

#[tokio::test]
#[ignore]
async fn test_get_all_market_stats_soak() {
    let client = EdgexClient::new();
    match client.get_all_market_stats().await {
        Ok(stats) => {
            assert!(stats.len() >= 100);
            println!("Fetched {} market stats from EdgeX", stats.len());
        }
        Err(e) => println!(
            "EdgeX get_all_market_stats skipped (API unavailable): {}",
            e
        ),
    }
}
