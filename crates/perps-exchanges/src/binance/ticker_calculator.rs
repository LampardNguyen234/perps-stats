use chrono::{Duration, Utc};
use perps_core::{Kline, Ticker};
use rust_decimal::Decimal;

/// Convert timeframe string to Binance interval and duration
pub fn parse_timeframe(timeframe: &str) -> anyhow::Result<(String, Duration)> {
    match timeframe.to_lowercase().as_str() {
        "5m" => Ok(("5m".to_string(), Duration::minutes(5))),
        "15m" => Ok(("15m".to_string(), Duration::minutes(15))),
        "30m" => Ok(("30m".to_string(), Duration::minutes(30))),
        "1h" => Ok(("1h".to_string(), Duration::hours(1))),
        "4h" => Ok(("4h".to_string(), Duration::hours(4))),
        "24h" | "1d" => Ok(("1d".to_string(), Duration::hours(24))),
        _ => Err(anyhow::anyhow!(
            "Invalid timeframe: {}. Supported: 5m, 15m, 30m, 1h, 4h, 24h",
            timeframe
        )),
    }
}

/// Calculate ticker statistics from klines data
/// This allows us to generate ticker-like data for any timeframe
#[allow(clippy::too_many_arguments)]
pub fn calculate_ticker_from_klines(
    symbol: &str,
    klines: &[Kline],
    current_price: Decimal,
    mark_price: Decimal,
    index_price: Decimal,
    best_bid: Decimal,
    best_bid_qty: Decimal,
    best_ask: Decimal,
    best_ask_qty: Decimal,
) -> anyhow::Result<Ticker> {
    if klines.is_empty() {
        return Err(anyhow::anyhow!("No klines data available"));
    }

    // Get first kline
    let first_kline = &klines[0];

    // Calculate high/low across all klines
    let mut high = klines[0].high;
    let mut low = klines[0].low;
    let mut total_volume = Decimal::ZERO;
    let mut total_turnover = Decimal::ZERO;

    for kline in klines {
        if kline.high > high {
            high = kline.high;
        }
        if kline.low < low {
            low = kline.low;
        }
        total_volume += kline.volume;
        total_turnover += kline.turnover;
    }

    // Calculate price change
    let open_price = first_kline.open;
    let price_change = current_price - open_price;
    let price_change_pct = if open_price > Decimal::ZERO {
        price_change / open_price
    } else {
        Decimal::ZERO
    };

    Ok(Ticker {
        symbol: symbol.to_string(),
        last_price: current_price,
        mark_price,
        index_price,
        best_bid_price: best_bid,
        best_bid_qty,
        best_ask_price: best_ask,
        best_ask_qty,
        volume_24h: total_volume,
        turnover_24h: total_turnover,
        price_change_24h: price_change,
        price_change_pct,
        high_price_24h: high,
        low_price_24h: low,
        timestamp: Utc::now(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_parse_timeframe() {
        assert_eq!(parse_timeframe("5m").unwrap().0, "5m");
        assert_eq!(parse_timeframe("1h").unwrap().0, "1h");
        assert_eq!(parse_timeframe("24h").unwrap().0, "1d");
        assert!(parse_timeframe("invalid").is_err());
    }

    #[test]
    fn test_calculate_ticker_from_klines() {
        let klines = vec![
            Kline {
                symbol: "BTC-USDT".to_string(),
                interval: "5m".to_string(),
                open_time: Utc::now() - Duration::minutes(10),
                close_time: Utc::now() - Duration::minutes(5),
                open: dec!(50000),
                high: dec!(50500),
                low: dec!(49800),
                close: dec!(50200),
                volume: dec!(100),
                turnover: dec!(5000000),
            },
            Kline {
                symbol: "BTC-USDT".to_string(),
                interval: "5m".to_string(),
                open_time: Utc::now() - Duration::minutes(5),
                close_time: Utc::now(),
                open: dec!(50200),
                high: dec!(50800),
                low: dec!(50100),
                close: dec!(50600),
                volume: dec!(150),
                turnover: dec!(7500000),
            },
        ];

        let ticker = calculate_ticker_from_klines(
            "BTC-USDT",
            &klines,
            dec!(50600),
            dec!(50610),
            dec!(50595),
            dec!(50599),
            dec!(10.5),
            dec!(50601),
            dec!(8.3),
        )
        .unwrap();

        assert_eq!(ticker.symbol, "BTC-USDT");
        assert_eq!(ticker.last_price, dec!(50600));
        assert_eq!(ticker.high_price_24h, dec!(50800));
        assert_eq!(ticker.low_price_24h, dec!(49800));
        assert_eq!(ticker.volume_24h, dec!(250));
    }
}
