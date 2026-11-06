use crate::bitget::types::*;
use anyhow::Result;
use chrono::{DateTime, TimeZone, Utc};
use perps_core::types::*;
use rust_decimal::Decimal;
use std::str::FromStr;

/// Parses a Bitget numeric string field into a `Decimal`.
pub fn parse_decimal(s: &str) -> Result<Decimal> {
    Ok(Decimal::from_str(s)?)
}

/// Parses a Bitget numeric field that may arrive as either a JSON string or a raw
/// JSON number (observed: `tickers`/`fills` quote numbers, `orderbook` does not).
pub fn parse_decimal_value(v: &serde_json::Value) -> Result<Decimal> {
    match v {
        serde_json::Value::String(s) => parse_decimal(s),
        serde_json::Value::Number(n) => Ok(Decimal::from_str(&n.to_string())?),
        other => Err(anyhow::anyhow!("expected numeric value, got: {:?}", other)),
    }
}

/// Bitget timestamps are Unix milliseconds encoded as a JSON string, everywhere.
pub fn unix_millis_str_to_datetime(s: &str) -> Result<DateTime<Utc>> {
    let millis = s.parse::<i64>()?;
    Utc.timestamp_millis_opt(millis)
        .single()
        .ok_or_else(|| anyhow::anyhow!("invalid Bitget timestamp (ms): {}", s))
}

pub fn instrument_to_market(i: &Instrument, global_symbol: String) -> Result<Market> {
    Ok(Market {
        symbol: global_symbol,
        contract: i.symbol.clone(),
        contract_size: Decimal::ONE,
        price_scale: i.price_precision.parse::<i32>()?,
        quantity_scale: i.quantity_precision.parse::<i32>()?,
        min_order_qty: parse_decimal(&i.min_order_qty)?,
        max_order_qty: parse_decimal(&i.max_order_qty)?,
        min_order_value: parse_decimal(&i.min_order_amount)?,
        max_leverage: parse_decimal(&i.max_leverage)?,
    })
}

pub fn ticker_data_to_ticker(t: &TickerData, global_symbol: String) -> Result<Ticker> {
    let last_price = parse_decimal(&t.last_price)?;
    let open_price_24h = parse_decimal(&t.open_price_24h)?;
    let mark_price = parse_decimal(&t.mark_price)?;
    let open_interest = parse_decimal(&t.open_interest)?;

    Ok(Ticker {
        symbol: global_symbol,
        last_price,
        mark_price,
        index_price: parse_decimal(&t.index_price)?,
        best_bid_price: parse_decimal(&t.bid1_price)?,
        best_bid_qty: parse_decimal(&t.bid1_size)?,
        best_ask_price: parse_decimal(&t.ask1_price)?,
        best_ask_qty: parse_decimal(&t.ask1_size)?,
        volume_24h: parse_decimal(&t.volume24h)?,
        turnover_24h: parse_decimal(&t.turnover24h)?,
        open_interest,
        open_interest_notional: open_interest * mark_price,
        price_change_24h: last_price - open_price_24h,
        price_change_pct: parse_decimal(&t.price_24h_pcnt)?,
        high_price_24h: parse_decimal(&t.high_price_24h)?,
        low_price_24h: parse_decimal(&t.low_price_24h)?,
        timestamp: unix_millis_str_to_datetime(&t.ts)?,
    })
}

pub fn funding_data_to_funding_rate(
    f: &FundingRateData,
    global_symbol: String,
) -> Result<FundingRate> {
    Ok(FundingRate {
        symbol: global_symbol,
        funding_rate: parse_decimal(&f.funding_rate)?,
        predicted_rate: parse_decimal(&f.next_funding_rate)?,
        funding_time: Utc::now(),
        next_funding_time: unix_millis_str_to_datetime(&f.next_update)?,
        funding_interval: f.funding_rate_interval.parse::<i32>()?,
        funding_rate_cap_floor: parse_decimal(&f.max_funding_rate)?,
    })
}

pub fn fill_to_trade(f: &FillData, global_symbol: String) -> Result<Trade> {
    Ok(Trade {
        id: f.exec_id.clone(),
        symbol: global_symbol,
        price: parse_decimal(&f.price)?,
        quantity: parse_decimal(&f.size)?,
        side: if f.side == "buy" {
            OrderSide::Buy
        } else {
            OrderSide::Sell
        },
        timestamp: unix_millis_str_to_datetime(&f.ts)?,
    })
}

/// `history-fund-rate` doesn't provide interval/next-funding/predicted-rate/cap-floor —
/// those fields are zeroed, matching the project's convention for history-only endpoints
/// (see `bybit::client::get_funding_rate_history`).
pub fn history_entry_to_funding_rate(
    e: &HistoryFundEntry,
    global_symbol: String,
) -> Result<FundingRate> {
    Ok(FundingRate {
        symbol: global_symbol,
        funding_rate: parse_decimal(&e.funding_rate)?,
        predicted_rate: Decimal::ZERO,
        funding_time: unix_millis_str_to_datetime(&e.funding_rate_timestamp)?,
        next_funding_time: Utc.timestamp_millis_opt(0).single().unwrap_or_default(),
        funding_interval: 0,
        funding_rate_cap_floor: Decimal::ZERO,
    })
}

/// Candle rows are positional: `[ts, open, high, low, close, volume_base, turnover_quote]`.
/// Bitget doesn't return a close time, so it's derived from the (global) interval length.
pub fn candle_row_to_kline(
    row: &[String],
    global_symbol: String,
    interval: &str,
    interval_duration: chrono::Duration,
) -> Result<Kline> {
    if row.len() < 7 {
        anyhow::bail!("candle row has {} fields, expected 7", row.len());
    }
    let open_time = unix_millis_str_to_datetime(&row[0])?;
    Ok(Kline {
        symbol: global_symbol,
        interval: interval.to_string(),
        open_time,
        close_time: open_time + interval_duration,
        open: parse_decimal(&row[1])?,
        high: parse_decimal(&row[2])?,
        low: parse_decimal(&row[3])?,
        close: parse_decimal(&row[4])?,
        volume: parse_decimal(&row[5])?,
        turnover: parse_decimal(&row[6])?,
    })
}

/// `MarketStats` needs `funding_rate`, which `Ticker` doesn't carry — convert directly
/// from the raw `tickers` payload instead of going through `Ticker`, so this stays a
/// single API call (see 02_phase_2_rest_extended.md's "derive from tickers" decision).
pub fn ticker_data_to_market_stats(t: &TickerData, global_symbol: String) -> Result<MarketStats> {
    let last_price = parse_decimal(&t.last_price)?;
    let open_price_24h = parse_decimal(&t.open_price_24h)?;

    Ok(MarketStats {
        symbol: global_symbol,
        volume_24h: parse_decimal(&t.volume24h)?,
        turnover_24h: parse_decimal(&t.turnover24h)?,
        open_interest: parse_decimal(&t.open_interest)?,
        funding_rate: parse_decimal(&t.funding_rate)?,
        last_price,
        mark_price: parse_decimal(&t.mark_price)?,
        index_price: parse_decimal(&t.index_price)?,
        price_change_24h: last_price - open_price_24h,
        price_change_pct: parse_decimal(&t.price_24h_pcnt)?,
        high_price_24h: parse_decimal(&t.high_price_24h)?,
        low_price_24h: parse_decimal(&t.low_price_24h)?,
        timestamp: unix_millis_str_to_datetime(&t.ts)?,
    })
}

pub fn orderbook_data_to_orderbook(ob: &OrderbookData, global_symbol: String) -> Result<Orderbook> {
    let bids =
        ob.b.iter()
            .map(|[price, qty]| {
                Ok(OrderbookLevel {
                    price: parse_decimal_value(price)?,
                    quantity: parse_decimal_value(qty)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

    let asks =
        ob.a.iter()
            .map(|[price, qty]| {
                Ok(OrderbookLevel {
                    price: parse_decimal_value(price)?,
                    quantity: parse_decimal_value(qty)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

    Ok(Orderbook {
        symbol: global_symbol,
        bids,
        asks,
        timestamp: unix_millis_str_to_datetime(&ob.ts)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_decimal() {
        assert_eq!(parse_decimal("79999.9").unwrap(), Decimal::new(799999, 1));
    }

    #[test]
    fn test_candle_row_to_kline_index_order() {
        // [ts, open, high, low, close, volume_base, turnover_quote]
        let row = vec![
            "1793068920000".to_string(),
            "79916.2".to_string(),
            "79984".to_string(),
            "79916.2".to_string(),
            "79974.2".to_string(),
            "8.3655".to_string(),
            "668973.42515".to_string(),
        ];
        let kline =
            candle_row_to_kline(&row, "BTC".to_string(), "1m", chrono::Duration::minutes(1))
                .unwrap();
        assert_eq!(kline.open, Decimal::from_str("79916.2").unwrap());
        assert_eq!(kline.high, Decimal::from_str("79984").unwrap());
        assert_eq!(kline.low, Decimal::from_str("79916.2").unwrap());
        assert_eq!(kline.close, Decimal::from_str("79974.2").unwrap());
        assert_eq!(kline.volume, Decimal::from_str("8.3655").unwrap());
        assert_eq!(kline.turnover, Decimal::from_str("668973.42515").unwrap());
        assert_eq!(
            (kline.close_time - kline.open_time).num_seconds(),
            60,
            "close_time should be open_time + interval duration, not a hardcoded 60000ms"
        );
    }

    #[test]
    fn test_funding_rate_history_decimal_ratio_not_percentage() {
        let entry = HistoryFundEntry {
            symbol: "BTCUSDT".to_string(),
            funding_rate: "0.0001".to_string(),
            funding_rate_timestamp: "1793059200000".to_string(),
        };
        let fr = history_entry_to_funding_rate(&entry, "BTC".to_string()).unwrap();
        assert_eq!(fr.funding_rate, Decimal::from_str("0.0001").unwrap());
    }

    #[test]
    fn test_unix_millis_str_to_datetime() {
        let dt = unix_millis_str_to_datetime("1793064345061").unwrap();
        assert_eq!(dt.timestamp_millis(), 1793064345061);
    }

    #[test]
    fn test_ticker_data_to_ticker_field_semantics() {
        let t = TickerData {
            symbol: "BTCUSDT".to_string(),
            ts: "1793064345061".to_string(),
            last_price: "79999.9".to_string(),
            open_price_24h: "80176.9".to_string(),
            high_price_24h: "80812.6".to_string(),
            low_price_24h: "79320.1".to_string(),
            ask1_price: "80000".to_string(),
            bid1_price: "79999.9".to_string(),
            bid1_size: "0.8865".to_string(),
            ask1_size: "22.6613".to_string(),
            price_24h_pcnt: "-0.00221".to_string(),
            volume24h: "33376.6428".to_string(),
            turnover24h: "2670838113.01215".to_string(),
            index_price: "80033.0515".to_string(),
            mark_price: "79999.9".to_string(),
            funding_rate: "0.0001".to_string(),
            open_interest: "36849.998".to_string(),
        };

        let ticker = ticker_data_to_ticker(&t, "BTC".to_string()).unwrap();

        // volume vs turnover must not be swapped
        assert_eq!(ticker.volume_24h, Decimal::from_str("33376.6428").unwrap());
        assert_eq!(
            ticker.turnover_24h,
            Decimal::from_str("2670838113.01215").unwrap()
        );
        // decimal ratio, not a percentage
        assert_eq!(
            ticker.price_change_pct,
            Decimal::from_str("-0.00221").unwrap()
        );
        // OI notional derived as qty * mark_price
        assert_eq!(
            ticker.open_interest_notional,
            Decimal::from_str("36849.998").unwrap() * Decimal::from_str("79999.9").unwrap()
        );
    }

    #[test]
    fn test_orderbook_sorted_no_crossed_book() {
        let ob = OrderbookData {
            // Mixed string/number encodings, matching what's actually been observed live.
            a: vec![
                [serde_json::json!("79995.0"), serde_json::json!(7.7526)],
                [serde_json::json!(79996.0), serde_json::json!(1.0)],
            ],
            b: vec![
                [serde_json::json!("79994.9"), serde_json::json!(4.5227)],
                [serde_json::json!(79994.0), serde_json::json!(1.0)],
            ],
            ts: "1793064346256".to_string(),
        };

        let orderbook = orderbook_data_to_orderbook(&ob, "BTC".to_string()).unwrap();

        // asks ascending
        assert!(orderbook.asks[0].price < orderbook.asks[1].price);
        // bids descending
        assert!(orderbook.bids[0].price > orderbook.bids[1].price);
        // no crossed book: best bid < best ask
        assert!(orderbook.bids[0].price < orderbook.asks[0].price);
    }
}
