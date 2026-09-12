use super::conversions::{gravity_orderbook_to_orderbook, gravity_parse_symbol};
use super::types::GravityOrderbook;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use perps_core::streaming::*;
use perps_core::types::*;
use serde::Deserialize;
use std::collections::HashMap;
use std::time::Duration;
use tokio_tungstenite::{connect_async, tungstenite::Message};

const WS_URL: &str = "wss://market-data.grvt.io/ws/full";
const INACTIVITY_TIMEOUT_SECS: u64 = 30;
/// Selector rate (ms) and depth. `book.s` pushes a complete top-N book at this
/// rate for every subscribed instrument; `WsOrderbookManager` clips down to
/// whatever depth an individual `get_orderbook()` call actually asked for, so
/// one fixed subscription depth serves every caller.
const SELECTOR_SUFFIX: &str = "500-100";

/// Only the fields needed to route a `book.s` push to its symbol and body.
/// Subscribe acknowledgements (`{"jsonrpc": "2.0", "result": {...}}`) don't
/// have a `feed` key and fail this parse — treated as "not a book message"
/// rather than an error.
#[derive(Deserialize)]
struct BookSnapshotMsg {
    stream: String,
    feed: GravityOrderbook,
}

fn build_subscribe_msg(gravity_symbols: &[String]) -> String {
    let selectors: Vec<String> = gravity_symbols
        .iter()
        .map(|s| format!("{}@{}", s, SELECTOR_SUFFIX))
        .collect();
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": "subscribe",
        "params": {
            "stream": "v1.book.s",
            "selectors": selectors,
        },
        "id": 1,
    })
    .to_string()
}

/// Parse one WS text frame into an `Orderbook`, if it's a `book.s` snapshot for
/// a symbol we asked for. Anything else (subscribe acks, unrelated streams)
/// returns `Ok(None)`.
fn parse_book_snapshot(
    text: &str,
    gravity_to_normalized: &HashMap<String, String>,
) -> Result<Option<Orderbook>> {
    let msg: BookSnapshotMsg = match serde_json::from_str(text) {
        Ok(m) => m,
        Err(_) => return Ok(None),
    };
    if msg.stream != "v1.book.s" {
        return Ok(None);
    }
    let Some(normalized) = gravity_to_normalized.get(&msg.feed.instrument) else {
        return Ok(None);
    };
    let orderbook = gravity_orderbook_to_orderbook(msg.feed, normalized.clone())?;
    Ok(Some(orderbook))
}

#[derive(Clone, Default)]
pub struct GravityWsClient;

impl GravityWsClient {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl IPerpsStream for GravityWsClient {
    fn get_name(&self) -> &str {
        "gravity"
    }

    async fn stream_tickers(&self, _symbols: Vec<String>) -> Result<DataStream<Ticker>> {
        Err(anyhow!("Gravity ticker streaming is unsupported"))
    }

    async fn stream_trades(&self, _symbols: Vec<String>) -> Result<DataStream<Trade>> {
        Err(anyhow!("Gravity trade streaming is unsupported"))
    }

    /// `symbols` are normalized global symbols (e.g. "BTC"), per the
    /// `IPerpsStream` contract used by `WsOrderbookManager`/`FullOrderbookAdapter`.
    async fn stream_orderbooks(&self, symbols: Vec<String>) -> Result<DataStream<Orderbook>> {
        let gravity_to_normalized: HashMap<String, String> = symbols
            .into_iter()
            .map(|normalized| (gravity_parse_symbol(&normalized), normalized))
            .collect();
        let gravity_symbols: Vec<String> = gravity_to_normalized.keys().cloned().collect();

        let (mut socket, _) = connect_async(WS_URL).await?;
        socket
            .send(Message::Text(build_subscribe_msg(&gravity_symbols)))
            .await?;
        Ok(Box::pin(async_stream::try_stream! {
            loop {
                let message = tokio::time::timeout(
                    Duration::from_secs(INACTIVITY_TIMEOUT_SECS),
                    socket.next(),
                )
                .await?;
                match message {
                    Some(Ok(Message::Text(text))) => {
                        if let Some(book) = parse_book_snapshot(&text, &gravity_to_normalized)? {
                            yield book;
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
        Ok(Box::pin(
            self.stream_orderbooks(config.symbols)
                .await?
                .map(|item| item.map(StreamEvent::Orderbook)),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_book_snapshot_ignores_subscribe_ack() {
        let ack = r#"{"jsonrpc":"2.0","result":{"stream":"v1.book.s","subs":["BTC_USDT_Perp@500-100"]},"id":1,"method":"subscribe"}"#;
        let map = HashMap::new();
        assert!(parse_book_snapshot(ack, &map).unwrap().is_none());
    }

    #[test]
    fn parse_book_snapshot_ignores_unknown_symbol() {
        let msg = r#"{"stream":"v1.book.s","selector":"BTC_USDT_Perp@500-100","sequence_number":"1","feed":{"instrument":"BTC_USDT_Perp","event_time":"1702641000000000000","bids":[],"asks":[]},"prev_sequence_number":"0"}"#;
        let map = HashMap::new(); // no entry for BTC_USDT_Perp
        assert!(parse_book_snapshot(msg, &map).unwrap().is_none());
    }

    #[test]
    fn parse_book_snapshot_parses_known_symbol() {
        let msg = r#"{"stream":"v1.book.s","selector":"BTC_USDT_Perp@500-100","sequence_number":"1","feed":{"instrument":"BTC_USDT_Perp","event_time":"1702641000000000000","bids":[{"price":"50000","size":"1.5","num_orders":2}],"asks":[{"price":"50001","size":"2.0","num_orders":1}]},"prev_sequence_number":"0"}"#;
        let mut map = HashMap::new();
        map.insert("BTC_USDT_Perp".to_string(), "BTC".to_string());
        let book = parse_book_snapshot(msg, &map).unwrap().unwrap();
        assert_eq!(book.symbol, "BTC");
        assert_eq!(book.bids.len(), 1);
        assert_eq!(book.asks.len(), 1);
    }

    #[test]
    fn build_subscribe_msg_formats_selector() {
        let msg = build_subscribe_msg(&["BTC_USDT_Perp".to_string()]);
        assert!(msg.contains("BTC_USDT_Perp@500-100"));
        assert!(msg.contains("\"method\":\"subscribe\""));
    }
}
