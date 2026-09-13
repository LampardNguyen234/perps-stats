//! Public market-data WebSocket client for 01.xyz (Nord)
//! (`wss://zo-mainnet.n1.xyz/ws/{stream1}&{stream2}&...`).
//!
//! Confirmed live 2026-09-13: Nord has no push ticker channel at all - only
//! `trades@{symbol}`, `deltas@{symbol}` (orderbook), and `candle@{symbol}:{res}` (which
//! carries neither a symbol nor mark/index price/OI/24h-volume, so it can't back a
//! `Ticker` either). `stream_tickers` therefore returns an error rather than fabricating a
//! `Ticker` from partial data, matching the precedent `GravityWsClient`/`QfexWsClient`/
//! `RisexWsClient` already set in this codebase for channels their exchange doesn't push.
//!
//! `deltas@{symbol}` is a genuine incremental delta stream (first frame after connecting
//! carries only a handful of changed levels, not a full book) - the client seeds a local
//! book per symbol from the REST client's uncached orderbook snapshot before applying WS
//! deltas, then treats an `update_id` discontinuity as a warn-and-continue condition, same
//! lenient behavior `EdgexWsClient`'s `DepthBook` already uses in this codebase.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::{SinkExt, StreamExt};
use perps_core::streaming::{DataStream, IPerpsStream, StreamConfig, StreamDataType, StreamEvent};
use perps_core::{IPerps, OrderSide, Orderbook, OrderbookLevel, Ticker, Trade};
use rust_decimal::Decimal;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use super::client::O1Client;
use super::conversions::f64_to_decimal;
use super::types::NordOrderbookInfo;
use super::ws_types::{NordWsDelta, NordWsTrade, NordWsTradesBatch};

const WS_BASE_URL: &str = "wss://zo-mainnet.n1.xyz/ws";
/// Confirmed via Nord docs: a single connection accepts at most 12 combined streams.
const MAX_STREAMS_PER_CONNECTION: usize = 12;

/// Local order book maintained per symbol from a REST snapshot plus WS deltas.
#[derive(Default)]
struct DepthBook {
    bids: BTreeMap<Decimal, Decimal>,
    asks: BTreeMap<Decimal, Decimal>,
    last_update_id: Option<u64>,
    /// Whether a delta has been applied since `seed_from_snapshot`. The REST snapshot is
    /// fetched (one HTTP round trip per symbol, sequentially) before the shared WS
    /// connection opens, so live trades in that window routinely make the very first
    /// delta's `last_update_id` not match the snapshot's `update_id` - expected and
    /// harmless (the delta is still applied correctly as an absolute replacement), not a
    /// real missed frame. That expected mismatch is logged at `debug`, not `warn`; only a
    /// mismatch between two consecutive *deltas* (this flag already `true`) indicates an
    /// actual gap.
    synced: bool,
}

fn apply_level(map: &mut BTreeMap<Decimal, Decimal>, price: f64, size: f64) {
    let price = f64_to_decimal(price);
    let size = f64_to_decimal(size);
    if size.is_zero() {
        map.remove(&price);
    } else {
        map.insert(price, size);
    }
}

impl DepthBook {
    fn seed_from_snapshot(&mut self, snapshot: &NordOrderbookInfo) {
        self.bids.clear();
        self.asks.clear();
        for &(price, size) in &snapshot.bids {
            apply_level(&mut self.bids, price, size);
        }
        for &(price, size) in &snapshot.asks {
            apply_level(&mut self.asks, price, size);
        }
        self.last_update_id = Some(snapshot.update_id);
        self.synced = false;
    }

    fn apply_delta(&mut self, delta: &NordWsDelta) {
        if let Some(last) = self.last_update_id {
            if delta.last_update_id != last {
                if self.synced {
                    tracing::warn!(
                        symbol = %delta.market_symbol,
                        expected = last,
                        got = delta.last_update_id,
                        diff = delta.last_update_id as i64 - last as i64,
                        "01 orderbook: possible missed frame (update_id gap)"
                    );
                } else {
                    tracing::debug!(
                        symbol = %delta.market_symbol,
                        snapshot_update_id = last,
                        first_delta_last_update_id = delta.last_update_id,
                        "01 orderbook: first delta after snapshot doesn't chain exactly (expected - REST snapshot predates WS connect)"
                    );
                }
            }
        }
        self.synced = true;
        for &(price, size) in &delta.bids {
            apply_level(&mut self.bids, price, size);
        }
        for &(price, size) in &delta.asks {
            apply_level(&mut self.asks, price, size);
        }
        self.last_update_id = Some(delta.update_id);
    }

    fn to_orderbook(&self, symbol: &str) -> Orderbook {
        Orderbook {
            symbol: symbol.to_string(),
            bids: self
                .bids
                .iter()
                .rev()
                .map(|(&price, &quantity)| OrderbookLevel { price, quantity })
                .collect(),
            asks: self
                .asks
                .iter()
                .map(|(&price, &quantity)| OrderbookLevel { price, quantity })
                .collect(),
            timestamp: Utc::now(),
        }
    }
}

/// Confirmed live 2026-09-13: WS trade `side` uses the same `"bid"`/`"ask"` vocabulary as
/// REST `NordTrade.taker_side` (`nord_trade_to_trade` in `conversions.rs`) - `"bid"` maps to
/// `OrderSide::Buy`, `"ask"` to `OrderSide::Sell`.
fn ws_trade_to_trade(t: &NordWsTrade, symbol: &str) -> Result<Trade> {
    let side = match t.side.as_str() {
        "bid" => OrderSide::Buy,
        "ask" => OrderSide::Sell,
        other => return Err(anyhow!("01 ws: unknown trade side: {}", other)),
    };
    let timestamp: DateTime<Utc> = DateTime::parse_from_rfc3339(&t.physical_time)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| {
            anyhow!(
                "01 ws: failed to parse trade time '{}': {}",
                t.physical_time,
                e
            )
        })?;
    Ok(Trade {
        id: t.trade_id.to_string(),
        symbol: symbol.to_string(),
        price: f64_to_decimal(t.price),
        quantity: f64_to_decimal(t.size),
        side,
        timestamp,
    })
}

fn dedup_symbols(symbols: Vec<String>) -> Result<Vec<String>> {
    if symbols.is_empty() {
        return Err(anyhow!("at least one symbol is required for 01 streaming"));
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for symbol in symbols {
        if seen.insert(symbol.to_uppercase()) {
            out.push(symbol);
        }
    }
    Ok(out)
}

/// Public market-data WebSocket client for 01.xyz (Nord).
///
/// Holds an owned `O1Client` purely to reuse its cached symbol<->market_id resolution and
/// its rate-limited REST GET (for the one-off orderbook snapshot needed to seed each
/// symbol's local book) - no second HTTP client, cache, or rate limiter.
#[derive(Clone)]
pub struct O1WsClient {
    rest: O1Client,
    ws_base_url: String,
}

impl O1WsClient {
    pub fn new() -> Self {
        Self {
            rest: O1Client::new(),
            ws_base_url: WS_BASE_URL.to_string(),
        }
    }
}

impl Default for O1WsClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IPerpsStream for O1WsClient {
    fn get_name(&self) -> &str {
        "01"
    }

    async fn stream_tickers(&self, _symbols: Vec<String>) -> Result<DataStream<Ticker>> {
        Err(anyhow!(
            "01 (Nord) has no push ticker channel over WebSocket"
        ))
    }

    async fn stream_trades(&self, symbols: Vec<String>) -> Result<DataStream<Trade>> {
        let stream = self
            .stream_multi(StreamConfig {
                symbols,
                data_types: vec![StreamDataType::Trade],
                auto_reconnect: false,
                kline_interval: None,
            })
            .await?;

        Ok(Box::pin(stream.filter_map(|item| async move {
            match item {
                Ok(StreamEvent::Trade(trade)) => Some(Ok(trade)),
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            }
        })))
    }

    async fn stream_orderbooks(&self, symbols: Vec<String>) -> Result<DataStream<Orderbook>> {
        let stream = self
            .stream_multi(StreamConfig {
                symbols,
                data_types: vec![StreamDataType::Orderbook],
                auto_reconnect: false,
                kline_interval: None,
            })
            .await?;

        Ok(Box::pin(stream.filter_map(|item| async move {
            match item {
                Ok(StreamEvent::Orderbook(ob)) => Some(Ok(ob)),
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            }
        })))
    }

    async fn stream_multi(&self, config: StreamConfig) -> Result<DataStream<StreamEvent>> {
        let wants_trade = config.data_types.contains(&StreamDataType::Trade);
        let wants_orderbook = config.data_types.contains(&StreamDataType::Orderbook);
        let wants_unsupported = config.data_types.contains(&StreamDataType::Ticker)
            || config.data_types.contains(&StreamDataType::FundingRate)
            || config.data_types.contains(&StreamDataType::Kline);
        if wants_unsupported {
            return Err(anyhow!(
                "01 (Nord) WebSocket supports only trades and orderbook deltas"
            ));
        }
        if !wants_trade && !wants_orderbook {
            return Err(anyhow!("01 stream config contains no supported data types"));
        }

        let symbols = dedup_symbols(config.symbols)?;

        // Resolve api symbol for each requested global symbol, and seed each symbol's
        // local book from a fresh (uncached) REST snapshot when orderbook is wanted.
        let mut api_symbol_to_global: HashMap<String, String> = HashMap::new();
        let mut books: HashMap<String, DepthBook> = HashMap::new();
        for symbol in &symbols {
            let api_symbol = self.rest.parse_symbol(symbol);
            let global_symbol = self.rest.normalize_symbol(&api_symbol);
            api_symbol_to_global.insert(api_symbol.clone(), global_symbol);

            if wants_orderbook {
                let market_id = self
                    .rest
                    .resolve_market_id(symbol)
                    .await
                    .with_context(|| format!("failed to resolve 01 market id for {symbol}"))?;
                let snapshot: NordOrderbookInfo = self
                    .rest
                    .get_request(&format!("/market/{}/orderbook", market_id))
                    .await
                    .with_context(|| {
                        format!("failed to fetch 01 orderbook snapshot for {symbol}")
                    })?;
                let mut book = DepthBook::default();
                book.seed_from_snapshot(&snapshot);
                books.insert(api_symbol, book);
            }
        }

        let mut channels = Vec::new();
        for api_symbol in api_symbol_to_global.keys() {
            if wants_trade {
                channels.push(format!("trades@{api_symbol}"));
            }
            if wants_orderbook {
                channels.push(format!("deltas@{api_symbol}"));
            }
        }
        if channels.len() > MAX_STREAMS_PER_CONNECTION {
            return Err(anyhow!(
                "01 WebSocket supports at most {} streams per connection, requested {}",
                MAX_STREAMS_PER_CONNECTION,
                channels.len()
            ));
        }

        let url = format!("{}/{}", self.ws_base_url, channels.join("&"));
        tracing::info!("Connecting to 01 WebSocket: {}", url);
        let (mut ws_stream, _) = connect_async(url.as_str())
            .await
            .context("failed to connect to 01 websocket")?;
        tracing::info!("Connected to 01 WebSocket");

        let stream = async_stream::stream! {
            while let Some(message) = ws_stream.next().await {
                match message {
                    Ok(Message::Text(text)) => {
                        let value: serde_json::Value = match serde_json::from_str(&text) {
                            Ok(v) => v,
                            Err(error) => {
                                tracing::debug!("Skipping malformed 01 websocket message: {}", error);
                                continue;
                            }
                        };

                        if let Some(delta_value) = value.get("delta") {
                            let delta: NordWsDelta = match serde_json::from_value(delta_value.clone()) {
                                Ok(d) => d,
                                Err(error) => {
                                    yield Err(anyhow!("failed to parse 01 delta payload: {}", error));
                                    continue;
                                }
                            };
                            let Some(global_symbol) = api_symbol_to_global.get(&delta.market_symbol) else {
                                continue;
                            };
                            let book = books.entry(delta.market_symbol.clone()).or_default();
                            book.apply_delta(&delta);
                            if wants_orderbook {
                                yield Ok(StreamEvent::Orderbook(book.to_orderbook(global_symbol)));
                            }
                        } else if let Some(trades_value) = value.get("trades") {
                            let batch: NordWsTradesBatch = match serde_json::from_value(trades_value.clone()) {
                                Ok(b) => b,
                                Err(error) => {
                                    yield Err(anyhow!("failed to parse 01 trades payload: {}", error));
                                    continue;
                                }
                            };
                            let Some(global_symbol) = api_symbol_to_global.get(&batch.market_symbol) else {
                                continue;
                            };
                            for trade in &batch.trades {
                                match ws_trade_to_trade(trade, global_symbol) {
                                    Ok(t) => yield Ok(StreamEvent::Trade(t)),
                                    Err(error) => yield Err(error),
                                }
                            }
                        } else {
                            tracing::trace!("Ignoring unrecognized 01 websocket message: {}", text);
                        }
                    }
                    Ok(Message::Ping(payload)) => {
                        if let Err(error) = ws_stream.send(Message::Pong(payload)).await {
                            yield Err(anyhow!("failed to send 01 websocket pong: {}", error));
                            break;
                        }
                    }
                    Ok(Message::Pong(_)) => tracing::trace!("Received 01 websocket transport pong"),
                    Ok(Message::Close(frame)) => {
                        tracing::info!("01 websocket connection closed: {:?}", frame);
                        break;
                    }
                    Ok(_) => tracing::trace!("Ignoring non-text 01 websocket message"),
                    Err(error) => {
                        yield Err(anyhow!("01 websocket error: {}", error));
                        break;
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(update_id: u64, bids: Vec<(f64, f64)>, asks: Vec<(f64, f64)>) -> NordOrderbookInfo {
        NordOrderbookInfo {
            update_id,
            bids,
            asks,
            bids_summary: super::super::types::NordOrderbookSummary { sum: 0.0, count: 0 },
            asks_summary: super::super::types::NordOrderbookSummary { sum: 0.0, count: 0 },
        }
    }

    #[test]
    fn seed_from_snapshot_then_apply_delta_replaces_absolute_and_removes_zero() {
        let mut book = DepthBook::default();
        book.seed_from_snapshot(&snapshot(
            100,
            vec![(100.0, 1.0), (99.0, 2.0)],
            vec![(101.0, 1.5)],
        ));
        assert_eq!(book.bids.get(&Decimal::from(100)), Some(&Decimal::ONE));

        book.apply_delta(&NordWsDelta {
            last_update_id: 100,
            update_id: 101,
            market_symbol: "BTCUSD".to_string(),
            bids: vec![(100.0, 0.5), (99.0, 0.0)],
            asks: vec![],
        });

        assert_eq!(
            book.bids.get(&Decimal::from(100)),
            Some(&Decimal::from_str_exact("0.5").unwrap())
        );
        assert_eq!(book.bids.get(&Decimal::from(99)), None);
        assert_eq!(book.last_update_id, Some(101));
    }

    #[test]
    fn first_delta_after_seeding_marks_synced_even_when_it_doesnt_chain() {
        // The REST snapshot is fetched before the WS connection opens, so the first delta's
        // last_update_id routinely doesn't match the snapshot's update_id - expected, not a
        // real gap (see `DepthBook::synced` doc comment). This must still flip `synced` to
        // true so a *second* mismatch (a genuine gap) is the one that gets flagged.
        let mut book = DepthBook::default();
        book.seed_from_snapshot(&snapshot(100, vec![(100.0, 1.0)], vec![]));
        assert!(!book.synced);

        book.apply_delta(&NordWsDelta {
            last_update_id: 999, // does not match snapshot's update_id (100) - expected
            update_id: 1000,
            market_symbol: "BTCUSD".to_string(),
            bids: vec![],
            asks: vec![],
        });
        assert!(book.synced);
        assert_eq!(book.last_update_id, Some(1000));
    }

    #[test]
    fn to_orderbook_sorts_bids_descending_and_asks_ascending() {
        let mut book = DepthBook::default();
        book.seed_from_snapshot(&snapshot(
            1,
            vec![(100.0, 1.0), (101.0, 1.0)],
            vec![(103.0, 1.0), (102.0, 1.0)],
        ));
        let ob = book.to_orderbook("BTC");
        assert_eq!(ob.bids[0].price, Decimal::from(101));
        assert_eq!(ob.asks[0].price, Decimal::from(102));
        assert!(ob.bids[0].price > ob.bids[1].price);
        assert!(ob.asks[0].price < ob.asks[1].price);
    }

    #[test]
    fn ws_trade_to_trade_maps_bid_ask_sides() {
        let trade = NordWsTrade {
            trade_id: 1,
            side: "bid".to_string(),
            price: 100.0,
            size: 0.5,
            physical_time: "2026-09-13T06:44:44.117406033Z".to_string(),
        };
        let t = ws_trade_to_trade(&trade, "BTC").unwrap();
        assert_eq!(t.side, OrderSide::Buy);

        let mut ask = trade;
        ask.side = "ask".to_string();
        let t = ws_trade_to_trade(&ask, "BTC").unwrap();
        assert_eq!(t.side, OrderSide::Sell);
    }

    #[test]
    fn get_name_is_01() {
        assert_eq!(O1WsClient::new().get_name(), "01");
    }
}
