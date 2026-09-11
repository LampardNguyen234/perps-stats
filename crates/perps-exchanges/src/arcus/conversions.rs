use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, TimeZone, Utc};
use perps_core::{
    FundingRate, Kline, Market, MarketStats, OpenInterest, OrderSide, Orderbook, OrderbookLevel,
    Ticker, Trade,
};
use rust_decimal::Decimal;

use super::types::*;

/// Kline timeframes Arcus's `/v1/candles` endpoint accepts.
pub const SUPPORTED_TIMEFRAMES: &[&str] = &[
    "1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h", "8h", "12h", "1d", "3d", "1w",
];

// ---- helpers ----

/// Parse a required Arcus decimal-string field. Never falls back to `f64` (per project
/// convention: Arcus's decimal strings must round-trip exactly) and never defaults to zero —
/// a missing/malformed required value is a hard error, not a silent zero.
fn parse_decimal(s: &str, field: &str) -> Result<Decimal> {
    Decimal::from_str_exact(s).with_context(|| format!("failed to parse {field} as decimal: {s:?}"))
}

/// Parse a decimal field that Arcus omits entirely on `OFFLINE` markets (e.g.
/// `lastTradePrice`, `high24h`, `low24h`). A missing value is a hard error here too — callers
/// only reach this after already filtering for `ONLINE` markets, where these fields are
/// always present.
fn parse_required_decimal(s: &Option<String>, field: &str) -> Result<Decimal> {
    let s = s
        .as_deref()
        .ok_or_else(|| anyhow!("{field} is missing (market is likely OFFLINE)"))?;
    parse_decimal(s, field)
}

fn unix_seconds_to_datetime(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(secs, 0).single().unwrap_or_else(Utc::now)
}

fn unix_micros_to_datetime(micros: i64) -> DateTime<Utc> {
    let secs = micros.div_euclid(1_000_000);
    let nanos = (micros.rem_euclid(1_000_000) * 1000) as u32;
    Utc.timestamp_opt(secs, nanos)
        .single()
        .unwrap_or_else(Utc::now)
}

/// Approximate duration for a supported timeframe string. Returns zero for anything outside
/// `SUPPORTED_TIMEFRAMES` (callers validate the interval before this is reached).
fn interval_duration(interval: &str) -> chrono::Duration {
    match interval {
        "1m" => chrono::Duration::minutes(1),
        "3m" => chrono::Duration::minutes(3),
        "5m" => chrono::Duration::minutes(5),
        "15m" => chrono::Duration::minutes(15),
        "30m" => chrono::Duration::minutes(30),
        "1h" => chrono::Duration::hours(1),
        "2h" => chrono::Duration::hours(2),
        "4h" => chrono::Duration::hours(4),
        "8h" => chrono::Duration::hours(8),
        "12h" => chrono::Duration::hours(12),
        "1d" => chrono::Duration::days(1),
        "3d" => chrono::Duration::days(3),
        "1w" => chrono::Duration::weeks(1),
        _ => chrono::Duration::zero(),
    }
}

/// Derive the absolute 24h price change from `last_price` and the decimal-ratio
/// `price_change_pct`, since Arcus exposes no absolute-change field directly.
/// Guards the `1 + price_change_pct == 0` case (a -100% move), which would otherwise divide
/// by zero.
fn derive_price_change_24h(last_price: Decimal, price_change_pct: Decimal) -> Decimal {
    let denom = Decimal::ONE + price_change_pct;
    if denom.is_zero() {
        Decimal::ZERO
    } else {
        last_price - (last_price / denom)
    }
}

// ---- ArcusMarket -> Market ----

/// Convert an `ArcusMarket` to a core `Market`.
///
/// `symbol` is set to the raw `base_asset` (e.g. `"BTC"`); callers must overwrite it with
/// `ArcusClient::normalize_symbol` to apply alias resolution before returning it.
pub fn to_market(m: &ArcusMarket) -> Result<Market> {
    let tick_size = parse_decimal(&m.tick_size, "tickSize")?;
    let step_size = parse_decimal(&m.step_size, "stepSize")?;
    let min_order_qty = parse_decimal(&m.min_order_size, "minOrderSize")?;
    let max_order_qty = parse_decimal(&m.max_order_size, "maxOrderSize")?;
    let min_order_value = parse_decimal(&m.min_order_notional, "minOrderNotional")?;
    let initial_margin_fraction =
        parse_decimal(&m.initial_margin_fraction, "initialMarginFraction")?;

    if initial_margin_fraction.is_zero() {
        return Err(anyhow!(
            "initialMarginFraction is zero for market {}",
            m.market_display_name
        ));
    }
    let max_leverage = (Decimal::ONE / initial_margin_fraction).round_dp(2);

    Ok(Market {
        symbol: m.base_asset.clone(),
        contract: m.market_display_name.clone(),
        contract_size: Decimal::ONE,
        price_scale: tick_size.scale() as i32,
        quantity_scale: step_size.scale() as i32,
        min_order_qty,
        max_order_qty,
        min_order_value,
        max_leverage,
    })
}

// ---- ArcusMarket + OrderbookSnapshot -> Ticker ----

/// Build a core `Ticker` from a markets entry plus a top-of-book orderbook snapshot.
///
/// `/v1/markets` alone never populates bid/ask (see `00_requirements.md`), so every caller
/// must fetch at least `nLevels=1` and pass it here for the ticker to satisfy
/// `Ticker::is_empty() == false`.
pub fn to_ticker(
    m: &ArcusMarket,
    ob: &OrderbookSnapshot,
    captured_at: DateTime<Utc>,
) -> Result<Ticker> {
    let last_price = parse_required_decimal(&m.last_trade_price, "lastTradePrice")?;
    let mark_price = parse_decimal(&m.mark_price, "markPrice")?;
    let index_price = parse_decimal(&m.oracle_price, "oraclePrice")?;
    let volume_24h = parse_decimal(&m.volume_24h, "volume24h")?;
    let turnover_24h = parse_decimal(&m.volume_24h_notional, "volume24hNotional")?;
    let open_interest = parse_decimal(&m.open_interest, "openInterest")?;
    let open_interest_notional = (open_interest * mark_price).round_dp(2);
    let price_change_pct = parse_decimal(&m.price_change_24h, "priceChange24h")?;
    let price_change_24h = derive_price_change_24h(last_price, price_change_pct);
    let high_price_24h = parse_required_decimal(&m.high_24h, "high24h")?;
    let low_price_24h = parse_required_decimal(&m.low_24h, "low24h")?;

    let (best_bid_price, best_bid_qty) = match ob.bids.first() {
        Some([p, q]) => (
            parse_decimal(p, "bid.price")?,
            parse_decimal(q, "bid.quantity")?,
        ),
        None => (Decimal::ZERO, Decimal::ZERO),
    };
    let (best_ask_price, best_ask_qty) = match ob.asks.first() {
        Some([p, q]) => (
            parse_decimal(p, "ask.price")?,
            parse_decimal(q, "ask.quantity")?,
        ),
        None => (Decimal::ZERO, Decimal::ZERO),
    };

    Ok(Ticker {
        symbol: m.base_asset.clone(),
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
        timestamp: captured_at,
    })
}

// ---- ArcusMarket -> FundingRate (current) ----

/// Build the current `FundingRate` from a markets entry.
///
/// `next_funding_at` is Unix **seconds**. Arcus exposes only the next settlement time, not the
/// current interval's start, so `funding_time` is derived as `next_funding_time - 1 hour`
/// (Arcus funding is hourly, unlike the more common 8h cadence).
pub fn to_funding_rate(m: &ArcusMarket) -> Result<FundingRate> {
    let funding_rate = parse_decimal(&m.funding_rate, "fundingRate")?;
    let predicted_rate = parse_decimal(&m.next_funding_rate, "nextFundingRate")?;
    let next_funding_time = unix_seconds_to_datetime(m.next_funding_at);
    let funding_time = next_funding_time - chrono::Duration::hours(1);

    Ok(FundingRate {
        symbol: m.base_asset.clone(),
        funding_rate,
        predicted_rate,
        funding_time,
        next_funding_time,
        funding_interval: 1,
        // Arcus does not expose a funding rate cap/floor.
        funding_rate_cap_floor: Decimal::ZERO,
    })
}

// ---- ArcusFundingRate -> FundingRate (history) ----

/// Convert one `/v1/fundingRates` history entry. `time` is microseconds, unlike the seconds
/// timestamp used by `to_funding_rate` for the current-rate case.
pub fn funding_rate_entry_to_funding_rate(
    e: &ArcusFundingRate,
    symbol: String,
) -> Result<FundingRate> {
    let funding_rate = parse_decimal(&e.funding_rate, "fundingRate")?;
    let funding_time = unix_micros_to_datetime(e.time);

    Ok(FundingRate {
        symbol,
        funding_rate,
        predicted_rate: funding_rate,
        funding_time,
        next_funding_time: funding_time + chrono::Duration::hours(1),
        funding_interval: 1,
        funding_rate_cap_floor: Decimal::ZERO,
    })
}

// ---- ArcusMarket -> OpenInterest ----

/// `open_interest_cap_notional` is a configured cap, not the current notional — always derive
/// `open_value` as `open_interest * mark_price`.
pub fn to_open_interest(m: &ArcusMarket, captured_at: DateTime<Utc>) -> Result<OpenInterest> {
    let open_interest = parse_decimal(&m.open_interest, "openInterest")?;
    let mark_price = parse_decimal(&m.mark_price, "markPrice")?;

    Ok(OpenInterest {
        symbol: m.base_asset.clone(),
        open_interest,
        open_value: (open_interest * mark_price).round_dp(2),
        timestamp: captured_at,
    })
}

// ---- ArcusMarket -> MarketStats ----

pub fn to_market_stats(m: &ArcusMarket, captured_at: DateTime<Utc>) -> Result<MarketStats> {
    let last_price = parse_required_decimal(&m.last_trade_price, "lastTradePrice")?;
    let mark_price = parse_decimal(&m.mark_price, "markPrice")?;
    let index_price = parse_decimal(&m.oracle_price, "oraclePrice")?;
    let volume_24h = parse_decimal(&m.volume_24h, "volume24h")?;
    let turnover_24h = parse_decimal(&m.volume_24h_notional, "volume24hNotional")?;
    let open_interest = parse_decimal(&m.open_interest, "openInterest")?;
    let funding_rate = parse_decimal(&m.funding_rate, "fundingRate")?;
    let price_change_pct = parse_decimal(&m.price_change_24h, "priceChange24h")?;
    let price_change_24h = derive_price_change_24h(last_price, price_change_pct);
    let high_price_24h = parse_required_decimal(&m.high_24h, "high24h")?;
    let low_price_24h = parse_required_decimal(&m.low_24h, "low24h")?;

    Ok(MarketStats {
        symbol: m.base_asset.clone(),
        volume_24h,
        turnover_24h,
        open_interest,
        funding_rate,
        last_price,
        mark_price,
        index_price,
        price_change_24h,
        price_change_pct,
        high_price_24h,
        low_price_24h,
        timestamp: captured_at,
    })
}

// ---- OrderbookSnapshot -> Orderbook ----

/// Convert a raw `OrderbookSnapshot` to a core `Orderbook`. Live data is already sorted
/// correctly (bids desc, asks asc) but this defensively re-sorts per project convention.
pub fn orderbook_snapshot_to_orderbook(
    ob: &OrderbookSnapshot,
    symbol: String,
) -> Result<Orderbook> {
    let mut bids = ob
        .bids
        .iter()
        .map(|[p, q]| {
            Ok(OrderbookLevel {
                price: parse_decimal(p, "bid.price")?,
                quantity: parse_decimal(q, "bid.quantity")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let mut asks = ob
        .asks
        .iter()
        .map(|[p, q]| {
            Ok(OrderbookLevel {
                price: parse_decimal(p, "ask.price")?,
                quantity: parse_decimal(q, "ask.quantity")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    bids.sort_by(|a, b| b.price.cmp(&a.price));
    asks.sort_by(|a, b| a.price.cmp(&b.price));

    Ok(Orderbook {
        symbol,
        bids,
        asks,
        timestamp: unix_micros_to_datetime(ob.timestamp),
    })
}

// ---- ArcusCandle -> Kline ----

/// `volume` (base) and `notional_volume` (USD turnover) map directly with no derivation.
/// There is no `closeTime` field; it is approximated as `open_time + interval_duration - 1ms`.
pub fn candle_to_kline(c: &ArcusCandle, symbol: String, interval: String) -> Result<Kline> {
    let open = parse_decimal(&c.open, "open")?;
    let high = parse_decimal(&c.high, "high")?;
    let low = parse_decimal(&c.low, "low")?;
    let close = parse_decimal(&c.close, "close")?;
    let volume = parse_decimal(&c.volume, "volume")?;
    let turnover = parse_decimal(&c.notional_volume, "notionalVolume")?;

    let open_time = unix_micros_to_datetime(c.open_time);
    let close_time = open_time + interval_duration(&interval) - chrono::Duration::milliseconds(1);

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
        turnover,
    })
}

// ---- ArcusTrade -> Trade ----

/// An unknown `side` value is a conversion error (never silently defaulted to `Buy`); callers
/// warn and skip such entries rather than failing the whole batch.
pub fn trade_to_trade(t: &ArcusTrade, symbol: String) -> Result<Trade> {
    let side = match t.side.as_str() {
        "BUY" => OrderSide::Buy,
        "SELL" => OrderSide::Sell,
        other => return Err(anyhow!("unknown Arcus trade side: {:?}", other)),
    };
    let price = parse_decimal(&t.price, "price")?;
    let quantity = parse_decimal(&t.size, "size")?;
    let timestamp = unix_micros_to_datetime(t.timestamp);

    Ok(Trade {
        id: t.trade_id.clone(),
        symbol,
        price,
        quantity,
        side,
        timestamp,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Live BTC-USD sample captured 2026-09-11 (see `docs/plan/arcus_exchange_integration/api_reference.md`).
    fn btc_market_json() -> &'static str {
        r#"{
          "marketDisplayName": "BTC-USD",
          "fullAssetName": "Bitcoin",
          "marketId": 1,
          "status": "ONLINE",
          "baseAsset": "BTC",
          "quoteAsset": "USD",
          "tickSize": "0.1",
          "stepSize": "0.00000001",
          "tickTiers": [
            { "upToPrice": "500000", "tick": "0.1" },
            { "tick": "5" }
          ],
          "minOrderNotional": "5",
          "minOrderSize": "0.0001",
          "maxOrderSize": "10000",
          "oraclePrice": "76843.4",
          "markPrice": "76851.6",
          "lastTradePrice": "76850",
          "fundingRate": "0.0000125",
          "nextFundingRate": "0.0000125",
          "nextFundingAt": 1789099200,
          "priceChange24h": "-0.0185",
          "volume24h": "423.18",
          "volume24hNotional": "33270438.88",
          "high24h": "78539.9",
          "low24h": "76438.2",
          "trades24h": 78531,
          "openInterest": "59.8787351",
          "openInterestCapNotional": null,
          "initialMarginFraction": "0.025",
          "maintenanceMarginFraction": "0.016667",
          "offHoursInitialMarginFraction": "0.025",
          "regularTradingHours": null,
          "isOutsideRth": false,
          "currentSettlementPrice": null,
          "upperTradingBound": null,
          "lowerTradingBound": null,
          "nextUpperTradingBound": null,
          "nextLowerTradingBound": null,
          "isUpperInExpansionZone": null,
          "isLowerInExpansionZone": null,
          "upperZoneEnteredAt": null,
          "upperExpectedExpansionAt": null,
          "lowerZoneEnteredAt": null,
          "lowerExpectedExpansionAt": null,
          "type": "PERPETUAL",
          "category": "CRYPTO",
          "addedTimestamp": 1778786860,
          "assetResolution": "10000000000",
          "pythId": "1"
        }"#
    }

    fn btc_market() -> ArcusMarket {
        serde_json::from_str(btc_market_json()).expect("valid ArcusMarket fixture")
    }

    fn btc_orderbook() -> OrderbookSnapshot {
        serde_json::from_str(
            r#"{
              "bids": [["76842.5", "0.00780784"], ["76842.4", "0.39887012"]],
              "asks": [["76842.6", "0.00448121"], ["76845.3", "0.00092522"]],
              "lastSequenceId": 169595085,
              "globalSequenceId": 2007570573,
              "timestamp": 1789099058412792
            }"#,
        )
        .expect("valid OrderbookSnapshot fixture")
    }

    #[test]
    fn test_deserialize_equity_market_with_rth_fields() {
        // Confirms camelCase field mapping and nullable RTH/trading-bound fields for equities.
        let json = r#"{
          "marketDisplayName": "AAPL-USD",
          "fullAssetName": "Apple",
          "marketId": 30,
          "status": "ONLINE",
          "baseAsset": "AAPL",
          "quoteAsset": "USD",
          "tickSize": "0.01",
          "stepSize": "0.0000001",
          "tickTiers": [{ "tick": "0.5" }],
          "minOrderNotional": "5",
          "minOrderSize": "0.01",
          "maxOrderSize": "100000",
          "oraclePrice": "325.32",
          "markPrice": "325.32",
          "lastTradePrice": "325.22",
          "fundingRate": "0.000004791666666666",
          "nextFundingRate": "0.00000474537037037",
          "nextFundingAt": 1789099200,
          "priceChange24h": "0.0253",
          "volume24h": "223.37",
          "volume24hNotional": "71601.62",
          "high24h": "326.34",
          "low24h": "315.76",
          "trades24h": 126,
          "openInterest": "884.8014124",
          "openInterestCapNotional": "500000",
          "initialMarginFraction": "0.05",
          "maintenanceMarginFraction": "0.033334",
          "offHoursInitialMarginFraction": "0.075",
          "regularTradingHours": {
            "startSecondsOfDay": 14400,
            "endSecondsOfDay": 72000,
            "timezone": "America/New_York",
            "isOvernight": false
          },
          "isOutsideRth": true,
          "currentSettlementPrice": "325.6",
          "upperTradingBound": "333.74",
          "lowerTradingBound": "317.46",
          "nextUpperTradingBound": "341.88",
          "nextLowerTradingBound": "309.32",
          "isUpperInExpansionZone": false,
          "isLowerInExpansionZone": false,
          "upperZoneEnteredAt": null,
          "upperExpectedExpansionAt": null,
          "lowerZoneEnteredAt": null,
          "lowerExpectedExpansionAt": null,
          "type": "PERPETUAL",
          "category": "EQUITIES",
          "addedTimestamp": 1747310340,
          "assetResolution": "10000000000",
          "pythId": "922"
        }"#;
        let m: ArcusMarket = serde_json::from_str(json).expect("valid equity ArcusMarket fixture");
        assert_eq!(m.base_asset, "AAPL");
        assert_eq!(m.category, "EQUITIES");
        assert_eq!(m.market_type, "PERPETUAL");
        assert!(m.regular_trading_hours.is_some());
        assert_eq!(m.is_outside_rth, Some(true));
    }

    #[test]
    fn test_to_market() {
        let m = btc_market();
        let market = to_market(&m).unwrap();
        assert_eq!(market.contract, "BTC-USD");
        assert_eq!(market.price_scale, 1); // "0.1" -> 1 decimal place
        assert_eq!(market.quantity_scale, 8); // "0.00000001" -> 8 decimal places
        assert_eq!(market.max_leverage, Decimal::from(40)); // 1 / 0.025
    }

    #[test]
    fn test_volume_and_turnover_from_live_sample() {
        let m = btc_market();
        let ob = btc_orderbook();
        let ticker = to_ticker(&m, &ob, Utc::now()).unwrap();
        assert_eq!(
            ticker.volume_24h,
            Decimal::from_str_exact("423.18").unwrap()
        );
        assert_eq!(
            ticker.turnover_24h,
            Decimal::from_str_exact("33270438.88").unwrap()
        );
    }

    #[test]
    fn test_price_change_pct_is_decimal_ratio_not_percentage() {
        let m = btc_market();
        let ob = btc_orderbook();
        let ticker = to_ticker(&m, &ob, Utc::now()).unwrap();
        // Live sample: priceChange24h = "-0.0185" must stay -0.0185, not -1.85.
        assert_eq!(
            ticker.price_change_pct,
            Decimal::from_str_exact("-0.0185").unwrap()
        );
    }

    #[test]
    fn test_funding_rate_is_decimal_ratio() {
        let m = btc_market();
        let fr = to_funding_rate(&m).unwrap();
        assert_eq!(
            fr.funding_rate,
            Decimal::from_str_exact("0.0000125").unwrap()
        );
        assert_eq!(fr.funding_interval, 1);
        assert_eq!(
            fr.funding_time,
            fr.next_funding_time - chrono::Duration::hours(1)
        );
    }

    #[test]
    fn test_open_interest_notional_is_derived() {
        let m = btc_market();
        let oi = to_open_interest(&m, Utc::now()).unwrap();
        let expected = Decimal::from_str_exact("59.8787351").unwrap()
            * Decimal::from_str_exact("76851.6").unwrap();
        assert_eq!(oi.open_value, expected.round_dp(2));
    }

    #[test]
    fn test_complete_ticker_is_not_empty() {
        let m = btc_market();
        let ob = btc_orderbook();
        let ticker = to_ticker(&m, &ob, Utc::now()).unwrap();
        assert!(!ticker.is_empty());
    }

    #[test]
    fn test_ticker_without_top_of_book_would_be_empty() {
        // A markets-only conversion (no orderbook) has zero bid/ask — this is exactly why
        // get_ticker/get_all_tickers must always fetch top-of-book separately.
        let m = btc_market();
        let empty_ob = OrderbookSnapshot {
            bids: vec![],
            asks: vec![],
            last_sequence_id: 0,
            global_sequence_id: 0,
            timestamp: 0,
        };
        let ticker = to_ticker(&m, &empty_ob, Utc::now()).unwrap();
        assert!(ticker.is_empty());
    }

    #[test]
    fn test_orderbook_sort_order_and_no_crossed_book() {
        let ob = btc_orderbook();
        let orderbook = orderbook_snapshot_to_orderbook(&ob, "BTC".to_string()).unwrap();
        assert!(orderbook.bids.windows(2).all(|w| w[0].price >= w[1].price));
        assert!(orderbook.asks.windows(2).all(|w| w[0].price <= w[1].price));
        assert!(orderbook.best_bid().unwrap() < orderbook.best_ask().unwrap());
    }

    #[test]
    fn test_orderbook_timestamp_is_microseconds() {
        let ob = btc_orderbook();
        let orderbook = orderbook_snapshot_to_orderbook(&ob, "BTC".to_string()).unwrap();
        // 1789099058412792 us -> seconds component should be a sane Unix timestamp.
        assert_eq!(orderbook.timestamp.timestamp(), 1789099058);
    }

    #[test]
    fn test_kline_volume_turnover_from_live_sample() {
        let json = r#"{
          "marketDisplayName": "BTC-USD",
          "marketId": 1,
          "timeframe": "1h",
          "openTime": 1789095600000000,
          "open": "76828.4",
          "high": "76876.6",
          "low": "76688",
          "close": "76848.8",
          "volume": "7.86663936",
          "takerBuyVolume": "3.77401904",
          "notionalVolume": "604078.057082446",
          "takerBuyNotionalVolume": "289774.011220982",
          "tradeCount": 2182,
          "isFinal": false
        }"#;
        let c: ArcusCandle = serde_json::from_str(json).expect("valid ArcusCandle fixture");
        let kline = candle_to_kline(&c, "BTC".to_string(), "1h".to_string()).unwrap();
        assert_eq!(kline.volume, Decimal::from_str_exact("7.86663936").unwrap());
        assert_eq!(
            kline.turnover,
            Decimal::from_str_exact("604078.057082446").unwrap()
        );
        assert!(kline.close_time > kline.open_time);
    }

    #[test]
    fn test_trade_side_buy_and_sell() {
        let buy = ArcusTrade {
            market_display_name: "BTC-USD".to_string(),
            market_id: 1,
            side: "BUY".to_string(),
            price: "76839.7".to_string(),
            size: "0.00130141".to_string(),
            trade_id: "4310979".to_string(),
            timestamp: 1789099049226223,
        };
        let trade = trade_to_trade(&buy, "BTC".to_string()).unwrap();
        assert_eq!(trade.side, OrderSide::Buy);
        assert_eq!(trade.id, "4310979");

        let mut sell = buy;
        sell.side = "SELL".to_string();
        let trade = trade_to_trade(&sell, "BTC".to_string()).unwrap();
        assert_eq!(trade.side, OrderSide::Sell);
    }

    #[test]
    fn test_trade_unknown_side_is_error_not_default_buy() {
        let t = ArcusTrade {
            market_display_name: "BTC-USD".to_string(),
            market_id: 1,
            side: "HOLD".to_string(),
            price: "1".to_string(),
            size: "1".to_string(),
            trade_id: "1".to_string(),
            timestamp: 0,
        };
        assert!(trade_to_trade(&t, "BTC".to_string()).is_err());
    }

    #[test]
    fn test_funding_rate_history_uses_microsecond_time() {
        let e = ArcusFundingRate {
            market_id: 1,
            market_display_name: "BTC-USD".to_string(),
            funding_rate: "0.0000125".to_string(),
            time: 1789095600000000,
        };
        let fr = funding_rate_entry_to_funding_rate(&e, "BTC".to_string()).unwrap();
        assert_eq!(fr.funding_time.timestamp(), 1789095600);
        assert_eq!(fr.funding_interval, 1);
    }

    #[test]
    fn test_required_decimal_field_missing_is_error_not_zero() {
        let mut m = btc_market();
        m.mark_price = "not-a-number".to_string();
        assert!(to_market_stats(&m, Utc::now()).is_err());
    }

    #[test]
    fn test_price_change_pct_negative_100_percent_guarded() {
        // 1 + price_change_pct == 0 would divide by zero; must return ZERO, not panic.
        let result = derive_price_change_24h(Decimal::from(100), Decimal::from(-1));
        assert_eq!(result, Decimal::ZERO);
    }
}
