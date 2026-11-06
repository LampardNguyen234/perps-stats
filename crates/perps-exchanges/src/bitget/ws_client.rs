use crate::bitget::conversions::{parse_decimal, parse_decimal_value, unix_millis_str_to_datetime};
use crate::bitget::ws_types::*;
use anyhow::Result;
use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use futures::{SinkExt, StreamExt};
use perps_core::streaming::*;
use perps_core::types::*;
use std::time::Duration;
use tokio_tungstenite::{connect_async, tungstenite::Message};

const WS_URL: &str = "wss://ws.bitget.com/v3/ws/public";
/// Bitget's documented convention (other API generations) is a plain-text `"ping"` every
/// 20-30s; Phase 0's live probe didn't observe a `"pong"` reply in its short window, so this
/// client sends pings defensively but never hard-fails on a missing reply — relies on
/// inactivity timeout + reconnect-on-close instead (see `ConnectionConfig::default()`).
const PING_INTERVAL_SECS: u64 = 20;
const INACTIVITY_TIMEOUT_SECS: u64 = 90;

fn ws_ticker_to_ticker(symbol: String, d: WsTickerData, ts: i64) -> Result<Ticker> {
    let last_price = parse_decimal(&d.last_price)?;
    let open_price_24h = parse_decimal(&d.open_price_24h)?;
    let mark_price = parse_decimal(&d.mark_price)?;
    let open_interest = parse_decimal(&d.open_interest)?;

    Ok(Ticker {
        symbol,
        last_price,
        mark_price,
        index_price: parse_decimal(&d.index_price)?,
        best_bid_price: parse_decimal(&d.bid1_price)?,
        best_bid_qty: parse_decimal(&d.bid1_size)?,
        best_ask_price: parse_decimal(&d.ask1_price)?,
        best_ask_qty: parse_decimal(&d.ask1_size)?,
        volume_24h: parse_decimal(&d.volume24h)?,
        turnover_24h: parse_decimal(&d.turnover24h)?,
        open_interest,
        open_interest_notional: open_interest * mark_price,
        price_change_24h: last_price - open_price_24h,
        price_change_pct: parse_decimal(&d.price_24h_pcnt)?,
        high_price_24h: parse_decimal(&d.high_price_24h)?,
        low_price_24h: parse_decimal(&d.low_price_24h)?,
        timestamp: Utc
            .timestamp_millis_opt(ts)
            .single()
            .unwrap_or_else(Utc::now),
    })
}

fn ws_trade_to_trade(symbol: String, d: WsTradeData) -> Result<Trade> {
    Ok(Trade {
        id: d.i,
        symbol,
        price: parse_decimal(&d.p)?,
        quantity: parse_decimal(&d.v)?,
        side: if d.side == "buy" {
            OrderSide::Buy
        } else {
            OrderSide::Sell
        },
        timestamp: unix_millis_str_to_datetime(&d.ts)?,
    })
}

fn ws_book_to_orderbook(symbol: String, d: WsBookData) -> Result<Orderbook> {
    let bids =
        d.b.iter()
            .map(|[price, qty]| {
                Ok(OrderbookLevel {
                    price: parse_decimal_value(price)?,
                    quantity: parse_decimal_value(qty)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

    let asks =
        d.a.iter()
            .map(|[price, qty]| {
                Ok(OrderbookLevel {
                    price: parse_decimal_value(price)?,
                    quantity: parse_decimal_value(qty)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

    Ok(Orderbook {
        symbol,
        bids,
        asks,
        timestamp: unix_millis_str_to_datetime(&d.ts)?,
    })
}

/// `try_stream!`'s `?` rewriting only sees top-level expressions in its generator body —
/// nested inside a `tokio::select!` arm, `?` resolves against the wrong scope and fails to
/// compile. Every loop below resolves the select into this enum first, then uses `?` in a
/// plain `match` at the generator's top level.
enum WsTick {
    Ping,
    Message(
        std::result::Result<
            Option<std::result::Result<Message, tokio_tungstenite::tungstenite::Error>>,
            tokio::time::error::Elapsed,
        >,
    ),
}

fn subscribe_message(symbols: &[String], topics: &[&str]) -> String {
    let args: Vec<serde_json::Value> = topics
        .iter()
        .flat_map(|topic| {
            symbols.iter().map(move |s| {
                serde_json::json!({ "instType": "usdt-futures", "topic": topic, "symbol": s })
            })
        })
        .collect();
    serde_json::json!({ "op": "subscribe", "args": args }).to_string()
}

/// Parses `{"action","arg":{"topic","symbol",...},"data":[...],"ts"}`, returning `None` for
/// anything that isn't a data envelope (subscribe acks, control frames) — defensive per the
/// retro's "model control frames defensively" lesson, never panics on an unrecognized shape.
fn parse_envelope<T: serde::de::DeserializeOwned>(text: &str) -> Option<(String, String, T, i64)> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    v.get("action")?.as_str()?;
    let arg = v.get("arg")?;
    let topic = arg.get("topic")?.as_str()?.to_string();
    let symbol = arg.get("symbol")?.as_str()?.to_string();
    let ts = v.get("ts")?.as_i64()?;
    let data: Vec<T> = serde_json::from_value(v.get("data")?.clone()).ok()?;
    let first = data.into_iter().next()?;
    Some((topic, symbol, first, ts))
}

#[derive(Clone, Default)]
pub struct BitgetWsClient;

impl BitgetWsClient {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl IPerpsStream for BitgetWsClient {
    fn get_name(&self) -> &str {
        "bitget"
    }

    async fn stream_tickers(&self, symbols: Vec<String>) -> Result<DataStream<Ticker>> {
        let (mut socket, _) = connect_async(WS_URL).await?;
        socket
            .send(Message::Text(subscribe_message(&symbols, &["ticker"])))
            .await?;
        let mut ping_tick = tokio::time::interval(Duration::from_secs(PING_INTERVAL_SECS));

        Ok(Box::pin(async_stream::try_stream! {
            loop {
                let tick = tokio::select! {
                    _ = ping_tick.tick() => WsTick::Ping,
                    message = tokio::time::timeout(Duration::from_secs(INACTIVITY_TIMEOUT_SECS), socket.next()) => WsTick::Message(message),
                };
                match tick {
                    WsTick::Ping => {
                        socket.send(Message::Text("ping".to_string())).await?;
                    }
                    WsTick::Message(timeout_result) => {
                        match timeout_result? {
                            Some(Ok(Message::Text(text))) => {
                                if let Some((topic, symbol, data, ts)) = parse_envelope::<WsTickerData>(&text) {
                                    if topic == "ticker" {
                                        if let Ok(ticker) = ws_ticker_to_ticker(symbol, data, ts) {
                                            yield ticker;
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
                }
            }
        }))
    }

    async fn stream_trades(&self, symbols: Vec<String>) -> Result<DataStream<Trade>> {
        let (mut socket, _) = connect_async(WS_URL).await?;
        socket
            .send(Message::Text(subscribe_message(&symbols, &["publicTrade"])))
            .await?;
        let mut ping_tick = tokio::time::interval(Duration::from_secs(PING_INTERVAL_SECS));

        Ok(Box::pin(async_stream::try_stream! {
            loop {
                let tick = tokio::select! {
                    _ = ping_tick.tick() => WsTick::Ping,
                    message = tokio::time::timeout(Duration::from_secs(INACTIVITY_TIMEOUT_SECS), socket.next()) => WsTick::Message(message),
                };
                match tick {
                    WsTick::Ping => {
                        socket.send(Message::Text("ping".to_string())).await?;
                    }
                    WsTick::Message(timeout_result) => {
                        match timeout_result? {
                            Some(Ok(Message::Text(text))) => {
                                if let Some((topic, symbol, data, _ts)) = parse_envelope::<WsTradeData>(&text) {
                                    if topic == "publicTrade" {
                                        if let Ok(trade) = ws_trade_to_trade(symbol, data) {
                                            yield trade;
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
                }
            }
        }))
    }

    /// `symbols` are normalized global symbols, per the `IPerpsStream` contract used by
    /// `WsOrderbookManager`/`FullOrderbookAdapter`. `books5` sends a full snapshot every
    /// push (live-confirmed) — no `seq`/`pseq` resync logic needed.
    async fn stream_orderbooks(&self, symbols: Vec<String>) -> Result<DataStream<Orderbook>> {
        let (mut socket, _) = connect_async(WS_URL).await?;
        socket
            .send(Message::Text(subscribe_message(&symbols, &["books5"])))
            .await?;
        let mut ping_tick = tokio::time::interval(Duration::from_secs(PING_INTERVAL_SECS));

        Ok(Box::pin(async_stream::try_stream! {
            loop {
                let tick = tokio::select! {
                    _ = ping_tick.tick() => WsTick::Ping,
                    message = tokio::time::timeout(Duration::from_secs(INACTIVITY_TIMEOUT_SECS), socket.next()) => WsTick::Message(message),
                };
                match tick {
                    WsTick::Ping => {
                        socket.send(Message::Text("ping".to_string())).await?;
                    }
                    WsTick::Message(timeout_result) => {
                        match timeout_result? {
                            Some(Ok(Message::Text(text))) => {
                                if let Some((topic, symbol, data, _ts)) = parse_envelope::<WsBookData>(&text) {
                                    if topic == "books5" {
                                        if let Ok(book) = ws_book_to_orderbook(symbol, data) {
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
                }
            }
        }))
    }

    async fn stream_multi(&self, config: StreamConfig) -> Result<DataStream<StreamEvent>> {
        let mut topics: Vec<&str> = Vec::new();
        if config.data_types.contains(&StreamDataType::Ticker) {
            topics.push("ticker");
        }
        if config.data_types.contains(&StreamDataType::Trade) {
            topics.push("publicTrade");
        }
        if config.data_types.contains(&StreamDataType::Orderbook) {
            topics.push("books5");
        }
        if topics.is_empty() {
            anyhow::bail!(
                "Bitget stream_multi: no supported data types requested (supported: ticker, trade, orderbook — funding rate has no dedicated channel, it's a field on ticker)"
            );
        }

        let (mut socket, _) = connect_async(WS_URL).await?;
        socket
            .send(Message::Text(subscribe_message(&config.symbols, &topics)))
            .await?;
        let mut ping_tick = tokio::time::interval(Duration::from_secs(PING_INTERVAL_SECS));

        Ok(Box::pin(async_stream::try_stream! {
            loop {
                let tick = tokio::select! {
                    _ = ping_tick.tick() => WsTick::Ping,
                    message = tokio::time::timeout(Duration::from_secs(INACTIVITY_TIMEOUT_SECS), socket.next()) => WsTick::Message(message),
                };
                match tick {
                    WsTick::Ping => {
                        socket.send(Message::Text("ping".to_string())).await?;
                    }
                    WsTick::Message(timeout_result) => {
                        match timeout_result? {
                            Some(Ok(Message::Text(text))) => {
                                // Dispatch by peeking `arg.topic` before committing to a data shape.
                                let topic: Option<String> = serde_json::from_str::<serde_json::Value>(&text)
                                    .ok()
                                    .and_then(|v| v.get("arg").and_then(|a| a.get("topic")).and_then(|t| t.as_str()).map(|s| s.to_string()));
                                match topic.as_deref() {
                                    Some("ticker") => {
                                        if let Some((_, symbol, data, ts)) = parse_envelope::<WsTickerData>(&text) {
                                            if let Ok(t) = ws_ticker_to_ticker(symbol, data, ts) {
                                                yield StreamEvent::Ticker(t);
                                            }
                                        }
                                    }
                                    Some("publicTrade") => {
                                        if let Some((_, symbol, data, _ts)) = parse_envelope::<WsTradeData>(&text) {
                                            if let Ok(t) = ws_trade_to_trade(symbol, data) {
                                                yield StreamEvent::Trade(t);
                                            }
                                        }
                                    }
                                    Some("books5") => {
                                        if let Some((_, symbol, data, _ts)) = parse_envelope::<WsBookData>(&text) {
                                            if let Ok(ob) = ws_book_to_orderbook(symbol, data) {
                                                yield StreamEvent::Orderbook(ob);
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            Some(Ok(Message::Ping(payload))) => socket.send(Message::Pong(payload)).await?,
                            Some(Ok(Message::Close(_))) | None => break,
                            Some(Err(error)) => Err(error)?,
                            _ => {}
                        }
                    }
                }
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    #[test]
    fn parse_envelope_ignores_subscribe_ack() {
        let ack = r#"{"event":"subscribe","arg":{"instType":"usdt-futures","topic":"ticker","symbol":"BTCUSDT"},"connId":"abc"}"#;
        assert!(parse_envelope::<WsTickerData>(ack).is_none());
    }

    #[test]
    fn parse_envelope_parses_ticker_frame() {
        let msg = r#"{"action":"snapshot","arg":{"instType":"usdt-futures","topic":"ticker","symbol":"BTCUSDT"},"data":[{"highPrice24h":"80812.6","lowPrice24h":"79320.1","openPrice24h":"79518.2","lastPrice":"79852.8","turnover24h":"2484916854.56567","volume24h":"31036.2517","bid1Price":"79852.8","ask1Price":"79852.9","bid1Size":"0.6751","ask1Size":"10.6181","price24hPcnt":"0.00421","indexPrice":"79887.9965","markPrice":"79852.8","fundingRate":"0.0001","openInterest":"36992.0609999999565","deliveryTime":"","deliveryStartTime":"","deliveryStatus":"","nextFundingTime":"1793088000000"}],"ts":1793069735975}"#;
        let (topic, symbol, data, ts) = parse_envelope::<WsTickerData>(msg).unwrap();
        assert_eq!(topic, "ticker");
        assert_eq!(symbol, "BTCUSDT");
        let ticker = ws_ticker_to_ticker("BTC".to_string(), data, ts).unwrap();
        assert_eq!(ticker.symbol, "BTC");
        assert!(ticker.last_price > Decimal::ZERO);
        assert!(ticker.volume_24h > Decimal::ZERO);
    }

    #[test]
    fn parse_envelope_parses_books5_frame() {
        let msg = r#"{"action":"snapshot","arg":{"instType":"usdt-futures","topic":"books5","symbol":"BTCUSDT"},"data":[{"a":[["79852.9","10.6181"],["79853.3","0.0015"]],"b":[["79852.8","0.4409"],["79852.6","0.0001"]],"seq":1151850456321,"pseq":0,"ts":"1793069736001"}],"ts":1793069736004}"#;
        let (topic, symbol, data, _ts) = parse_envelope::<WsBookData>(msg).unwrap();
        assert_eq!(topic, "books5");
        let book = ws_book_to_orderbook(symbol, data).unwrap();
        assert_eq!(book.asks[0].price, Decimal::from_str("79852.9").unwrap());
        assert_eq!(book.bids[0].price, Decimal::from_str("79852.8").unwrap());
        // no crossed book
        assert!(book.bids[0].price < book.asks[0].price);
    }

    #[test]
    fn parse_envelope_parses_trade_frame() {
        let msg = r#"{"action":"update","arg":{"instType":"usdt-futures","topic":"publicTrade","symbol":"BTCUSDT"},"data":[{"i":"1498808593995808774","p":"79852.8","v":"0.0008","S":"sell","T":"1793069736002","L":"1498808593995808775","isRPI":"no"}],"ts":1793069736004}"#;
        let (topic, symbol, data, _ts) = parse_envelope::<WsTradeData>(msg).unwrap();
        assert_eq!(topic, "publicTrade");
        let trade = ws_trade_to_trade(symbol, data).unwrap();
        assert_eq!(trade.id, "1498808593995808774");
        assert_eq!(trade.side, OrderSide::Sell);
    }

    #[test]
    fn subscribe_message_batches_topics_and_symbols() {
        let msg = subscribe_message(
            &["BTCUSDT".to_string(), "ETHUSDT".to_string()],
            &["ticker", "books5"],
        );
        let v: serde_json::Value = serde_json::from_str(&msg).unwrap();
        assert_eq!(v["op"], "subscribe");
        assert_eq!(v["args"].as_array().unwrap().len(), 4);
    }
}
