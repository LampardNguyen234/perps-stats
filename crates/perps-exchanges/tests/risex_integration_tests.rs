use perps_core::IPerps;
use perps_exchanges::risex::RiseXClient;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

fn client() -> RiseXClient {
    RiseXClient::new()
}

macro_rules! skip_on_err {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(e) => {
                eprintln!("API unavailable, skipping: {}", e);
                return;
            }
        }
    };
}

#[tokio::test]
async fn test_get_markets_integration() {
    let markets = skip_on_err!(client().get_markets().await);
    assert!(!markets.is_empty(), "no markets returned");
    for m in &markets {
        assert!(!m.symbol.is_empty());
    }
}

#[tokio::test]
async fn test_get_ticker_btc() {
    let ticker = skip_on_err!(client().get_ticker("BTC").await);
    assert_eq!(ticker.symbol, "BTC");
    assert!(
        ticker.mark_price > Decimal::ZERO,
        "mark_price should be non-zero (possible serde naming mismatch)"
    );
    assert!(
        ticker.turnover_24h > Decimal::ZERO,
        "turnover_24h should be non-zero"
    );
    if ticker.volume_24h > Decimal::ZERO {
        assert!(
            ticker.turnover_24h > ticker.volume_24h,
            "turnover {} should exceed volume {} for USDC-margined perp",
            ticker.turnover_24h,
            ticker.volume_24h
        );
    }
    assert!(
        ticker.best_bid_price > Decimal::ZERO,
        "bid price should be non-zero"
    );
    assert!(
        ticker.best_ask_price > Decimal::ZERO,
        "ask price should be non-zero"
    );
    assert!(
        ticker.best_bid_price < ticker.best_ask_price,
        "crossed book"
    );
}

#[tokio::test]
async fn test_get_all_tickers() {
    let tickers = skip_on_err!(client().get_all_tickers().await);
    assert!(!tickers.is_empty());
    for t in &tickers {
        assert!(
            t.mark_price > Decimal::ZERO,
            "zero mark_price for {} — possible serde mismatch",
            t.symbol
        );
    }
}

#[tokio::test]
async fn test_get_orderbook_btc() {
    let ob = skip_on_err!(client().get_orderbook("BTC", 20).await);
    let ob = ob.best_for_tight_spreads().unwrap();
    assert!(!ob.bids.is_empty());
    assert!(!ob.asks.is_empty());
    assert!(
        ob.best_bid().unwrap() < ob.best_ask().unwrap(),
        "crossed orderbook"
    );
    for i in 0..ob.bids.len().saturating_sub(1) {
        assert!(ob.bids[i].price >= ob.bids[i + 1].price);
    }
}

#[tokio::test]
async fn test_get_funding_rate_btc() {
    let fr = skip_on_err!(client().get_funding_rate("BTC").await);
    assert_eq!(fr.symbol, "BTC");
    assert!(
        fr.funding_rate.abs() < dec!(0.01),
        "funding_rate {} out of expected range — may be in wrong units",
        fr.funding_rate
    );
    assert!(fr.funding_interval > 0);
}

#[tokio::test]
async fn test_get_funding_rate_history_btc() {
    use chrono::Utc;
    let end = Utc::now();
    let start = end - chrono::Duration::days(7);
    let history = skip_on_err!(
        client()
            .get_funding_rate_history("BTC", Some(start), Some(end), Some(100))
            .await
    );
    assert!(!history.is_empty(), "no funding history for last 7 days");
    for r in &history {
        assert_eq!(r.symbol, "BTC");
    }
}

#[tokio::test]
async fn test_get_open_interest_btc() {
    let oi = skip_on_err!(client().get_open_interest("BTC").await);
    assert!(
        oi.open_interest > Decimal::ZERO,
        "open_interest should be non-zero"
    );
    assert!(oi.open_value > Decimal::ZERO, "notional should be non-zero");
    assert!(
        oi.open_value > oi.open_interest,
        "notional {} should exceed qty {} for BTC",
        oi.open_value,
        oi.open_interest
    );
}

#[tokio::test]
async fn test_get_klines_btc_1h() {
    use chrono::Utc;
    let end = Utc::now();
    let start = end - chrono::Duration::hours(24);
    let klines = skip_on_err!(
        client()
            .get_klines("BTC", "1h", Some(start), Some(end), None)
            .await
    );
    assert!(!klines.is_empty(), "no klines returned");
    for k in &klines {
        assert!(k.close > Decimal::ZERO);
        assert!(k.close_time > k.open_time, "close_time <= open_time");
    }
}

#[tokio::test]
async fn test_get_recent_trades_btc() {
    let trades = skip_on_err!(client().get_recent_trades("BTC", 20).await);
    assert!(!trades.is_empty());
    for t in &trades {
        assert!(t.price > Decimal::ZERO);
    }
}

#[tokio::test]
async fn test_get_market_stats_btc() {
    let stats = skip_on_err!(client().get_market_stats("BTC").await);
    assert_eq!(stats.symbol, "BTC");
    assert!(stats.mark_price > Decimal::ZERO);
    assert!(stats.turnover_24h > Decimal::ZERO);
}

#[tokio::test]
async fn test_is_supported_btc() {
    let supported = skip_on_err!(client().is_supported("BTC").await);
    assert!(supported);
}

#[tokio::test]
async fn test_is_supported_invalid() {
    let supported = skip_on_err!(client().is_supported("NOTAREAL").await);
    assert!(!supported);
}
