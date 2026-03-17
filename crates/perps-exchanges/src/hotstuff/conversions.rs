use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, TimeZone, Utc};
use perps_core::{
    FundingRate, Kline, Market, MarketStats, OpenInterest, Orderbook, OrderbookLevel, OrderSide,
    Ticker, Trade,
};
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;

use super::types::{HotstuffInstrument, HotstuffKline, HotstuffLevel, HotstuffOrderbook,
    HotstuffTicker, HotstuffTrade};

// ---- helpers ----

/// Count the number of significant decimal places in a Decimal value.
/// For example: 0.001 → 3, 0.5 → 1, 1.0 → 0.
fn decimal_scale(d: Decimal) -> Option<i32> {
    if d <= Decimal::ZERO {
        return None;
    }
    // Use the scale() method which returns the number of decimal digits
    Some(d.scale() as i32)
}

fn parse_decimal(s: &str, field: &str) -> Result<Decimal> {
    Decimal::from_str_exact(s)
        .or_else(|_| s.parse::<f64>().ok().and_then(Decimal::from_f64).ok_or_else(|| anyhow!("")))
        .with_context(|| format!("Failed to parse {} as decimal: {:?}", field, s))
}

fn parse_decimal_opt(opt: &Option<String>, field: &str) -> Decimal {
    opt.as_deref()
        .and_then(|s| parse_decimal(s, field).ok())
        .unwrap_or(Decimal::ZERO)
}

fn unix_ms_to_datetime(ms: i64) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(ms).single().unwrap_or_else(Utc::now)
}

fn f64_to_decimal(v: f64, field: &str) -> Result<Decimal> {
    Decimal::from_f64(v).with_context(|| format!("Failed to convert f64 to decimal for {}", field))
}

// ---- instrument → market ----

/// Convert a Hotstuff instrument to a core `Market`.
/// `symbol` should be the global symbol (e.g. "BTC").
pub fn instrument_to_market(inst: &HotstuffInstrument, symbol: String) -> Result<Market> {
    let tick_size = inst
        .tick_size
        .and_then(Decimal::from_f64)
        .unwrap_or(Decimal::ONE);

    let lot_size = inst
        .lot_size
        .and_then(Decimal::from_f64)
        .unwrap_or(Decimal::ONE);

    let max_leverage = inst
        .max_leverage
        .map(Decimal::from)
        .unwrap_or_else(|| Decimal::from(20));

    let min_notional = inst
        .min_notional_usd
        .and_then(Decimal::from_f64)
        .unwrap_or(Decimal::ZERO);

    // Derive scale from tick/lot sizes: count decimal places
    let price_scale = decimal_scale(tick_size).unwrap_or(2);
    let quantity_scale = decimal_scale(lot_size).unwrap_or(3);

    Ok(Market {
        symbol,
        contract: inst.name.clone(),
        contract_size: Decimal::ONE,
        price_scale,
        quantity_scale,
        min_order_qty: lot_size,
        max_order_qty: Decimal::from(1_000_000),
        min_order_value: min_notional,
        max_leverage,
    })
}

// ---- ticker ----

/// Convert a `HotstuffTicker` to a core `Ticker`.
/// `symbol` is the global symbol (e.g. "BTC").
pub fn ticker_to_ticker(t: &HotstuffTicker, symbol: String) -> Result<Ticker> {
    let last_price = parse_decimal_opt(&t.last_price, "last_price");
    let mark_price = parse_decimal_opt(&t.mark_price, "mark_price");
    let index_price = parse_decimal_opt(&t.index_price, "index_price");

    let best_bid_price = parse_decimal_opt(&t.best_bid_price, "best_bid_price");
    let best_bid_qty = parse_decimal_opt(&t.best_bid_size, "best_bid_size");
    let best_ask_price = parse_decimal_opt(&t.best_ask_price, "best_ask_price");
    let best_ask_qty = parse_decimal_opt(&t.best_ask_size, "best_ask_size");

    // API's volume_24h is actually turnover_24h (quote notional); derive base volume by dividing by last_price
    let turnover_24h = parse_decimal_opt(&t.volume_24h, "volume_24h");
    let volume_24h = if last_price > Decimal::ZERO {
        turnover_24h / last_price
    } else {
        Decimal::ZERO
    };
    let open_interest = parse_decimal_opt(&t.open_interest, "open_interest");

    // change_24h is an absolute price change
    let change_24h = parse_decimal_opt(&t.change_24h, "change_24h");
    let price_change_24h = change_24h;
    let price_change_pct = if last_price > Decimal::ZERO && price_change_24h != Decimal::ZERO {
        let prev = last_price - price_change_24h;
        if prev > Decimal::ZERO {
            price_change_24h / prev
        } else {
            Decimal::ZERO
        }
    } else {
        Decimal::ZERO
    };

    let high_price_24h = parse_decimal_opt(&t.max_trading_price, "max_trading_price");
    let low_price_24h = parse_decimal_opt(&t.min_trading_price, "min_trading_price");

    // open_interest_notional = OI × mark_price
    let open_interest_notional = if open_interest > Decimal::ZERO && mark_price > Decimal::ZERO {
        open_interest * mark_price
    } else {
        Decimal::ZERO
    };

    let timestamp = t
        .last_updated
        .map(unix_ms_to_datetime)
        .unwrap_or_else(Utc::now);

    Ok(Ticker {
        symbol,
        last_price,
        mark_price,
        index_price,
        best_bid_price,
        best_bid_qty,
        best_ask_price,
        best_ask_qty,
        volume_24h,
        turnover_24h,
        open_interest,
        open_interest_notional,
        price_change_24h,
        price_change_pct,
        high_price_24h,
        low_price_24h,
        timestamp,
    })
}

// ---- orderbook ----

fn level_to_orderbook_level(level: &HotstuffLevel) -> Result<OrderbookLevel> {
    Ok(OrderbookLevel {
        price: f64_to_decimal(level.price, "price")?,
        quantity: f64_to_decimal(level.size, "size")?,
    })
}

/// Convert a `HotstuffOrderbook` to a core `Orderbook`.
/// `symbol` is the global symbol (e.g. "BTC").
pub fn orderbook_to_orderbook(ob: HotstuffOrderbook, symbol: String) -> Result<Orderbook> {
    let mut bids = ob
        .bids
        .iter()
        .map(level_to_orderbook_level)
        .collect::<Result<Vec<_>>>()?;

    let mut asks = ob
        .asks
        .iter()
        .map(level_to_orderbook_level)
        .collect::<Result<Vec<_>>>()?;

    // Sort: bids descending, asks ascending
    bids.sort_by(|a, b| b.price.cmp(&a.price));
    asks.sort_by(|a, b| a.price.cmp(&b.price));

    let timestamp = ob
        .timestamp
        .map(unix_ms_to_datetime)
        .unwrap_or_else(Utc::now);

    Ok(Orderbook {
        symbol,
        bids,
        asks,
        timestamp,
    })
}

// ---- funding rate ----

/// Extract a `FundingRate` from a `HotstuffTicker`.
/// `symbol` is the global symbol (e.g. "BTC").
pub fn ticker_to_funding_rate(t: &HotstuffTicker, symbol: String) -> Result<FundingRate> {
    let funding_rate = parse_decimal_opt(&t.funding_rate, "funding_rate");

    let timestamp = t
        .last_updated
        .map(unix_ms_to_datetime)
        .unwrap_or_else(Utc::now);

    // Hotstuff doesn't provide next_funding_time; default to now + 1 hour
    let next_funding_time = timestamp + chrono::Duration::hours(1);

    Ok(FundingRate {
        symbol,
        funding_rate,
        predicted_rate: funding_rate,
        funding_time: timestamp,
        next_funding_time,
        funding_interval: 1,
        funding_rate_cap_floor: Decimal::from_str_exact("0.75").unwrap_or(Decimal::ZERO),
    })
}

// ---- open interest ----

/// Extract an `OpenInterest` from a `HotstuffTicker`.
/// `symbol` is the global symbol (e.g. "BTC").
pub fn ticker_to_open_interest(t: &HotstuffTicker, symbol: String) -> Result<OpenInterest> {
    let open_interest = parse_decimal_opt(&t.open_interest, "open_interest");
    let mark_price = parse_decimal_opt(&t.mark_price, "mark_price");
    let open_value = open_interest * mark_price;

    let timestamp = t
        .last_updated
        .map(unix_ms_to_datetime)
        .unwrap_or_else(Utc::now);

    Ok(OpenInterest {
        symbol,
        open_interest,
        open_value,
        timestamp,
    })
}

// ---- klines ----

/// Convert a `HotstuffKline` to a core `Kline`.
/// `symbol` is the global symbol, `interval` is the human-readable interval string (e.g. "1h").
pub fn kline_to_kline(k: &HotstuffKline, symbol: String, interval: String) -> Result<Kline> {
    let open = k.open.map(|v| f64_to_decimal(v, "open")).transpose()?.unwrap_or(Decimal::ZERO);
    let high = k.high.map(|v| f64_to_decimal(v, "high")).transpose()?.unwrap_or(Decimal::ZERO);
    let low = k.low.map(|v| f64_to_decimal(v, "low")).transpose()?.unwrap_or(Decimal::ZERO);
    let close = k.close.map(|v| f64_to_decimal(v, "close")).transpose()?.unwrap_or(Decimal::ZERO);
    let volume = k.volume.map(|v| f64_to_decimal(v, "volume")).transpose()?.unwrap_or(Decimal::ZERO);

    let open_time = k.time.map(unix_ms_to_datetime).unwrap_or_else(Utc::now);
    // close_time is not provided; approximate by open_time (or leave as open_time)
    let close_time = open_time;

    Ok(Kline {
        symbol,
        interval,
        open_time,
        close_time,
        open,
        high,
        low,
        close,
        volume,
        turnover: close * volume, // approximate: close × volume
    })
}

// ---- trades ----

/// Convert a `HotstuffTrade` to a core `Trade`.
/// `symbol` is the global symbol (e.g. "BTC").
pub fn trade_to_trade(t: HotstuffTrade, symbol: String) -> Result<Trade> {
    let price = t
        .price
        .as_deref()
        .map(|s| parse_decimal(s, "price"))
        .transpose()?
        .unwrap_or(Decimal::ZERO);

    let quantity = t
        .size
        .as_deref()
        .map(|s| parse_decimal(s, "size"))
        .transpose()?
        .unwrap_or(Decimal::ZERO);

    // side: "b" = buy, "s" = sell
    let side = match t.side.as_deref() {
        Some("b") | Some("buy") => OrderSide::Buy,
        Some("s") | Some("sell") => OrderSide::Sell,
        _ => OrderSide::Buy, // default
    };

    // timestamp is ISO 8601 string e.g. "2026-02-03T17:00:57.558Z"
    let timestamp = t
        .timestamp
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(Utc::now);

    let id = t
        .trade_id
        .map(|id| id.to_string())
        .unwrap_or_else(|| "0".to_string());

    Ok(Trade {
        id,
        symbol,
        price,
        quantity,
        side,
        timestamp,
    })
}

// ---- market stats ----

/// Convert a `HotstuffTicker` to a core `MarketStats`.
/// `symbol` is the global symbol (e.g. "BTC").
pub fn ticker_to_market_stats(t: &HotstuffTicker, symbol: String) -> Result<MarketStats> {
    let last_price = parse_decimal_opt(&t.last_price, "last_price");
    let mark_price = parse_decimal_opt(&t.mark_price, "mark_price");
    let index_price = parse_decimal_opt(&t.index_price, "index_price");
    // API's volume_24h is actually turnover_24h; derive base volume by dividing by last_price
    let turnover_24h = parse_decimal_opt(&t.volume_24h, "volume_24h");
    let volume_24h = if last_price > Decimal::ZERO {
        turnover_24h / last_price
    } else {
        Decimal::ZERO
    };
    let open_interest = parse_decimal_opt(&t.open_interest, "open_interest");
    let funding_rate = parse_decimal_opt(&t.funding_rate, "funding_rate");

    let change_24h = parse_decimal_opt(&t.change_24h, "change_24h");
    let price_change_24h = change_24h;
    let price_change_pct = if last_price > Decimal::ZERO && price_change_24h != Decimal::ZERO {
        let prev = last_price - price_change_24h;
        if prev > Decimal::ZERO {
            price_change_24h / prev
        } else {
            Decimal::ZERO
        }
    } else {
        Decimal::ZERO
    };

    let high_price_24h = parse_decimal_opt(&t.max_trading_price, "max_trading_price");
    let low_price_24h = parse_decimal_opt(&t.min_trading_price, "min_trading_price");

    let timestamp = t
        .last_updated
        .map(unix_ms_to_datetime)
        .unwrap_or_else(Utc::now);

    Ok(MarketStats {
        symbol,
        last_price,
        mark_price,
        index_price,
        volume_24h,
        turnover_24h,
        price_change_24h,
        price_change_pct,
        high_price_24h,
        low_price_24h,
        open_interest,
        funding_rate,
        timestamp,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_ticker(symbol: &str) -> HotstuffTicker {
        HotstuffTicker {
            symbol: symbol.to_string(),
            mark_price: Some("50000.0".to_string()),
            mid_price: Some("50001.0".to_string()),
            index_price: Some("49999.0".to_string()),
            best_bid_price: Some("49998.0".to_string()),
            best_ask_price: Some("50004.0".to_string()),
            best_bid_size: Some("1.5".to_string()),
            best_ask_size: Some("0.8".to_string()),
            funding_rate: Some("0.001".to_string()),
            open_interest: Some("1200.5".to_string()),
            volume_24h: Some("1000000.0".to_string()),
            change_24h: Some("500.0".to_string()),
            max_trading_price: Some("51000.0".to_string()),
            min_trading_price: Some("48000.0".to_string()),
            last_price: Some("49998.0".to_string()),
            last_updated: Some(1_700_000_000_000),
        }
    }

    #[test]
    fn test_ticker_conversion() {
        let ht = make_ticker("BTC-PERP");
        let ticker = ticker_to_ticker(&ht, "BTC".to_string()).unwrap();

        assert_eq!(ticker.symbol, "BTC");
        assert_eq!(ticker.last_price, Decimal::from_str_exact("49998.0").unwrap());
        assert_eq!(ticker.best_bid_price, Decimal::from_str_exact("49998.0").unwrap());
        assert_eq!(ticker.best_ask_price, Decimal::from_str_exact("50004.0").unwrap());
        assert_eq!(ticker.high_price_24h, Decimal::from_str_exact("51000.0").unwrap());
    }

    #[test]
    fn test_funding_rate_conversion() {
        let ht = make_ticker("BTC-PERP");
        let fr = ticker_to_funding_rate(&ht, "BTC".to_string()).unwrap();

        assert_eq!(fr.symbol, "BTC");
        assert_eq!(fr.funding_rate, Decimal::from_str_exact("0.001").unwrap());
    }

    #[test]
    fn test_trade_conversion_buy() {
        let ht = HotstuffTrade {
            instrument: Some("BTC-PERP".to_string()),
            trade_id: Some(12345),
            side: Some("b".to_string()),
            price: Some("50000.0".to_string()),
            size: Some("0.5".to_string()),
            timestamp: Some("2026-02-03T17:00:57.558Z".to_string()),
        };
        let trade = trade_to_trade(ht, "BTC".to_string()).unwrap();

        assert_eq!(trade.symbol, "BTC");
        assert_eq!(trade.side, OrderSide::Buy);
        assert_eq!(trade.id, "12345");
    }

    #[test]
    fn test_trade_conversion_sell() {
        let ht = HotstuffTrade {
            instrument: Some("BTC-PERP".to_string()),
            trade_id: Some(u64::MAX),
            side: Some("s".to_string()),
            price: Some("50000.0".to_string()),
            size: Some("0.5".to_string()),
            timestamp: Some("2026-02-03T17:00:57.558Z".to_string()),
        };
        let trade = trade_to_trade(ht, "BTC".to_string()).unwrap();

        assert_eq!(trade.side, OrderSide::Sell);
        // Verify u64::MAX doesn't panic
        assert_eq!(trade.id, u64::MAX.to_string());
    }
}
