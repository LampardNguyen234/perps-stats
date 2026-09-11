use super::types::{ContractMeta, DepthRecord, FundingRateRecord, KlineRecord, TickerRecord};
use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, TimeZone, Utc};
use perps_core::{
    FundingRate, Kline, Market, MarketStats, OpenInterest, Orderbook, OrderbookLevel, Ticker,
};
use rust_decimal::Decimal;

/// `(project interval string, EdgeX `klineType`, interval duration in milliseconds)`.
/// Single source of truth for both the outbound API call (`klineType`) and the inbound
/// `close_time` derivation (EdgeX exposes no close timestamp of its own).
pub const KLINE_INTERVALS: &[(&str, &str, i64)] = &[
    ("1m", "MINUTE_1", 60_000),
    ("5m", "MINUTE_5", 300_000),
    ("15m", "MINUTE_15", 900_000),
    ("30m", "MINUTE_30", 1_800_000),
    ("1h", "HOUR_1", 3_600_000),
    ("4h", "HOUR_4", 14_400_000),
    ("1d", "DAY_1", 86_400_000),
    ("1w", "WEEK_1", 604_800_000),
];

// `pub(crate)`: reused as-is by `ws_client.rs` (per `01_overview.md` Key decision 4 - the
// WS conversion functions reuse these scalar-parsing helpers rather than duplicating them).
pub(crate) fn parse_decimal(s: &str, field: &str) -> Result<Decimal> {
    Decimal::from_str_exact(s).with_context(|| format!("failed to parse {field}: {s:?}"))
}

pub(crate) fn parse_ms(s: &str, field: &str) -> Result<i64> {
    s.parse::<i64>()
        .with_context(|| format!("failed to parse {field} as milliseconds: {s:?}"))
}

pub(crate) fn datetime_from_ms(ms: i64) -> Result<DateTime<Utc>> {
    Utc.timestamp_millis_opt(ms)
        .single()
        .ok_or_else(|| anyhow!("invalid millisecond timestamp: {ms}"))
}

pub(crate) fn interval_duration_ms(interval: &str) -> Result<i64> {
    KLINE_INTERVALS
        .iter()
        .find(|(k, _, _)| *k == interval)
        .map(|(_, _, d)| *d)
        .ok_or_else(|| anyhow!("EdgeX: unsupported kline interval: {interval}"))
}

/// Counts digits after the decimal point in a plain decimal string
/// (e.g. `"0.001"` -> 3, `"1"` -> 0). EdgeX's `tickSize`/`stepSize` are plain decimals with
/// no price-dependent tiers, so this is exact (unlike Arcus's tiered tick sizes).
fn decimal_places(s: &str) -> i32 {
    s.split_once('.')
        .map(|(_, frac)| frac.len() as i32)
        .unwrap_or(0)
}

/// Converts EdgeX contract metadata into the domain `Market` type.
///
/// `symbol` is left empty here — callers must overwrite it with the normalized global
/// symbol (`EdgexClient::normalize_symbol`), since that requires alias resolution this
/// pure function does not perform.
pub fn contract_to_market(meta: &ContractMeta) -> Result<Market> {
    Ok(Market {
        symbol: String::new(),
        contract: meta.contract_name.clone(),
        contract_size: Decimal::ONE,
        price_scale: decimal_places(&meta.tick_size),
        quantity_scale: decimal_places(&meta.step_size),
        min_order_qty: parse_decimal(&meta.min_order_size, "minOrderSize")?,
        max_order_qty: parse_decimal(&meta.max_order_size, "maxOrderSize")?,
        min_order_value: Decimal::ZERO,
        max_leverage: parse_decimal(&meta.display_max_leverage, "displayMaxLeverage")?,
    })
}

/// Merges a `getTicker` record (no bid/ask) with a `getDepth` record's best level for
/// top-of-book completeness. `symbol` is left empty for the caller to fill in.
pub fn ticker_record_to_ticker(ticker: &TickerRecord, depth: &DepthRecord) -> Result<Ticker> {
    let best_bid = depth.bids.first();
    let best_ask = depth.asks.first();
    let mark_price = parse_decimal(&ticker.mark_price, "markPrice")?;
    let open_interest = parse_decimal(&ticker.open_interest, "openInterest")?;

    Ok(Ticker {
        symbol: String::new(),
        last_price: parse_decimal(&ticker.last_price, "lastPrice")?,
        mark_price,
        index_price: parse_decimal(&ticker.index_price, "indexPrice")?,
        best_bid_price: best_bid
            .map(|l| parse_decimal(&l.price, "bids[0].price"))
            .transpose()?
            .unwrap_or(Decimal::ZERO),
        best_bid_qty: best_bid
            .map(|l| parse_decimal(&l.size, "bids[0].size"))
            .transpose()?
            .unwrap_or(Decimal::ZERO),
        best_ask_price: best_ask
            .map(|l| parse_decimal(&l.price, "asks[0].price"))
            .transpose()?
            .unwrap_or(Decimal::ZERO),
        best_ask_qty: best_ask
            .map(|l| parse_decimal(&l.size, "asks[0].size"))
            .transpose()?
            .unwrap_or(Decimal::ZERO),
        volume_24h: parse_decimal(&ticker.size, "size")?,
        turnover_24h: parse_decimal(&ticker.value, "value")?,
        open_interest,
        open_interest_notional: (open_interest * mark_price).round_dp(2),
        price_change_24h: parse_decimal(&ticker.price_change, "priceChange")?,
        price_change_pct: parse_decimal(&ticker.price_change_percent, "priceChangePercent")?,
        high_price_24h: parse_decimal(&ticker.high, "high")?,
        low_price_24h: parse_decimal(&ticker.low, "low")?,
        timestamp: datetime_from_ms(parse_ms(&ticker.end_time, "endTime")?)?,
    })
}

/// Converts a `getDepth` record into the domain `Orderbook`. `getDepth` carries no event
/// timestamp, so callers pass in the response envelope's `responseTime` (ms) instead.
/// Bids/asks are defensively re-sorted and a crossed book is rejected, per project
/// convention (`docs/new_exchange_requirements.md` §3.2), even though live EdgeX data is
/// already correctly ordered. `truncate_to` is `None` for "return the complete response".
pub fn depth_record_to_orderbook(
    record: &DepthRecord,
    response_time_ms: i64,
    truncate_to: Option<usize>,
) -> Result<Orderbook> {
    let mut bids: Vec<OrderbookLevel> = record
        .bids
        .iter()
        .map(|l| {
            Ok(OrderbookLevel {
                price: parse_decimal(&l.price, "bids[].price")?,
                quantity: parse_decimal(&l.size, "bids[].size")?,
            })
        })
        .collect::<Result<_>>()?;
    let mut asks: Vec<OrderbookLevel> = record
        .asks
        .iter()
        .map(|l| {
            Ok(OrderbookLevel {
                price: parse_decimal(&l.price, "asks[].price")?,
                quantity: parse_decimal(&l.size, "asks[].size")?,
            })
        })
        .collect::<Result<_>>()?;

    bids.sort_by(|a, b| b.price.cmp(&a.price));
    asks.sort_by(|a, b| a.price.cmp(&b.price));

    if let (Some(best_bid), Some(best_ask)) = (bids.first(), asks.first()) {
        if best_bid.price >= best_ask.price {
            bail!(
                "EdgeX orderbook is crossed: best_bid={} best_ask={}",
                best_bid.price,
                best_ask.price
            );
        }
    }

    if let Some(n) = truncate_to {
        bids.truncate(n);
        asks.truncate(n);
    }

    Ok(Orderbook {
        symbol: String::new(),
        bids,
        asks,
        timestamp: datetime_from_ms(response_time_ms)?,
    })
}

/// Converts one `getKline` record into the domain `Kline`. EdgeX exposes no close
/// timestamp — `close_time` is derived as `open_time + interval - 1ms`.
pub fn kline_record_to_kline(record: &KlineRecord, interval: &str) -> Result<Kline> {
    let duration_ms = interval_duration_ms(interval)?;
    let open_ms = parse_ms(&record.kline_time, "klineTime")?;
    let open_time = datetime_from_ms(open_ms)?;
    let close_time = datetime_from_ms(open_ms + duration_ms - 1)?;

    Ok(Kline {
        symbol: String::new(),
        interval: interval.to_string(),
        open_time,
        close_time,
        open: parse_decimal(&record.open, "open")?,
        high: parse_decimal(&record.high, "high")?,
        low: parse_decimal(&record.low, "low")?,
        close: parse_decimal(&record.close, "close")?,
        volume: parse_decimal(&record.size, "size")?,
        turnover: parse_decimal(&record.value, "value")?,
    })
}

/// Converts a `getLatestFundingRate`/`getFundingRatePage` row into the domain
/// `FundingRate`. `interval_min_str` is the record's own `fundingRateIntervalMin` when
/// present, else the contract's — passed in already resolved by the caller.
///
/// `forecastFundingRate` can be an empty string on settlement rows; falls back to the
/// settled `fundingRate` rather than erroring. `predictedFundingRate` (a different field,
/// documented as the interest component) is never used as a forecast substitute.
pub fn funding_record_to_funding_rate(
    record: &FundingRateRecord,
    interval_min_str: &str,
    meta: &ContractMeta,
) -> Result<FundingRate> {
    let funding_rate = parse_decimal(&record.funding_rate, "fundingRate")?;
    let predicted_rate = if record.forecast_funding_rate.is_empty() {
        funding_rate
    } else {
        parse_decimal(&record.forecast_funding_rate, "forecastFundingRate")?
    };

    let interval_min: i64 = interval_min_str
        .parse()
        .with_context(|| format!("failed to parse fundingRateIntervalMin: {interval_min_str:?}"))?;
    if interval_min <= 0 || interval_min % 60 != 0 {
        bail!("EdgeX: fundingRateIntervalMin {interval_min} is not a positive multiple of 60");
    }
    let funding_interval = (interval_min / 60) as i32;

    let funding_time_ms = parse_ms(&record.funding_time, "fundingTime")?;
    let funding_time = datetime_from_ms(funding_time_ms)?;
    let next_funding_time = datetime_from_ms(funding_time_ms + interval_min * 60_000)?;

    // Asymmetric bounds (fundingMinRate/fundingMaxRate) collapse to one magnitude because
    // the core type stores a single cap/floor value.
    let min_rate = parse_decimal(&meta.funding_min_rate, "fundingMinRate")?.abs();
    let max_rate = parse_decimal(&meta.funding_max_rate, "fundingMaxRate")?.abs();
    let funding_rate_cap_floor = min_rate.max(max_rate);

    Ok(FundingRate {
        symbol: String::new(),
        funding_rate,
        predicted_rate,
        funding_time,
        next_funding_time,
        funding_interval,
        funding_rate_cap_floor,
    })
}

/// Maps a `getTicker` record directly to `MarketStats` (no bid/ask fields needed, so no
/// `getDepth` call required here, unlike `ticker_record_to_ticker`).
pub fn ticker_record_to_market_stats(ticker: &TickerRecord) -> Result<MarketStats> {
    Ok(MarketStats {
        symbol: String::new(),
        volume_24h: parse_decimal(&ticker.size, "size")?,
        turnover_24h: parse_decimal(&ticker.value, "value")?,
        open_interest: parse_decimal(&ticker.open_interest, "openInterest")?,
        funding_rate: parse_decimal(&ticker.funding_rate, "fundingRate")?,
        last_price: parse_decimal(&ticker.last_price, "lastPrice")?,
        mark_price: parse_decimal(&ticker.mark_price, "markPrice")?,
        index_price: parse_decimal(&ticker.index_price, "indexPrice")?,
        price_change_24h: parse_decimal(&ticker.price_change, "priceChange")?,
        price_change_pct: parse_decimal(&ticker.price_change_percent, "priceChangePercent")?,
        high_price_24h: parse_decimal(&ticker.high, "high")?,
        low_price_24h: parse_decimal(&ticker.low, "low")?,
        timestamp: datetime_from_ms(parse_ms(&ticker.end_time, "endTime")?)?,
    })
}

/// Derives `OpenInterest.open_value` as `open_interest * mark_price` (rounded to 2 dp),
/// since EdgeX exposes no notional-open-interest field directly.
pub fn ticker_record_to_open_interest(ticker: &TickerRecord) -> Result<OpenInterest> {
    let open_interest = parse_decimal(&ticker.open_interest, "openInterest")?;
    let mark_price = parse_decimal(&ticker.mark_price, "markPrice")?;
    Ok(OpenInterest {
        symbol: String::new(),
        open_interest,
        open_value: (open_interest * mark_price).round_dp(2),
        timestamp: datetime_from_ms(parse_ms(&ticker.end_time, "endTime")?)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn btc_contract() -> ContractMeta {
        ContractMeta {
            contract_id: "30000001".to_string(),
            contract_name: "BTCUSDC".to_string(),
            base_coin_id: "1001".to_string(),
            quote_coin_id: "1000".to_string(),
            tick_size: "0.1".to_string(),
            step_size: "0.001".to_string(),
            min_order_size: "0.001".to_string(),
            max_order_size: "100".to_string(),
            default_taker_fee_rate: "0.00045".to_string(),
            default_maker_fee_rate: "0.0004".to_string(),
            default_leverage: "10".to_string(),
            funding_rate_interval_min: "240".to_string(),
            funding_min_rate: "-0.002".to_string(),
            funding_max_rate: "0.002".to_string(),
            display_max_leverage: "100".to_string(),
            enable_trade: true,
            enable_display: true,
            is_stock: false,
            is_fx: false,
        }
    }

    fn btc_ticker() -> TickerRecord {
        TickerRecord {
            contract_id: "30000001".to_string(),
            contract_name: "BTCUSDC".to_string(),
            price_change: "-981.1".to_string(),
            price_change_percent: "-0.012552".to_string(),
            size: "5505.888".to_string(),
            value: "425008583.0734".to_string(),
            high: "78172.2".to_string(),
            low: "76419.6".to_string(),
            open: "78160.2".to_string(),
            close: "77179.1".to_string(),
            end_time: "1789113600000".to_string(),
            last_price: "77179.1".to_string(),
            index_price: "77225.2658783509".to_string(),
            oracle_price: "77179.813424055301785339".to_string(),
            mark_price: "77179.813424055301785339".to_string(),
            open_interest: "3303.927".to_string(),
            funding_rate: "0.00005000".to_string(),
            funding_time: "1789099200000".to_string(),
            next_funding_time: "1789113600000".to_string(),
        }
    }

    fn btc_depth() -> DepthRecord {
        use super::super::types::BookOrder;
        DepthRecord {
            contract_id: "30000001".to_string(),
            level: 15,
            asks: vec![
                BookOrder {
                    price: "77179.3".to_string(),
                    size: "0.716".to_string(),
                },
                BookOrder {
                    price: "77179.4".to_string(),
                    size: "0.519".to_string(),
                },
            ],
            bids: vec![
                BookOrder {
                    price: "77179.0".to_string(),
                    size: "1.196".to_string(),
                },
                BookOrder {
                    price: "77178.9".to_string(),
                    size: "1.096".to_string(),
                },
            ],
        }
    }

    #[test]
    fn test_contract_to_market_btc() {
        let market = contract_to_market(&btc_contract()).unwrap();
        assert_eq!(market.contract, "BTCUSDC");
        assert_eq!(market.contract_size, Decimal::ONE);
        assert_eq!(market.price_scale, 1);
        assert_eq!(market.quantity_scale, 3);
        assert_eq!(
            market.min_order_qty,
            Decimal::from_str_exact("0.001").unwrap()
        );
        assert_eq!(
            market.max_order_qty,
            Decimal::from_str_exact("100").unwrap()
        );
        assert_eq!(market.min_order_value, Decimal::ZERO);
        assert_eq!(market.max_leverage, Decimal::from_str_exact("100").unwrap());
        assert_eq!(market.symbol, "");
    }

    #[test]
    fn test_decimal_places() {
        assert_eq!(decimal_places("0.1"), 1);
        assert_eq!(decimal_places("0.001"), 3);
        assert_eq!(decimal_places("100"), 0);
    }

    #[test]
    fn test_ticker_record_to_ticker_non_zero_and_ratios() {
        let ticker = ticker_record_to_ticker(&btc_ticker(), &btc_depth()).unwrap();
        assert!(ticker.last_price > Decimal::ZERO);
        assert!(ticker.volume_24h > Decimal::ZERO);
        assert!(ticker.turnover_24h > ticker.volume_24h);
        assert!(ticker.open_interest > Decimal::ZERO);
        // Decimal ratio, not a percentage (-1.2552) or bps (-12.552).
        assert_eq!(
            ticker.price_change_pct,
            Decimal::from_str_exact("-0.012552").unwrap()
        );
        // Top-of-book merged in from depth, not left at zero.
        assert_eq!(
            ticker.best_bid_price,
            Decimal::from_str_exact("77179.0").unwrap()
        );
        assert_eq!(
            ticker.best_ask_price,
            Decimal::from_str_exact("77179.3").unwrap()
        );
    }

    #[test]
    fn test_ticker_record_volume_turnover_not_swapped() {
        let ticker = ticker_record_to_ticker(&btc_ticker(), &btc_depth()).unwrap();
        assert_eq!(
            ticker.volume_24h,
            Decimal::from_str_exact("5505.888").unwrap()
        );
        assert_eq!(
            ticker.turnover_24h,
            Decimal::from_str_exact("425008583.0734").unwrap()
        );
    }

    #[test]
    fn test_open_interest_notional_derivation_rounded() {
        let ticker = ticker_record_to_ticker(&btc_ticker(), &btc_depth()).unwrap();
        let expected = (Decimal::from_str_exact("3303.927").unwrap()
            * Decimal::from_str_exact("77179.813424055301785339").unwrap())
        .round_dp(2);
        assert_eq!(ticker.open_interest_notional, expected);
        // No more than 2 decimal places.
        assert!(ticker.open_interest_notional.scale() <= 2);
    }

    #[test]
    fn test_depth_record_to_orderbook_sorted_no_cross() {
        let ob = depth_record_to_orderbook(&btc_depth(), 1789113507340, None).unwrap();
        assert!(
            ob.bids[0].price > ob.bids[1].price,
            "bids must be descending"
        );
        assert!(
            ob.asks[0].price < ob.asks[1].price,
            "asks must be ascending"
        );
        assert!(
            ob.asks[0].price > ob.bids[0].price,
            "book must not be crossed"
        );
    }

    #[test]
    fn test_depth_record_to_orderbook_truncates() {
        let ob = depth_record_to_orderbook(&btc_depth(), 1789113507340, Some(1)).unwrap();
        assert_eq!(ob.bids.len(), 1);
        assert_eq!(ob.asks.len(), 1);
    }

    #[test]
    fn test_kline_record_to_kline_close_time_derivation() {
        let record = KlineRecord {
            kline_time: "1789106400000".to_string(),
            size: "159.941".to_string(),
            value: "12351388.8421".to_string(),
            high: "77379.1".to_string(),
            low: "77115.5".to_string(),
            open: "77200.5".to_string(),
            close: "77242.7".to_string(),
        };
        let kline = kline_record_to_kline(&record, "1h").unwrap();
        assert_eq!(kline.volume, Decimal::from_str_exact("159.941").unwrap());
        assert_eq!(
            kline.turnover,
            Decimal::from_str_exact("12351388.8421").unwrap()
        );
        assert_eq!(
            (kline.close_time - kline.open_time).num_milliseconds(),
            3_600_000 - 1
        );
    }

    #[test]
    fn test_kline_record_to_kline_rejects_unsupported_interval() {
        let record = KlineRecord {
            kline_time: "1789106400000".to_string(),
            size: "1".to_string(),
            value: "1".to_string(),
            high: "1".to_string(),
            low: "1".to_string(),
            open: "1".to_string(),
            close: "1".to_string(),
        };
        assert!(kline_record_to_kline(&record, "3m").is_err());
    }

    #[test]
    fn test_funding_record_to_funding_rate_decimal_ratio_and_next_time() {
        let record = FundingRateRecord {
            contract_id: "30000001".to_string(),
            funding_time: "1789099200000".to_string(),
            funding_rate: "0.00005000".to_string(),
            forecast_funding_rate: "0.00005000".to_string(),
            funding_rate_interval_min: Some("240".to_string()),
            is_settlement: false,
        };
        let fr = funding_record_to_funding_rate(&record, "240", &btc_contract()).unwrap();
        assert_eq!(
            fr.funding_rate,
            Decimal::from_str_exact("0.00005000").unwrap()
        );
        assert_eq!(fr.funding_interval, 4);
        assert_eq!((fr.next_funding_time - fr.funding_time).num_minutes(), 240);
        assert_eq!(
            fr.funding_rate_cap_floor,
            Decimal::from_str_exact("0.002").unwrap()
        );
    }

    #[test]
    fn test_funding_record_empty_forecast_falls_back_to_settled_rate() {
        let record = FundingRateRecord {
            contract_id: "30000001".to_string(),
            funding_time: "1789113600000".to_string(),
            funding_rate: "0.00005000".to_string(),
            forecast_funding_rate: String::new(),
            funding_rate_interval_min: Some("240".to_string()),
            is_settlement: true,
        };
        let fr = funding_record_to_funding_rate(&record, "240", &btc_contract()).unwrap();
        // Must fall back to the settled `fundingRate`, not silently zero or error.
        assert_eq!(fr.predicted_rate, fr.funding_rate);
    }
}
