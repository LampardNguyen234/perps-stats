use crate::qfex::conversions::{normalize_qfex_symbol, parse_qfex_symbol};
use crate::qfex::ws_types::*;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::Utc;
use futures::{SinkExt, StreamExt};
use perps_core::streaming::*;
use perps_core::types::*;
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::str::FromStr;
use std::time::Duration;
use tokio::time;
use tokio_tungstenite::{connect_async, tungstenite::Message};

const WS_BASE_URL: &str = "wss://mds.qfex.com/";
const INACTIVITY_TIMEOUT_SECS: u64 = 30;

fn parse_levels(raw: &[[String; 2]]) -> Result<Vec<OrderbookLevel>> {
    raw.iter()
        .map(|pair| {
            Ok(OrderbookLevel {
                price: Decimal::from_str(&pair[0])
                    .map_err(|e| anyhow!("price parse error: {}", e))?,
                quantity: Decimal::from_str(&pair[1])
                    .map_err(|e| anyhow!("qty parse error: {}", e))?,
            })
        })
        .collect()
}

fn ws_l2_to_orderbook(msg: &WsL2Message) -> Result<Orderbook> {
    let bids = parse_levels(&msg.bid)?;
    let asks = parse_levels(&msg.ask)?;

    Ok(Orderbook {
        symbol: normalize_qfex_symbol(&msg.symbol),
        bids,
        asks,
        timestamp: Utc::now(),
    })
}

fn build_subscribe_msg(
    channels: Vec<String>,
    symbols: Vec<String>,
    sig_figs: &[u8],
) -> Result<String> {
    let req = WsSubscribeRequest {
        r#type: "subscribe".to_string(),
        channels,
        symbols,
        sig_figs: Some(sig_figs.to_vec()),
    };
    Ok(serde_json::to_string(&req)?)
}

#[derive(Default)]
struct ResolutionBooks(HashMap<String, std::collections::BTreeMap<u8, Orderbook>>);
impl ResolutionBooks {
    fn apply(&mut self, update: &WsL2Message) -> Result<MultiResolutionOrderbook> {
        let book = ws_l2_to_orderbook(update)?;
        let slots = self.0.entry(book.symbol.clone()).or_default();
        slots.insert(update.sig_figs, book.clone());
        // Missing resolutions remain absent until their own snapshot arrives.
        Ok(MultiResolutionOrderbook {
            symbol: book.symbol,
            timestamp: book.timestamp,
            orderbooks: slots.values().cloned().collect(),
        })
    }
}

/// Each adapter session owns its resolution slots and connection sequence.
#[derive(Clone)]
pub struct QfexWsClient {
    sig_figs: Vec<u8>,
}
impl QfexWsClient {
    pub fn new() -> Self {
        Self::with_sig_figs(vec![0])
    }
    pub fn with_sig_figs(sig_figs: Vec<u8>) -> Self {
        Self { sig_figs }
    }
}
impl Default for QfexWsClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl perps_core::WsOrderbookAdapter for QfexWsClient {
    fn exchange_name(&self) -> &str {
        "qfex"
    }
    async fn connect_and_subscribe(
        &self,
        symbols: Vec<String>,
    ) -> Result<DataStream<perps_core::OrderbookUpdate>> {
        let wire_symbols = symbols.iter().map(|s| parse_qfex_symbol(s)).collect();
        let (mut socket, _) = connect_async(WS_BASE_URL).await?;
        socket
            .send(Message::Text(build_subscribe_msg(
                vec!["level2".into()],
                wire_symbols,
                &self.sig_figs,
            )?))
            .await?;
        let sig_figs = self.sig_figs.clone();
        Ok(Box::pin(async_stream::try_stream! {
            let mut books = ResolutionBooks::default();
            let mut sequence = None;
            loop {
                let message = time::timeout(Duration::from_secs(INACTIVITY_TIMEOUT_SECS), socket.next()).await?;
                match message {
                    Some(Ok(Message::Text(text))) => {
                        let envelope: WsEnvelope = serde_json::from_str(&text)?;
                        if envelope.r#type.as_deref() != Some("level2") { continue; }
                        let update: WsL2Message = serde_json::from_str(&text)?;
                        if let Some(last) = sequence {
                            if update.sequence != last + 1 {
                                tracing::warn!(last, next = update.sequence, "QFEX sequence gap; received full snapshot");
                            }
                        }
                        sequence = Some(update.sequence);
                        if !sig_figs.contains(&update.sig_figs) { continue; }
                        yield perps_core::OrderbookUpdate::Full(books.apply(&update)?);
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

#[async_trait]
impl IPerpsStream for QfexWsClient {
    fn get_name(&self) -> &str {
        "qfex"
    }
    async fn stream_tickers(&self, _symbols: Vec<String>) -> Result<DataStream<Ticker>> {
        Err(anyhow!("QFEX ticker streaming is unsupported"))
    }
    async fn stream_trades(&self, _symbols: Vec<String>) -> Result<DataStream<Trade>> {
        Err(anyhow!("QFEX trade streaming is unsupported"))
    }
    async fn stream_orderbooks(&self, symbols: Vec<String>) -> Result<DataStream<Orderbook>> {
        use perps_core::{OrderbookUpdate, WsOrderbookAdapter};
        let mut stream = self.connect_and_subscribe(symbols).await?;
        Ok(Box::pin(async_stream::try_stream! {
            while let Some(item) = stream.next().await {
                if let OrderbookUpdate::Full(books) = item? {
                    for book in books.orderbooks { yield book; }
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
    fn frame(symbol: &str, sig_figs: u8, quantity: &str) -> WsL2Message {
        WsL2Message {
            sequence: 1,
            r#type: "level2".into(),
            time: "".into(),
            symbol: symbol.into(),
            bid: vec![["100".into(), quantity.into()]],
            ask: vec![["101".into(), quantity.into()]],
            sig_figs,
        }
    }
    #[test]
    fn resolutions_are_ordered_independent_and_reset_with_the_session() {
        let mut session = ResolutionBooks::default();
        let first = session.apply(&frame("BTC-USD", 2, "3")).unwrap();
        assert_eq!(first.orderbooks.len(), 1);
        session.apply(&frame("ETH-USD", 0, "99")).unwrap();
        session.apply(&frame("BTC-USD", 0, "1")).unwrap();
        let books = session.apply(&frame("BTC-USD", 1, "2")).unwrap();
        assert_eq!(
            books
                .orderbooks
                .iter()
                .map(|b| b.bids[0].quantity)
                .collect::<Vec<_>>(),
            vec![Decimal::ONE, Decimal::TWO, Decimal::from(3)]
        );
        let mut replacement = ResolutionBooks::default();
        assert_eq!(
            replacement
                .apply(&frame("BTC-USD", 0, "4"))
                .unwrap()
                .orderbooks
                .len(),
            1
        );
    }
}
