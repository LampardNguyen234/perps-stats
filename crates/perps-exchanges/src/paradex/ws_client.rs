use super::ws_types::*;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use futures::{SinkExt, StreamExt};
use perps_core::streaming::*;
use perps_core::types::*;
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Mutex, Notify, RwLock};
use tokio::time;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

const WS_BASE_URL: &str = "wss://ws.api.prod.paradex.trade/v1";

// Max depth Paradex allows for snapshot channel (only valid value is 15).
const ORDERBOOK_DEPTH: u32 = 15;
// Snapshot update frequency.
const ORDERBOOK_FREQ_MS: u32 = 100;

// OrderbookManager tuning
const SNAPSHOT_TTL_SECS: u64 = 30;
const FIRST_DATA_TIMEOUT_SECS: u64 = 15;
const RECONNECT_DELAY_SECS: u64 = 2;
const INACTIVITY_TIMEOUT_SECS: u64 = 60;
const WATCHDOG_TICK_SECS: u64 = 15;

// ─── Shared helpers ───────────────────────────────────────────────────────────

fn orderbook_channel(market: &str) -> String {
    format!(
        "order_book.{}.snapshot@{}@{}ms",
        market, ORDERBOOK_DEPTH, ORDERBOOK_FREQ_MS
    )
}

fn clip_orderbook(ob: &MultiResolutionOrderbook, depth: usize) -> MultiResolutionOrderbook {
    if depth == 0 {
        return ob.clone();
    }
    let books = ob
        .orderbooks
        .iter()
        .map(|book| Orderbook {
            symbol: book.symbol.clone(),
            bids: book.bids.iter().take(depth).cloned().collect(),
            asks: book.asks.iter().take(depth).cloned().collect(),
            timestamp: book.timestamp,
        })
        .collect();
    MultiResolutionOrderbook {
        symbol: ob.symbol.clone(),
        timestamp: ob.timestamp,
        orderbooks: books,
    }
}

/// Split a full snapshot into bids/asks by level.side ("BUY"/"SELL").
fn parse_snapshot(snap: &ParadexOrderbookSnapshot) -> Result<Orderbook> {
    let mut bids = Vec::new();
    let mut asks = Vec::new();
    for level in &snap.inserts {
        let entry = OrderbookLevel {
            price: Decimal::from_str(&level.price)?,
            quantity: Decimal::from_str(&level.size)?,
        };
        if level.side == "BUY" {
            bids.push(entry);
        } else {
            asks.push(entry);
        }
    }
    // Sort: bids descending (highest price first), asks ascending (lowest price first).
    bids.sort_by(|a, b| b.price.cmp(&a.price));
    asks.sort_by(|a, b| a.price.cmp(&b.price));
    Ok(Orderbook {
        symbol: snap.market.clone(),
        bids,
        asks,
        timestamp: Utc.timestamp_millis_opt(snap.last_updated_at as i64).unwrap(),
    })
}

// ─── ParadexWsClient (IPerpsStream) ──────────────────────────────────────────

/// Streaming client for the `stream` command. Implements IPerpsStream.
#[derive(Clone)]
pub struct ParadexWsClient {
    base_url: String,
    id_counter: Arc<AtomicU32>,
}

impl ParadexWsClient {
    pub fn new() -> Self {
        Self {
            base_url: WS_BASE_URL.to_string(),
            id_counter: Arc::new(AtomicU32::new(1)),
        }
    }

    fn next_id(&self) -> u32 {
        self.id_counter.fetch_add(1, Ordering::Relaxed)
    }

    async fn connect(&self) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>> {
        tracing::info!("Connecting to Paradex WebSocket: {}", self.base_url);
        let (ws_stream, response) = connect_async(&self.base_url).await?;
        tracing::info!(
            "Connected to Paradex WebSocket (status: {:?})",
            response.status()
        );
        Ok(ws_stream)
    }

    async fn subscribe(
        &self,
        ws_stream: &mut WebSocketStream<MaybeTlsStream<TcpStream>>,
        channel: String,
    ) -> Result<()> {
        let req = ParadexWsSubscribeRequest::new(self.next_id(), channel.clone());
        let json = serde_json::to_string(&req)?;
        tracing::debug!("Paradex subscribing: {}", json);
        ws_stream.send(Message::Text(json)).await?;
        Ok(())
    }

    fn convert_ticker(&self, item: &ParadexMarketSummaryItem) -> Result<Ticker> {
        let last_price = Decimal::from_str(&item.last_traded_price).unwrap_or(Decimal::ZERO);
        let mark_price = Decimal::from_str(&item.mark_price)?;
        let index_price = Decimal::from_str(&item.underlying_price)?;
        let price_change_rate =
            Decimal::from_str(&item.price_change_rate_24h).unwrap_or(Decimal::ZERO);
        let price_change_24h = if price_change_rate != Decimal::ZERO {
            last_price * price_change_rate / (Decimal::ONE + price_change_rate)
        } else {
            Decimal::ZERO
        };
        let price_change_abs = price_change_24h.abs();

        let parse_opt = |s: &Option<String>| -> Decimal {
            s.as_deref()
                .filter(|v| !v.is_empty())
                .and_then(|v| Decimal::from_str(v).ok())
                .unwrap_or(Decimal::ZERO)
        };

        Ok(Ticker {
            symbol: item.symbol.clone(),
            last_price,
            mark_price,
            index_price,
            best_bid_price: parse_opt(&item.bid),
            best_bid_qty: parse_opt(&item.bid_size),
            best_ask_price: parse_opt(&item.ask),
            best_ask_qty: parse_opt(&item.ask_size),
            volume_24h: Decimal::ZERO,
            turnover_24h: Decimal::from_str(&item.volume_24h).unwrap_or(Decimal::ZERO),
            open_interest: Decimal::ZERO,
            open_interest_notional: Decimal::ZERO,
            price_change_24h,
            price_change_pct: price_change_rate,
            high_price_24h: last_price + price_change_abs,
            low_price_24h: if last_price > price_change_abs {
                last_price - price_change_abs
            } else {
                Decimal::ZERO
            },
            timestamp: Utc.timestamp_millis_opt(item.created_at as i64).unwrap(),
        })
    }

    fn convert_trade(&self, trade: &ParadexTradeItem, market: &str) -> Result<Trade> {
        Ok(Trade {
            id: trade.timestamp.to_string(),
            symbol: trade.market.clone().unwrap_or_else(|| market.to_string()),
            price: Decimal::from_str(&trade.price)?,
            quantity: Decimal::from_str(&trade.size)?,
            side: if trade.side == "BUY" {
                OrderSide::Buy
            } else {
                OrderSide::Sell
            },
            timestamp: Utc.timestamp_opt(trade.timestamp, 0).unwrap(),
        })
    }

    fn convert_funding_rate(&self, data: &ParadexFundingDataItem) -> Result<FundingRate> {
        Ok(FundingRate {
            symbol: data.market.clone(),
            funding_rate: Decimal::from_str(&data.funding_rate)?,
            funding_time: Utc.timestamp_millis_opt(data.created_at as i64).unwrap(),
            predicted_rate: Decimal::ZERO,
            next_funding_time: Utc.timestamp_millis_opt(0).unwrap(),
            funding_interval: 0,
            funding_rate_cap_floor: Decimal::ZERO,
        })
    }

    async fn handle_text_ping(
        &self,
        text: &str,
        ws_stream: &mut WebSocketStream<MaybeTlsStream<TcpStream>>,
    ) -> Result<bool> {
        if text.contains("\"type\":\"ping\"") {
            let pong = ParadexWsPong {
                msg_type: "pong".to_string(),
            };
            ws_stream
                .send(Message::Text(serde_json::to_string(&pong)?))
                .await?;
            return Ok(true);
        }
        Ok(false)
    }
}

impl Default for ParadexWsClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IPerpsStream for ParadexWsClient {
    fn get_name(&self) -> &str {
        "paradex"
    }

    async fn stream_tickers(&self, symbols: Vec<String>) -> Result<DataStream<Ticker>> {
        let mut ws_stream = self.connect().await?;
        for symbol in &symbols {
            self.subscribe(&mut ws_stream, format!("markets_summary.{}", symbol))
                .await?;
        }

        let client = self.clone();
        let stream = async_stream::stream! {
            while let Some(msg) = ws_stream.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        if client.handle_text_ping(&text, &mut ws_stream).await.unwrap_or(false) {
                            continue;
                        }
                        if let Ok(envelope) = serde_json::from_str::<ParadexWsMessage>(&text) {
                            if envelope.method.as_deref() == Some("subscription") {
                                if let Some(params) = envelope.params {
                                    if params.channel.starts_with("markets_summary") {
                                        match serde_json::from_value::<ParadexMarketSummaryItem>(params.data) {
                                            Ok(item) => match client.convert_ticker(&item) {
                                                Ok(ticker) => yield Ok(ticker),
                                                Err(e) => tracing::warn!("Paradex: ticker convert error: {}", e),
                                            },
                                            Err(e) => tracing::warn!("Paradex: markets_summary parse error: {}", e),
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Ok(Message::Close(frame)) => {
                        tracing::info!("Paradex WebSocket closed: {:?}", frame);
                        break;
                    }
                    Err(e) => {
                        yield Err(anyhow!("Paradex WebSocket error: {}", e));
                        break;
                    }
                    _ => {}
                }
            }
        };
        Ok(Box::pin(stream))
    }

    async fn stream_trades(&self, symbols: Vec<String>) -> Result<DataStream<Trade>> {
        let mut ws_stream = self.connect().await?;
        for symbol in &symbols {
            self.subscribe(&mut ws_stream, format!("trades.{}", symbol))
                .await?;
        }

        let client = self.clone();
        let stream = async_stream::stream! {
            while let Some(msg) = ws_stream.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        if client.handle_text_ping(&text, &mut ws_stream).await.unwrap_or(false) {
                            continue;
                        }
                        if let Ok(envelope) = serde_json::from_str::<ParadexWsMessage>(&text) {
                            if envelope.method.as_deref() == Some("subscription") {
                                if let Some(params) = envelope.params {
                                    if params.channel.starts_with("trades") {
                                        let market = params.channel
                                            .strip_prefix("trades.")
                                            .unwrap_or("")
                                            .to_string();
                                        match serde_json::from_value::<Vec<ParadexTradeItem>>(params.data.clone()) {
                                            Ok(trades) => {
                                                for t in &trades {
                                                    match client.convert_trade(t, &market) {
                                                        Ok(trade) => yield Ok(trade),
                                                        Err(e) => tracing::warn!("Paradex: trade convert error: {}", e),
                                                    }
                                                }
                                            }
                                            Err(_) => {
                                                if let Ok(t) = serde_json::from_value::<ParadexTradeItem>(params.data) {
                                                    match client.convert_trade(&t, &market) {
                                                        Ok(trade) => yield Ok(trade),
                                                        Err(e) => tracing::warn!("Paradex: trade convert error: {}", e),
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Ok(Message::Close(frame)) => {
                        tracing::info!("Paradex WebSocket closed: {:?}", frame);
                        break;
                    }
                    Err(e) => {
                        yield Err(anyhow!("Paradex WebSocket error: {}", e));
                        break;
                    }
                    _ => {}
                }
            }
        };
        Ok(Box::pin(stream))
    }

    async fn stream_orderbooks(&self, symbols: Vec<String>) -> Result<DataStream<Orderbook>> {
        let mut ws_stream = self.connect().await?;
        for symbol in &symbols {
            self.subscribe(&mut ws_stream, orderbook_channel(symbol))
                .await?;
        }

        let client = self.clone();
        let stream = async_stream::stream! {
            while let Some(msg) = ws_stream.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        if client.handle_text_ping(&text, &mut ws_stream).await.unwrap_or(false) {
                            continue;
                        }
                        if let Ok(envelope) = serde_json::from_str::<ParadexWsMessage>(&text) {
                            if envelope.method.as_deref() == Some("subscription") {
                                if let Some(params) = envelope.params {
                                    if params.channel.starts_with("order_book") {
                                        match serde_json::from_value::<ParadexOrderbookSnapshot>(params.data) {
                                            Ok(snap) => match parse_snapshot(&snap) {
                                                Ok(ob) => yield Ok(ob),
                                                Err(e) => tracing::warn!("Paradex: orderbook convert error: {}", e),
                                            },
                                            Err(e) => tracing::warn!("Paradex: orderbook parse error: {}", e),
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Ok(Message::Close(frame)) => {
                        tracing::info!("Paradex WebSocket closed: {:?}", frame);
                        break;
                    }
                    Err(e) => {
                        yield Err(anyhow!("Paradex WebSocket error: {}", e));
                        break;
                    }
                    _ => {}
                }
            }
        };
        Ok(Box::pin(stream))
    }

    async fn stream_multi(&self, config: StreamConfig) -> Result<DataStream<StreamEvent>> {
        let mut ws_stream = self.connect().await?;

        for symbol in &config.symbols {
            for data_type in &config.data_types {
                let channel = match data_type {
                    StreamDataType::Ticker => format!("markets_summary.{}", symbol),
                    StreamDataType::Trade => format!("trades.{}", symbol),
                    StreamDataType::Orderbook => orderbook_channel(symbol),
                    StreamDataType::FundingRate => format!("funding_data.{}", symbol),
                    StreamDataType::Kline => continue,
                };
                self.subscribe(&mut ws_stream, channel).await?;
            }
        }

        let client = self.clone();
        let stream = async_stream::stream! {
            while let Some(msg) = ws_stream.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        if client.handle_text_ping(&text, &mut ws_stream).await.unwrap_or(false) {
                            continue;
                        }
                        let envelope = match serde_json::from_str::<ParadexWsMessage>(&text) {
                            Ok(e) => e,
                            Err(e) => {
                                tracing::warn!("Paradex: failed to parse message: {} raw={}", e, &text[..text.len().min(200)]);
                                continue;
                            }
                        };

                        if envelope.method.as_deref() != Some("subscription") {
                            continue;
                        }
                        let params = match envelope.params {
                            Some(p) => p,
                            None => continue,
                        };
                        let channel = &params.channel;

                        if channel.starts_with("markets_summary") {
                            match serde_json::from_value::<ParadexMarketSummaryItem>(params.data) {
                                Ok(item) => match client.convert_ticker(&item) {
                                    Ok(ticker) => yield Ok(StreamEvent::Ticker(ticker)),
                                    Err(e) => tracing::warn!("Paradex: ticker convert error: {}", e),
                                },
                                Err(e) => tracing::warn!("Paradex: markets_summary parse error: {}", e),
                            }
                        } else if channel.starts_with("order_book") {
                            match serde_json::from_value::<ParadexOrderbookSnapshot>(params.data) {
                                Ok(snap) => match parse_snapshot(&snap) {
                                    Ok(ob) => yield Ok(StreamEvent::Orderbook(ob)),
                                    Err(e) => tracing::warn!("Paradex: orderbook convert error: {}", e),
                                },
                                Err(e) => tracing::warn!("Paradex: orderbook parse error: {}", e),
                            }
                        } else if channel.starts_with("trades") {
                            let market = channel.strip_prefix("trades.").unwrap_or("").to_string();
                            match serde_json::from_value::<Vec<ParadexTradeItem>>(params.data.clone()) {
                                Ok(trades) => {
                                    for t in &trades {
                                        match client.convert_trade(t, &market) {
                                            Ok(trade) => yield Ok(StreamEvent::Trade(trade)),
                                            Err(e) => tracing::warn!("Paradex: trade convert error: {}", e),
                                        }
                                    }
                                }
                                Err(_) => {
                                    if let Ok(t) = serde_json::from_value::<ParadexTradeItem>(params.data) {
                                        match client.convert_trade(&t, &market) {
                                            Ok(trade) => yield Ok(StreamEvent::Trade(trade)),
                                            Err(e) => tracing::warn!("Paradex: trade convert error: {}", e),
                                        }
                                    }
                                }
                            }
                        } else if channel.starts_with("funding_data") {
                            match serde_json::from_value::<ParadexFundingDataItem>(params.data) {
                                Ok(item) => match client.convert_funding_rate(&item) {
                                    Ok(rate) => yield Ok(StreamEvent::FundingRate(rate)),
                                    Err(e) => tracing::warn!("Paradex: funding rate convert error: {}", e),
                                },
                                Err(e) => tracing::warn!("Paradex: funding_data parse error: {}", e),
                            }
                        }
                    }
                    Ok(Message::Close(frame)) => {
                        tracing::info!("Paradex WebSocket closed: {:?}", frame);
                        break;
                    }
                    Err(e) => {
                        yield Err(anyhow!("Paradex WebSocket error: {}", e));
                        break;
                    }
                    _ => {}
                }
            }
        };
        Ok(Box::pin(stream))
    }
}

// ─── ParadexOrderbookManager (batch/start path) ───────────────────────────────

struct SnapshotEntry {
    orderbook: MultiResolutionOrderbook,
    updated_at: Instant,
}

/// Persistent WS connection maintaining live per-symbol orderbook snapshots for the
/// `start` batch collection path and the `liquidity` command.
///
/// Paradex sends full L2 snapshots every 100ms — no delta application needed.
/// Enabled when `DATABASE_URL` is set and `ENABLE_ORDERBOOK_STREAMING=true`.
pub struct ParadexOrderbookManager {
    snapshots: Arc<RwLock<HashMap<String, SnapshotEntry>>>,
    notifiers: Arc<Mutex<HashMap<String, Arc<Notify>>>>,
    /// Paradex-format market names currently subscribed ("BTC-USD-PERP", …).
    subscribed: Arc<Mutex<HashSet<String>>>,
    subscribe_tx: mpsc::Sender<Vec<String>>,
}

impl ParadexOrderbookManager {
    pub fn new() -> Self {
        let snapshots = Arc::new(RwLock::new(HashMap::new()));
        let notifiers = Arc::new(Mutex::new(HashMap::new()));
        let subscribed = Arc::new(Mutex::new(HashSet::new()));
        let (subscribe_tx, subscribe_rx) = mpsc::channel::<Vec<String>>(64);

        let snapshots_bg = Arc::clone(&snapshots);
        let notifiers_bg = Arc::clone(&notifiers);
        let subscribed_bg = Arc::clone(&subscribed);

        tokio::spawn(async move {
            run_manager_task(snapshots_bg, notifiers_bg, subscribed_bg, subscribe_rx).await;
        });

        Self { snapshots, notifiers, subscribed, subscribe_tx }
    }

    /// Queue Paradex-format market names (e.g. "BTC-USD-PERP") for subscription.
    pub async fn subscribe_symbols(&self, symbols: Vec<String>) {
        if symbols.is_empty() {
            return;
        }
        if let Err(e) = self.subscribe_tx.send(symbols).await {
            tracing::warn!("ParadexOrderbookManager: subscribe send failed: {}", e);
        }
    }

    /// Return a fresh orderbook snapshot, blocking up to FIRST_DATA_TIMEOUT_SECS on first call.
    pub async fn get_orderbook(
        &self,
        symbol: &str,
        depth: usize,
    ) -> Result<MultiResolutionOrderbook> {
        let notifier = {
            let mut n = self.notifiers.lock().await;
            Arc::clone(
                n.entry(symbol.to_string())
                    .or_insert_with(|| Arc::new(Notify::new())),
            )
        };

        let already_subscribed = self.subscribed.lock().await.contains(symbol);
        if !already_subscribed {
            self.subscribe_tx
                .send(vec![symbol.to_string()])
                .await
                .map_err(|e| anyhow!("subscribe_tx: {}", e))?;
        }

        let notified_fut = notifier.notified();

        {
            let snaps = self.snapshots.read().await;
            if let Some(entry) = snaps.get(symbol) {
                if entry.updated_at.elapsed() < Duration::from_secs(SNAPSHOT_TTL_SECS) {
                    return Ok(clip_orderbook(&entry.orderbook, depth));
                }
            }
        }

        time::timeout(Duration::from_secs(FIRST_DATA_TIMEOUT_SECS), notified_fut)
            .await
            .map_err(|_| {
                anyhow!(
                    "timeout waiting for Paradex orderbook snapshot for '{}'",
                    symbol
                )
            })?;

        let snaps = self.snapshots.read().await;
        let entry = snaps
            .get(symbol)
            .ok_or_else(|| anyhow!("no Paradex orderbook snapshot for '{}'", symbol))?;
        Ok(clip_orderbook(&entry.orderbook, depth))
    }
}

async fn run_manager_task(
    snapshots: Arc<RwLock<HashMap<String, SnapshotEntry>>>,
    notifiers: Arc<Mutex<HashMap<String, Arc<Notify>>>>,
    subscribed: Arc<Mutex<HashSet<String>>>,
    mut subscribe_rx: mpsc::Receiver<Vec<String>>,
) {
    let mut id_counter: u32 = 1;

    loop {
        tracing::info!("ParadexOrderbookManager: connecting to {}", WS_BASE_URL);

        let (ws_stream, _) = match connect_async(WS_BASE_URL).await {
            Ok(pair) => pair,
            Err(e) => {
                tracing::error!("ParadexOrderbookManager: connect error: {}", e);
                time::sleep(Duration::from_secs(RECONNECT_DELAY_SECS)).await;
                continue;
            }
        };

        let (mut ws_sink, mut ws_source) = ws_stream.split();

        // Clear stale snapshots so callers block for fresh data.
        snapshots.write().await.clear();

        // Re-subscribe all active symbols.
        {
            let syms: Vec<String> = subscribed.lock().await.iter().cloned().collect();
            for sym in &syms {
                let req = ParadexWsSubscribeRequest::new(id_counter, orderbook_channel(sym));
                id_counter = id_counter.wrapping_add(1);
                if let Ok(json) = serde_json::to_string(&req) {
                    let _ = ws_sink.send(Message::Text(json)).await;
                }
            }
            if !syms.is_empty() {
                tracing::info!(
                    "ParadexOrderbookManager: re-subscribed {} markets",
                    syms.len()
                );
            }
        }

        let mut last_message_at = Instant::now();
        let mut watchdog = time::interval(Duration::from_secs(WATCHDOG_TICK_SECS));

        'connection: loop {
            tokio::select! {
                msg_opt = ws_source.next() => {
                    match msg_opt {
                        Some(Ok(Message::Text(text))) => {
                            last_message_at = Instant::now();
                            handle_manager_message(
                                &text,
                                &snapshots,
                                &notifiers,
                            ).await;
                        }
                        Some(Ok(Message::Ping(payload))) => {
                            last_message_at = Instant::now();
                            let _ = ws_sink.send(Message::Pong(payload)).await;
                        }
                        Some(Ok(Message::Pong(_))) => {
                            last_message_at = Instant::now();
                        }
                        Some(Ok(Message::Close(_))) => {
                            tracing::info!("ParadexOrderbookManager: server closed connection");
                            break 'connection;
                        }
                        Some(Ok(_)) => {}
                        Some(Err(e)) => {
                            tracing::warn!("ParadexOrderbookManager: ws error: {}", e);
                            break 'connection;
                        }
                        None => {
                            tracing::info!("ParadexOrderbookManager: stream ended");
                            break 'connection;
                        }
                    }
                }

                req = subscribe_rx.recv() => {
                    match req {
                        Some(new_syms) => {
                            let mut all_new = new_syms;
                            while let Ok(more) = subscribe_rx.try_recv() {
                                all_new.extend(more);
                            }

                            let truly_new: Vec<String> = {
                                let mut sub = subscribed.lock().await;
                                all_new.into_iter().filter(|s| sub.insert(s.clone())).collect()
                            };

                            if truly_new.is_empty() {
                                continue;
                            }

                            for sym in &truly_new {
                                let req = ParadexWsSubscribeRequest::new(id_counter, orderbook_channel(sym));
                                id_counter = id_counter.wrapping_add(1);
                                if let Ok(json) = serde_json::to_string(&req) {
                                    if let Err(e) = ws_sink.send(Message::Text(json)).await {
                                        tracing::error!(
                                            "ParadexOrderbookManager: subscribe send error: {}",
                                            e
                                        );
                                        break 'connection;
                                    }
                                }
                            }
                            tracing::debug!(
                                "ParadexOrderbookManager: subscribed {:?}",
                                truly_new
                            );
                        }
                        None => {
                            tracing::debug!("ParadexOrderbookManager: subscribe_rx closed, shutting down");
                            return;
                        }
                    }
                }

                _ = watchdog.tick() => {
                    if last_message_at.elapsed() > Duration::from_secs(INACTIVITY_TIMEOUT_SECS) {
                        tracing::warn!(
                            "ParadexOrderbookManager: no message for {}s, reconnecting",
                            INACTIVITY_TIMEOUT_SECS
                        );
                        break 'connection;
                    }
                }
            }
        }

        tracing::info!(
            "ParadexOrderbookManager: reconnecting in {}s",
            RECONNECT_DELAY_SECS
        );
        time::sleep(Duration::from_secs(RECONNECT_DELAY_SECS)).await;
    }
}

async fn handle_manager_message(
    text: &str,
    snapshots: &Arc<RwLock<HashMap<String, SnapshotEntry>>>,
    notifiers: &Arc<Mutex<HashMap<String, Arc<Notify>>>>,
) {
    let envelope: ParadexWsMessage = match serde_json::from_str(text) {
        Ok(e) => e,
        Err(_) => return,
    };

    if envelope.method.as_deref() != Some("subscription") {
        return;
    }
    let params = match envelope.params {
        Some(p) => p,
        None => return,
    };
    if !params.channel.starts_with("order_book") {
        return;
    }

    let snap: ParadexOrderbookSnapshot = match serde_json::from_value(params.data) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("ParadexOrderbookManager: orderbook parse error: {}", e);
            return;
        }
    };

    let ob = match parse_snapshot(&snap) {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("ParadexOrderbookManager: snapshot convert error: {}", e);
            return;
        }
    };

    let symbol = ob.symbol.clone();
    let best_bid = ob.bids.first().map(|l| l.price);
    let best_ask = ob.asks.first().map(|l| l.price);
    let bid_liq: Decimal = ob.bids.iter().map(|l| l.quantity * l.price).sum();
    let ask_liq: Decimal = ob.asks.iter().map(|l| l.quantity * l.price).sum();
    tracing::debug!(
        "ParadexOrderbookManager: {} bestBid={} bestAsk={} bidLiq={:.4} askLiq={:.4}",
        symbol,
        best_bid
            .map(|p| format!("{:.2}", p))
            .unwrap_or_else(|| "-".to_string()),
        best_ask
            .map(|p| format!("{:.2}", p))
            .unwrap_or_else(|| "-".to_string()),
        bid_liq,
        ask_liq,
    );

    let multi_ob = MultiResolutionOrderbook::from_single(ob);
    {
        let mut snaps = snapshots.write().await;
        snaps.insert(
            symbol.clone(),
            SnapshotEntry {
                orderbook: multi_ob,
                updated_at: Instant::now(),
            },
        );
    }

    if let Some(notifier) = notifiers.lock().await.get(&symbol) {
        notifier.notify_waiters();
    }
}
