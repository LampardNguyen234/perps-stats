use crate::qfex::conversions::{to_global_symbol, to_qfex_symbol};
use crate::qfex::ws_types::*;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::Utc;
use futures::{SinkExt, StreamExt};
use perps_core::streaming::*;
use perps_core::types::*;
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex, Notify, RwLock};
use tokio::time;
use tokio_tungstenite::{connect_async, tungstenite::Message};

const WS_BASE_URL: &str = "wss://mds.qfex.com/";
const SNAPSHOT_TTL_SECS: u64 = 5;
const FIRST_DATA_TIMEOUT_SECS: u64 = 10;
const RECONNECT_DELAY_SECS: u64 = 1;
const INACTIVITY_TIMEOUT_SECS: u64 = 30;
const WATCHDOG_TICK_SECS: u64 = 10;

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
        symbol: to_global_symbol(&msg.symbol),
        bids,
        asks,
        timestamp: Utc::now(),
    })
}

fn clip_orderbook(ob: &MultiResolutionOrderbook, depth: usize) -> MultiResolutionOrderbook {
    if depth == 0 {
        return ob.clone();
    }
    let clipped_books: Vec<Orderbook> = ob
        .orderbooks
        .iter()
        .map(|book| {
            let bids = book.bids.iter().take(depth).cloned().collect();
            let asks = book.asks.iter().take(depth).cloned().collect();
            Orderbook {
                symbol: book.symbol.clone(),
                bids,
                asks,
                timestamp: book.timestamp,
            }
        })
        .collect();

    MultiResolutionOrderbook {
        symbol: ob.symbol.clone(),
        timestamp: ob.timestamp,
        orderbooks: clipped_books,
    }
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

struct SnapshotEntry {
    orderbook: MultiResolutionOrderbook,
    updated_at: Instant,
}

/// Persistent WS connection that maintains per-symbol L2 orderbook snapshots.
pub struct OrderbookManager {
    /// Key: QFEX symbol (e.g. "NVDA-USD") → snapshot
    snapshots: Arc<RwLock<HashMap<String, SnapshotEntry>>>,
    /// Key: QFEX symbol → Notify for first-data signaling
    notifiers: Arc<Mutex<HashMap<String, Arc<Notify>>>>,
    /// Symbols currently subscribed (QFEX format)
    subscribed: Arc<Mutex<HashSet<String>>>,
    /// Channel to request new subscriptions
    subscribe_tx: mpsc::Sender<Vec<String>>,
    #[allow(dead_code)]
    sig_figs: Vec<u8>,
}

impl OrderbookManager {
    pub fn new(sig_figs: Vec<u8>) -> Self {
        let snapshots: Arc<RwLock<HashMap<String, SnapshotEntry>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let notifiers: Arc<Mutex<HashMap<String, Arc<Notify>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let subscribed: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
        let (subscribe_tx, subscribe_rx) = mpsc::channel::<Vec<String>>(64);

        let snapshots_bg = Arc::clone(&snapshots);
        let notifiers_bg = Arc::clone(&notifiers);
        let subscribed_bg = Arc::clone(&subscribed);
        let sig_figs_bg = sig_figs.clone();

        tokio::spawn(async move {
            run_background_task(
                snapshots_bg,
                notifiers_bg,
                subscribed_bg,
                subscribe_rx,
                sig_figs_bg,
            )
            .await;
        });

        Self {
            snapshots,
            notifiers,
            subscribed,
            subscribe_tx,
            sig_figs,
        }
    }

    /// Get (or wait for) the current orderbook snapshot for a symbol.
    ///
    /// - If not yet subscribed, triggers a subscription.
    /// - Waits up to 10 s for the first snapshot.
    /// - Returns a clipped copy truncated to `depth` levels.
    pub async fn get_orderbook(
        &self,
        symbol: &str,
        depth: usize,
    ) -> Result<MultiResolutionOrderbook> {
        let qfex_sym = to_qfex_symbol(symbol);

        // Ensure we have a notifier for this symbol, and subscribe if needed
        let notifier = {
            let mut notifiers = self.notifiers.lock().await;
            let entry = notifiers
                .entry(qfex_sym.clone())
                .or_insert_with(|| Arc::new(Notify::new()));
            Arc::clone(entry)
        };

        let already_subscribed = {
            let sub = self.subscribed.lock().await;
            sub.contains(&qfex_sym)
        };

        if !already_subscribed {
            self.subscribe_tx
                .send(vec![qfex_sym.clone()])
                .await
                .map_err(|e| anyhow!("subscribe_tx send error: {}", e))?;
        }

        // Create the notified future *before* the snapshot check to avoid a TOCTOU race:
        // if a notification fires between the check and the await, it would be lost otherwise.
        let notified_fut = notifier.notified();

        // Check if fresh snapshot is already available
        {
            let snaps = self.snapshots.read().await;
            if let Some(entry) = snaps.get(&qfex_sym) {
                if entry.updated_at.elapsed() < Duration::from_secs(SNAPSHOT_TTL_SECS) {
                    return Ok(clip_orderbook(&entry.orderbook, depth));
                }
            }
        }

        // Wait for first/fresh data
        time::timeout(Duration::from_secs(FIRST_DATA_TIMEOUT_SECS), notified_fut)
            .await
            .map_err(|_| anyhow!("Timeout waiting for orderbook data for {}", symbol))?;

        // Read snapshot after notification
        let snaps = self.snapshots.read().await;
        let entry = snaps
            .get(&qfex_sym)
            .ok_or_else(|| anyhow!("No snapshot available for {}", symbol))?;

        if entry.updated_at.elapsed() >= Duration::from_secs(SNAPSHOT_TTL_SECS) {
            return Err(anyhow!("Stale orderbook snapshot for {}", symbol));
        }

        Ok(clip_orderbook(&entry.orderbook, depth))
    }
}

// ─── Background task ──────────────────────────────────────────────────────────

async fn run_background_task(
    snapshots: Arc<RwLock<HashMap<String, SnapshotEntry>>>,
    notifiers: Arc<Mutex<HashMap<String, Arc<Notify>>>>,
    subscribed: Arc<Mutex<HashSet<String>>>,
    mut subscribe_rx: mpsc::Receiver<Vec<String>>,
    sig_figs: Vec<u8>,
) {
    loop {
        tracing::debug!("OrderbookManager: connecting to {}", WS_BASE_URL);

        let ws_result = connect_async(WS_BASE_URL).await;
        let (ws_stream, _) = match ws_result {
            Ok(pair) => pair,
            Err(e) => {
                tracing::error!("OrderbookManager: connect error: {}", e);
                time::sleep(Duration::from_secs(RECONNECT_DELAY_SECS)).await;
                continue;
            }
        };

        let (mut ws_sink, mut ws_source) = ws_stream.split();

        // On reconnect: clear all snapshots so consumers block until fresh data arrives.
        // The existing TTL path in get_orderbook handles the wait automatically.
        snapshots.write().await.clear();
        tracing::debug!("OrderbookManager: cleared snapshots on reconnect");

        // On reconnect: re-subscribe to all already-subscribed symbols in one message
        {
            let sub = subscribed.lock().await;
            if !sub.is_empty() {
                let symbols: Vec<String> = sub.iter().cloned().collect();
                match build_subscribe_msg(vec!["level2".to_string()], symbols, &sig_figs) {
                    Ok(msg) => {
                        if let Err(e) = ws_sink.send(Message::Text(msg.into())).await {
                            tracing::error!("OrderbookManager: re-subscribe send error: {}", e);
                        } else {
                            tracing::info!(
                                "OrderbookManager: re-subscribed to {} symbols",
                                sub.len()
                            );
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            "OrderbookManager: failed to build re-subscribe msg: {}",
                            e
                        );
                    }
                }
            }
        }

        let mut last_sequence: Option<u64> = None;
        let mut last_message_at = Instant::now();
        let mut watchdog = time::interval(Duration::from_secs(WATCHDOG_TICK_SECS));

        'connection: loop {
            tokio::select! {
                msg_opt = ws_source.next() => {
                    match msg_opt {
                        Some(Ok(Message::Text(text))) => {
                            let text_str: &str = &text;
                            // Detect message type via envelope
                            match serde_json::from_str::<WsEnvelope>(text_str) {
                                Ok(envelope) => {
                                    match envelope.r#type.as_deref().unwrap_or("") {
                                        "level2" => {
                                            match serde_json::from_str::<WsL2Message>(text_str) {
                                                Ok(l2_msg) => {
                                                    // Gap detection: use != to catch both backward
                                                    // duplicates and forward jumps (missed messages).
                                                    // Sequence is global per-connection across all
                                                    // symbols and sig_figs values.
                                                    if let Some(last_seq) = last_sequence {
                                                        if l2_msg.sequence != last_seq + 1 {
                                                            tracing::warn!(
                                                                "OrderbookManager: sequence gap detected: expected {}, got {}",
                                                                last_seq + 1,
                                                                l2_msg.sequence
                                                            );
                                                        }
                                                    }
                                                    last_sequence = Some(l2_msg.sequence);
                                                    last_message_at = Instant::now();

                                                    let qfex_sym = l2_msg.symbol.clone();

                                                    match ws_l2_to_orderbook(&l2_msg) {
                                                        Ok(ob) => {
                                                            {
                                                                let mut snaps = snapshots.write().await;
                                                                // Accumulate into a per-sig_figs slot rather than
                                                                // overwriting the entire entry. This preserves all
                                                                // resolution levels when multiple sig_figs are
                                                                // subscribed for the same symbol.
                                                                let entry = snaps
                                                                    .entry(qfex_sym.clone())
                                                                    .or_insert_with(|| SnapshotEntry {
                                                                        orderbook: MultiResolutionOrderbook::from_single(ob.clone()),
                                                                        updated_at: Instant::now(),
                                                                    });
                                                                let sig_idx = l2_msg.sig_figs as usize;
                                                                if sig_idx < entry.orderbook.orderbooks.len() {
                                                                    entry.orderbook.orderbooks[sig_idx] = ob;
                                                                } else {
                                                                    entry.orderbook.orderbooks.resize_with(sig_idx + 1, || ob.clone());
                                                                    entry.orderbook.orderbooks[sig_idx] = ob;
                                                                }
                                                                entry.orderbook.timestamp = Utc::now();
                                                                entry.updated_at = Instant::now();
                                                            }

                                                            // Notify waiters
                                                            let notifiers_guard = notifiers.lock().await;
                                                            if let Some(n) = notifiers_guard.get(&qfex_sym) {
                                                                n.notify_waiters();
                                                            }
                                                        }
                                                        Err(e) => {
                                                            tracing::warn!(
                                                                "OrderbookManager: l2 parse error for {}: {}",
                                                                qfex_sym, e
                                                            );
                                                        }
                                                    }
                                                }
                                                Err(e) => {
                                                    tracing::debug!("OrderbookManager: failed to parse level2 msg: {}", e);
                                                }
                                            }
                                        }
                                        "subscribed" => {
                                            if let Ok(sub_msg) = serde_json::from_str::<WsSubscribedMessage>(text_str) {
                                                tracing::debug!(
                                                    "OrderbookManager: subscribed to channels={:?} symbols={:?}",
                                                    sub_msg.channels,
                                                    sub_msg.symbols
                                                );
                                            }
                                        }
                                        other => {
                                            tracing::trace!("OrderbookManager: unhandled msg type={}", other);
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::trace!("OrderbookManager: envelope parse error: {}", e);
                                }
                            }
                        }
                        Some(Ok(Message::Ping(payload))) => {
                            last_message_at = Instant::now();
                            if let Err(e) = ws_sink.send(Message::Pong(payload)).await {
                                tracing::error!("OrderbookManager: pong send error: {}", e);
                                break 'connection;
                            }
                        }
                        Some(Ok(Message::Pong(_))) => {
                            last_message_at = Instant::now();
                        }
                        Some(Ok(Message::Close(_))) => {
                            tracing::info!("OrderbookManager: server closed connection");
                            break 'connection;
                        }
                        Some(Ok(_)) => {
                            // Binary, Pong, etc. — ignore
                        }
                        Some(Err(e)) => {
                            tracing::error!("OrderbookManager: ws error: {}", e);
                            break 'connection;
                        }
                        None => {
                            tracing::info!("OrderbookManager: ws stream ended");
                            break 'connection;
                        }
                    }
                }

                req = subscribe_rx.recv() => {
                    match req {
                        Some(new_symbols) => {
                            // Drain any additional pending requests
                            let mut all_new: Vec<String> = new_symbols;
                            while let Ok(more) = subscribe_rx.try_recv() {
                                all_new.extend(more);
                            }

                            // Deduplicate and add to subscribed set
                            {
                                let mut sub = subscribed.lock().await;
                                for s in &all_new {
                                    sub.insert(s.clone());
                                }
                            }

                            // Send one subscribe message for all new symbols
                            match build_subscribe_msg(
                                vec!["level2".to_string()],
                                all_new.clone(),
                                &sig_figs,
                            ) {
                                Ok(msg) => {
                                    if let Err(e) = ws_sink.send(Message::Text(msg.into())).await {
                                        tracing::error!(
                                            "OrderbookManager: subscribe send error: {}",
                                            e
                                        );
                                        break 'connection;
                                    } else {
                                        tracing::debug!(
                                            "OrderbookManager: subscribed to {:?}, sig_figs {:?}",
                                            all_new, sig_figs
                                        );
                                    }
                                }
                                Err(e) => {
                                    tracing::error!("OrderbookManager: build subscribe error: {}", e);
                                }
                            }
                        }
                        None => {
                            tracing::debug!("OrderbookManager: subscribe_rx closed, shutting down");
                            return;
                        }
                    }
                }

                _ = watchdog.tick() => {
                    if last_message_at.elapsed() > Duration::from_secs(INACTIVITY_TIMEOUT_SECS) {
                        tracing::warn!(
                            "OrderbookManager: no message received for {}s, forcing reconnect",
                            INACTIVITY_TIMEOUT_SECS
                        );
                        break 'connection;
                    }
                }
            }
        }

        tracing::info!(
            "OrderbookManager: reconnecting in {}s",
            RECONNECT_DELAY_SECS
        );
        time::sleep(Duration::from_secs(RECONNECT_DELAY_SECS)).await;
    }
}

// ─── QfexWsClient ─────────────────────────────────────────────────────────────

/// QFEX WebSocket streaming client.
///
/// Each streaming method opens its own dedicated WS connection.
/// The `OrderbookManager` field provides a shared, persistent connection for
/// `get_orderbook` calls made via the REST client.
#[derive(Clone)]
pub struct QfexWsClient {
    pub orderbook_manager: Arc<OrderbookManager>,
}

impl QfexWsClient {
    pub fn new() -> Self {
        Self {
            orderbook_manager: Arc::new(OrderbookManager::new(vec![0])),
        }
    }

    pub fn with_sig_figs(sig_figs: Vec<u8>) -> Self {
        Self {
            orderbook_manager: Arc::new(OrderbookManager::new(sig_figs)),
        }
    }
}

impl Default for QfexWsClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IPerpsStream for QfexWsClient {
    fn get_name(&self) -> &str {
        "qfex"
    }

    /// Stream BBO tickers. Opens a fresh WS connection per call.
    async fn stream_tickers(&self, _symbols: Vec<String>) -> Result<DataStream<Ticker>> {
        unimplemented!()
    }

    /// Stream raw orderbook updates. Opens a fresh WS connection per call.
    async fn stream_orderbooks(&self, _symbols: Vec<String>) -> Result<DataStream<Orderbook>> {
        unimplemented!()
    }

    /// Stream public trades. Opens a fresh WS connection per call.
    async fn stream_trades(&self, _symbols: Vec<String>) -> Result<DataStream<Trade>> {
        unimplemented!()
    }

    /// Subscribe to multiple data types. Opens a fresh WS connection per call.
    async fn stream_multi(&self, _config: StreamConfig) -> Result<DataStream<StreamEvent>> {
        unimplemented!()
    }
}
