use super::conversions::{
    depth_book_full_to_orderbook, standx_normalize_symbol, standx_parse_symbol,
};
use super::types::DepthBookResponse;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use futures::{SinkExt, StreamExt};
use perps_core::streaming::*;
use perps_core::types::*;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::Value;
use std::time::Duration;
use tokio_tungstenite::{connect_async, tungstenite::Message};

const WS_URL: &str = "wss://perps.standx.com/ws-stream/v1";
/// Server pings every 10s and disconnects after 5 minutes idle; connections are also
/// capped at 24h and auto-terminated (client must reconnect — handled by
/// `WsOrderbookManager`'s reconnect-on-close loop, same as every other WS exchange).
/// A healthy socket produces at least a Ping within this window even with no book activity.
const INACTIVITY_TIMEOUT_SECS: u64 = 30;

fn parse_decimal(s: &str) -> Result<Decimal> {
    Decimal::from_str_exact(s).map_err(|e| anyhow!("cannot parse decimal {:?}: {}", s, e))
}

fn parse_rfc3339(s: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| anyhow!("bad timestamp {:?}: {}", s, e))
}

/// `price` channel frame body. Live-verified 2026-09-13.
#[derive(Deserialize)]
struct PriceData {
    last_price: String,
    mark_price: String,
    index_price: String,
    /// `[best_bid_price, best_ask_price]`.
    spread: [String; 2],
    time: String,
}

/// `public_trade` channel frame body. Live-verified 2026-09-13 — this shape (integer `id`,
/// `side: "buy"|"sell"`, `time` as unix milliseconds) differs from the REST
/// `query_recent_trades` shape (`is_buyer_taker: bool`, RFC3339 `time` string, no `id`).
#[derive(Deserialize)]
struct PublicTradeData {
    id: i64,
    price: String,
    qty: String,
    side: String,
    /// Unix milliseconds.
    time: i64,
}

fn price_data_to_ticker(symbol: String, d: PriceData) -> Result<Ticker> {
    // This frame alone cannot populate a full Ticker (no volume/OI/high-low) — yields a
    // partial Ticker with those fields zeroed, matching `Ticker::is_empty()`'s documented
    // WS-ticker limitation.
    Ok(Ticker {
        symbol,
        last_price: parse_decimal(&d.last_price)?,
        mark_price: parse_decimal(&d.mark_price)?,
        index_price: parse_decimal(&d.index_price)?,
        best_bid_price: parse_decimal(&d.spread[0])?,
        best_bid_qty: Decimal::ZERO,
        best_ask_price: parse_decimal(&d.spread[1])?,
        best_ask_qty: Decimal::ZERO,
        volume_24h: Decimal::ZERO,
        turnover_24h: Decimal::ZERO,
        open_interest: Decimal::ZERO,
        open_interest_notional: Decimal::ZERO,
        price_change_24h: Decimal::ZERO,
        price_change_pct: Decimal::ZERO,
        high_price_24h: Decimal::ZERO,
        low_price_24h: Decimal::ZERO,
        timestamp: parse_rfc3339(&d.time)?,
    })
}

fn trade_data_to_trade(symbol: String, d: PublicTradeData) -> Result<Trade> {
    let side = match d.side.as_str() {
        "buy" | "Buy" => OrderSide::Buy,
        "sell" | "Sell" => OrderSide::Sell,
        other => anyhow::bail!("unknown public_trade side: {other}"),
    };
    let timestamp = Utc
        .timestamp_millis_opt(d.time)
        .single()
        .ok_or_else(|| anyhow!("bad public_trade time: {}", d.time))?;
    Ok(Trade {
        id: d.id.to_string(),
        symbol,
        price: parse_decimal(&d.price)?,
        quantity: parse_decimal(&d.qty)?,
        side,
        timestamp,
    })
}

fn depth_data_to_orderbook(symbol: String, data: Value) -> Result<Orderbook> {
    let raw: DepthBookResponse = serde_json::from_value(data)?;
    depth_book_full_to_orderbook(raw, symbol)
}

/// One subscribe message per (channel, symbol) pair — the docs only show one symbol per
/// frame. `[OPEN]` whether a batched form (many symbols in one frame) also works was not
/// verified live; harmless either way since `WsOrderbookManager` already shards to at most
/// `DEFAULT_MAX_SYMBOLS_PER_CONNECTION` (20) symbols per connection.
fn build_subscribe_msgs(standx_symbols: &[String], channel: &str) -> Vec<String> {
    standx_symbols
        .iter()
        .map(|s| {
            serde_json::json!({ "subscribe": { "channel": channel, "symbol": s } }).to_string()
        })
        .collect()
}

/// Dispatch by the top-level `channel` field before assuming any data shape — an
/// unrecognized frame (subscribe ack, ping/control, future channel) is skipped rather than
/// erroring the whole stream (mirrors Gravity's `book.s`-ack handling).
fn parse_envelope(text: &str) -> Option<(String, String, Value)> {
    let v: Value = serde_json::from_str(text).ok()?;
    let channel = v.get("channel")?.as_str()?.to_string();
    let symbol = v.get("symbol")?.as_str()?.to_string();
    let data = v.get("data")?.clone();
    Some((channel, symbol, data))
}

#[derive(Clone, Default)]
pub struct StandxWsClient;

impl StandxWsClient {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl IPerpsStream for StandxWsClient {
    fn get_name(&self) -> &str {
        "standx"
    }

    async fn stream_tickers(&self, symbols: Vec<String>) -> Result<DataStream<Ticker>> {
        let standx_symbols: Vec<String> = symbols.iter().map(|s| standx_parse_symbol(s)).collect();
        let (mut socket, _) = connect_async(WS_URL).await?;
        for msg in build_subscribe_msgs(&standx_symbols, "price") {
            socket.send(Message::Text(msg)).await?;
        }
        Ok(Box::pin(async_stream::try_stream! {
            loop {
                let message = tokio::time::timeout(
                    Duration::from_secs(INACTIVITY_TIMEOUT_SECS),
                    socket.next(),
                )
                .await?;
                match message {
                    Some(Ok(Message::Text(text))) => {
                        if let Some((channel, symbol, data)) = parse_envelope(&text) {
                            if channel == "price" {
                                if let Ok(d) = serde_json::from_value::<PriceData>(data) {
                                    if let Ok(ticker) = price_data_to_ticker(standx_normalize_symbol(&symbol), d) {
                                        yield ticker;
                                    }
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => socket.send(Message::Pong(payload)).await?,
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(error)) => Err(error)?,
                    _ => {}
                }
            }
        }))
    }

    async fn stream_trades(&self, symbols: Vec<String>) -> Result<DataStream<Trade>> {
        let standx_symbols: Vec<String> = symbols.iter().map(|s| standx_parse_symbol(s)).collect();
        let (mut socket, _) = connect_async(WS_URL).await?;
        for msg in build_subscribe_msgs(&standx_symbols, "public_trade") {
            socket.send(Message::Text(msg)).await?;
        }
        Ok(Box::pin(async_stream::try_stream! {
            loop {
                let message = tokio::time::timeout(
                    Duration::from_secs(INACTIVITY_TIMEOUT_SECS),
                    socket.next(),
                )
                .await?;
                match message {
                    Some(Ok(Message::Text(text))) => {
                        if let Some((channel, symbol, data)) = parse_envelope(&text) {
                            if channel == "public_trade" {
                                if let Ok(d) = serde_json::from_value::<PublicTradeData>(data) {
                                    if let Ok(trade) = trade_data_to_trade(standx_normalize_symbol(&symbol), d) {
                                        yield trade;
                                    }
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => socket.send(Message::Pong(payload)).await?,
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(error)) => Err(error)?,
                    _ => {}
                }
            }
        }))
    }

    /// `symbols` are normalized global symbols, per the `IPerpsStream` contract used by
    /// `WsOrderbookManager`/`FullOrderbookAdapter`. `depth_book` sends a full snapshot every
    /// message (confirmed live and by docs prose) — no Rule 1-4 continuity logic applies.
    async fn stream_orderbooks(&self, symbols: Vec<String>) -> Result<DataStream<Orderbook>> {
        let standx_symbols: Vec<String> = symbols.iter().map(|s| standx_parse_symbol(s)).collect();
        let (mut socket, _) = connect_async(WS_URL).await?;
        for msg in build_subscribe_msgs(&standx_symbols, "depth_book") {
            socket.send(Message::Text(msg)).await?;
        }
        Ok(Box::pin(async_stream::try_stream! {
            loop {
                let message = tokio::time::timeout(
                    Duration::from_secs(INACTIVITY_TIMEOUT_SECS),
                    socket.next(),
                )
                .await?;
                match message {
                    Some(Ok(Message::Text(text))) => {
                        if let Some((channel, symbol, data)) = parse_envelope(&text) {
                            if channel == "depth_book" {
                                if let Ok(book) = depth_data_to_orderbook(standx_normalize_symbol(&symbol), data) {
                                    yield book;
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => socket.send(Message::Pong(payload)).await?,
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(error)) => Err(error)?,
                    _ => {}
                }
            }
        }))
    }

    async fn stream_multi(&self, config: StreamConfig) -> Result<DataStream<StreamEvent>> {
        let standx_symbols: Vec<String> = config
            .symbols
            .iter()
            .map(|s| standx_parse_symbol(s))
            .collect();

        let mut channels: Vec<&str> = Vec::new();
        if config.data_types.contains(&StreamDataType::Ticker) {
            channels.push("price");
        }
        if config.data_types.contains(&StreamDataType::Trade) {
            channels.push("public_trade");
        }
        if config.data_types.contains(&StreamDataType::Orderbook) {
            channels.push("depth_book");
        }
        if channels.is_empty() {
            anyhow::bail!(
                "StandX stream_multi: no supported data types requested (supported: ticker, trade, orderbook)"
            );
        }

        let (mut socket, _) = connect_async(WS_URL).await?;
        for channel in &channels {
            for msg in build_subscribe_msgs(&standx_symbols, channel) {
                socket.send(Message::Text(msg)).await?;
            }
        }

        Ok(Box::pin(async_stream::try_stream! {
            loop {
                let message = tokio::time::timeout(
                    Duration::from_secs(INACTIVITY_TIMEOUT_SECS),
                    socket.next(),
                )
                .await?;
                match message {
                    Some(Ok(Message::Text(text))) => {
                        if let Some((channel, symbol, data)) = parse_envelope(&text) {
                            let normalized = standx_normalize_symbol(&symbol);
                            match channel.as_str() {
                                "price" => {
                                    if let Ok(d) = serde_json::from_value::<PriceData>(data) {
                                        if let Ok(t) = price_data_to_ticker(normalized, d) {
                                            yield StreamEvent::Ticker(t);
                                        }
                                    }
                                }
                                "public_trade" => {
                                    if let Ok(d) = serde_json::from_value::<PublicTradeData>(data) {
                                        if let Ok(t) = trade_data_to_trade(normalized, d) {
                                            yield StreamEvent::Trade(t);
                                        }
                                    }
                                }
                                "depth_book" => {
                                    if let Ok(ob) = depth_data_to_orderbook(normalized, data) {
                                        yield StreamEvent::Orderbook(ob);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => socket.send(Message::Pong(payload)).await?,
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(error)) => Err(error)?,
                    _ => {}
                }
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_envelope_ignores_non_channel_frames() {
        let ack = r#"{"subscribed":{"channel":"price","symbol":"BTC-USD"}}"#;
        assert!(parse_envelope(ack).is_none());
    }

    #[test]
    fn parse_envelope_parses_price_frame() {
        let msg = r#"{"seq":1,"channel":"price","symbol":"BTC-USD","data":{"base":"BTC","index_price":"77298.61","last_price":"77260.99","mark_price":"77260.99","mid_price":"77254.50","quote":"DUSD","spread":["77248.01","77260.99"],"symbol":"BTC-USD","time":"2026-09-13T05:41:06.104799291Z"}}"#;
        let (channel, symbol, data) = parse_envelope(msg).unwrap();
        assert_eq!(channel, "price");
        assert_eq!(symbol, "BTC-USD");
        let d: PriceData = serde_json::from_value(data).unwrap();
        let ticker = price_data_to_ticker("BTC".to_string(), d).unwrap();
        assert_eq!(ticker.symbol, "BTC");
        assert!(ticker.last_price > Decimal::ZERO);
        assert!(ticker.volume_24h.is_zero());
    }

    #[test]
    fn parse_envelope_parses_public_trade_frame() {
        let msg = r#"{"seq":3,"channel":"public_trade","symbol":"BTC-USD","data":{"id":465880883,"price":"77248.06","qty":"0.0001","side":"sell","symbol":"BTC-USD","time":1789278068947}}"#;
        let (channel, _symbol, data) = parse_envelope(msg).unwrap();
        assert_eq!(channel, "public_trade");
        let d: PublicTradeData = serde_json::from_value(data).unwrap();
        let trade = trade_data_to_trade("BTC".to_string(), d).unwrap();
        assert_eq!(trade.id, "465880883");
        assert_eq!(trade.side, OrderSide::Sell);
    }

    #[test]
    fn parse_envelope_parses_depth_book_frame() {
        let msg = r#"{"seq":2,"channel":"depth_book","symbol":"BTC-USD","data":{"asks":[["77261","0.0375"],["77262","0.0003"]],"bids":[["77248","0.0386"],["77245","0.5178"]],"symbol":"BTC-USD"}}"#;
        let (channel, symbol, data) = parse_envelope(msg).unwrap();
        assert_eq!(channel, "depth_book");
        let book = depth_data_to_orderbook(standx_normalize_symbol(&symbol), data).unwrap();
        assert_eq!(book.symbol, "BTC");
        assert_eq!(book.asks[0].price, Decimal::from(77261));
        assert_eq!(book.bids[0].price, Decimal::from(77248));
    }

    #[test]
    fn build_subscribe_msgs_formats_frame() {
        let msgs = build_subscribe_msgs(&["BTC-USD".to_string()], "price");
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].contains(r#""channel":"price""#));
        assert!(msgs[0].contains(r#""symbol":"BTC-USD""#));
    }
}
