use crate::cache::ContractCache;
use crate::risex::ws_types::*;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::Utc;
use futures::{SinkExt, StreamExt};
use perps_core::streaming::*;
use perps_core::types::*;
use rust_decimal::Decimal;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex, Notify, RwLock};
use tokio::time;
use tokio_tungstenite::{connect_async, tungstenite::Message};

const WS_URL: &str = "wss://ws.rise.trade/ws";
const SNAPSHOT_TTL_SECS: u64 = 5;
const FIRST_DATA_TIMEOUT_SECS: u64 = 10;
const RECONNECT_DELAY_SECS: u64 = 2;
const INACTIVITY_TIMEOUT_SECS: u64 = 30;
const WATCHDOG_TICK_SECS: u64 = 10;

fn parse_price(s: &str) -> Result<Decimal> {
    Decimal::from_str(s).map_err(|e| anyhow!("price parse '{}': {}", s, e))
}

/// Convert human-readable Decimal back to wei integer string (×10^18) for checksum.
/// Must use normalize() — rust_decimal preserves scale after multiplication, so
/// without it `64043.4 * 10^18` displays as "64043400000000000000000.0" not "64043400000000000000000",
/// which would break the CRC32 string match against the server.
fn decimal_to_wei_str(d: Decimal) -> String {
    let scale = Decimal::from(1_000_000_000_000_000_000u64); // 10^18
    (d * scale).trunc().normalize().to_string()
}

/// CRC32-IEEE over the book state after applying an update.
/// Format: interleave bid[i] then ask[i] by index (bids desc, asks asc),
/// each level as "wei_price:wei_qty:", then trim trailing ':'.
fn compute_checksum(
    bids: impl Iterator<Item = (Decimal, Decimal)>,
    asks: impl Iterator<Item = (Decimal, Decimal)>,
    bid_len: usize,
    ask_len: usize,
) -> u32 {
    let bids: Vec<_> = bids.collect();
    let asks: Vec<_> = asks.collect();
    let max_len = bid_len.max(ask_len);
    let mut buf = String::new();

    for i in 0..max_len {
        if i < bids.len() {
            buf.push_str(&decimal_to_wei_str(bids[i].0));
            buf.push(':');
            buf.push_str(&decimal_to_wei_str(bids[i].1));
            buf.push(':');
        }
        if i < asks.len() {
            buf.push_str(&decimal_to_wei_str(asks[i].0));
            buf.push(':');
            buf.push_str(&decimal_to_wei_str(asks[i].1));
            buf.push(':');
        }
    }

    if buf.ends_with(':') {
        buf.pop();
    }

    crc32fast::hash(buf.as_bytes())
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

// ─── Local orderbook state ────────────────────────────────────────────────────

/// Live L2 book for one market, maintained by applying incremental deltas.
struct MarketState {
    /// price → qty, BTreeMap for O(log n) mutation and free sorted iteration.
    bids: BTreeMap<Decimal, Decimal>,
    asks: BTreeMap<Decimal, Decimal>,
    symbol: String,
}

impl MarketState {
    fn new(symbol: String) -> Self {
        Self {
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            symbol,
        }
    }

    fn apply_level(book: &mut BTreeMap<Decimal, Decimal>, price: Decimal, qty: Decimal) {
        if qty.is_zero() {
            book.remove(&price);
        } else {
            book.insert(price, qty);
        }
    }

    fn reset_from_snapshot(&mut self, bid_levels: &[WsLevel], ask_levels: &[WsLevel]) -> Result<()> {
        self.bids.clear();
        self.asks.clear();
        for l in bid_levels {
            let p = parse_price(&l.price)?;
            let q = parse_price(&l.quantity)?;
            if !q.is_zero() {
                self.bids.insert(p, q);
            }
        }
        for l in ask_levels {
            let p = parse_price(&l.price)?;
            let q = parse_price(&l.quantity)?;
            if !q.is_zero() {
                self.asks.insert(p, q);
            }
        }
        Ok(())
    }

    fn apply_update(&mut self, bid_deltas: &[WsLevel], ask_deltas: &[WsLevel]) -> Result<()> {
        for l in bid_deltas {
            Self::apply_level(&mut self.bids, parse_price(&l.price)?, parse_price(&l.quantity)?);
        }
        for l in ask_deltas {
            Self::apply_level(&mut self.asks, parse_price(&l.price)?, parse_price(&l.quantity)?);
        }
        Ok(())
    }

    fn checksum(&self) -> u32 {
        // bids descending (best first), asks ascending (best first)
        compute_checksum(
            self.bids.iter().rev().map(|(p, q)| (*p, *q)),
            self.asks.iter().map(|(p, q)| (*p, *q)),
            self.bids.len(),
            self.asks.len(),
        )
    }

    fn to_orderbook(&self) -> Orderbook {
        // BTreeMap iterates ascending; bids must be descending (best = highest price first).
        let bids: Vec<OrderbookLevel> = self
            .bids
            .iter()
            .rev()
            .map(|(p, q)| OrderbookLevel { price: *p, quantity: *q })
            .collect();
        let asks: Vec<OrderbookLevel> = self
            .asks
            .iter()
            .map(|(p, q)| OrderbookLevel { price: *p, quantity: *q })
            .collect();

        if let (Some(bid), Some(ask)) = (bids.first(), asks.first()) {
            if bid.price >= ask.price {
                tracing::warn!(
                    "RISEx WS crossed orderbook for {}: bid {} >= ask {}",
                    self.symbol,
                    bid.price,
                    ask.price
                );
            }
            // Sanity-check: mid-price should be in a plausible range ($0.01–$1M).
            let mid = (bid.price + ask.price) / Decimal::TWO;
            if mid < Decimal::new(1, 2) || mid > Decimal::new(1_000_000, 0) {
                tracing::warn!(
                    "RISEx WS suspicious mid-price for {}: {} — possible wei conversion error",
                    self.symbol,
                    mid
                );
            }
        }

        Orderbook {
            symbol: self.symbol.clone(),
            bids,
            asks,
            timestamp: Utc::now(),
        }
    }
}

struct SnapshotEntry {
    orderbook: MultiResolutionOrderbook,
    updated_at: Instant,
}

// ─── RisexOrderbookManager ────────────────────────────────────────────────────

/// Persistent WS connection maintaining live per-symbol orderbook snapshots for RISEx.
///
/// The `market_id_cache` must be initialized (via any REST call on `RiseXClient`) before
/// `subscribe_symbols` can resolve global symbols to RISEx integer market_ids.
pub struct RisexOrderbookManager {
    snapshots: Arc<RwLock<HashMap<String, SnapshotEntry>>>,
    notifiers: Arc<Mutex<HashMap<String, Arc<Notify>>>>,
    /// Global symbols currently subscribed (shared with background task for reconnect).
    subscribed: Arc<Mutex<HashSet<String>>>,
    subscribe_tx: mpsc::Sender<Vec<String>>,
}

impl RisexOrderbookManager {
    pub fn new(market_id_cache: ContractCache<u64>) -> Self {
        let snapshots = Arc::new(RwLock::new(HashMap::new()));
        let notifiers = Arc::new(Mutex::new(HashMap::new()));
        let subscribed = Arc::new(Mutex::new(HashSet::new()));
        let (subscribe_tx, subscribe_rx) = mpsc::channel::<Vec<String>>(64);

        let snapshots_bg = Arc::clone(&snapshots);
        let notifiers_bg = Arc::clone(&notifiers);
        let subscribed_bg = Arc::clone(&subscribed);

        tokio::spawn(async move {
            run_background_task(
                market_id_cache,
                snapshots_bg,
                notifiers_bg,
                subscribed_bg,
                subscribe_rx,
            )
            .await;
        });

        Self { snapshots, notifiers, subscribed, subscribe_tx }
    }

    /// Fire-and-forget: queue global symbols for subscription.
    pub async fn subscribe_symbols(&self, symbols: Vec<String>) {
        if symbols.is_empty() {
            return;
        }
        if let Err(e) = self.subscribe_tx.send(symbols).await {
            tracing::warn!("RisexOrderbookManager: subscribe send failed: {}", e);
        }
    }

    /// Return a fresh orderbook snapshot, blocking up to 10 s on first call per symbol.
    pub async fn get_orderbook(&self, symbol: &str, depth: usize) -> Result<MultiResolutionOrderbook> {
        // Create notifier entry before subscribe so we never miss a notification.
        let notifier = {
            let mut n = self.notifiers.lock().await;
            Arc::clone(n.entry(symbol.to_string()).or_insert_with(|| Arc::new(Notify::new())))
        };

        let already_subscribed = self.subscribed.lock().await.contains(symbol);
        if !already_subscribed {
            self.subscribe_tx
                .send(vec![symbol.to_string()])
                .await
                .map_err(|e| anyhow!("subscribe_tx: {}", e))?;
        }

        // Arm the notified future before the snapshot check to avoid a TOCTOU race.
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
            .map_err(|_| anyhow!("timeout waiting for RISEx orderbook snapshot for '{}'", symbol))?;

        let snaps = self.snapshots.read().await;
        let entry = snaps
            .get(symbol)
            .ok_or_else(|| anyhow!("no RISEx orderbook snapshot for '{}'", symbol))?;
        Ok(clip_orderbook(&entry.orderbook, depth))
    }
}

// ─── Background task ──────────────────────────────────────────────────────────

/// Build the market_id → global_symbol reverse map from the ContractCache.
async fn build_id_to_symbol(cache: &ContractCache<u64>) -> HashMap<u64, String> {
    let mut map = HashMap::new();
    for sym in cache.get_all_symbols().await {
        if let Some(id) = cache.get(&sym).await {
            map.insert(id, sym);
        }
    }
    map
}

fn build_subscribe_msg(market_ids: &[u64]) -> Result<String> {
    let req = WsRequest {
        method: "subscribe".to_string(),
        params: WsRequestParams {
            channel: "orderbook".to_string(),
            market_ids: market_ids.to_vec(),
        },
    };
    Ok(serde_json::to_string(&req)?)
}

async fn run_background_task(
    market_id_cache: ContractCache<u64>,
    snapshots: Arc<RwLock<HashMap<String, SnapshotEntry>>>,
    notifiers: Arc<Mutex<HashMap<String, Arc<Notify>>>>,
    subscribed: Arc<Mutex<HashSet<String>>>,
    mut subscribe_rx: mpsc::Receiver<Vec<String>>,
) {
    loop {
        tracing::debug!("RisexOrderbookManager: connecting to {}", WS_URL);

        let (ws_stream, _) = match connect_async(WS_URL).await {
            Ok(pair) => pair,
            Err(e) => {
                tracing::error!("RisexOrderbookManager: connect error: {}", e);
                time::sleep(Duration::from_secs(RECONNECT_DELAY_SECS)).await;
                continue;
            }
        };

        let (mut ws_sink, mut ws_source) = ws_stream.split();

        // Rebuild reverse map from cache each cycle — cache is stable once initialized.
        let id_to_symbol = build_id_to_symbol(&market_id_cache).await;

        // Per-connection local book state — dropped on reconnect, rebuilt via snapshot.
        let mut market_states: HashMap<u64, MarketState> = HashMap::new();

        // Clear stale snapshots so callers block until fresh data arrives.
        snapshots.write().await.clear();

        // Re-subscribe all previously active symbols in a single batch.
        {
            let syms: Vec<String> = subscribed.lock().await.iter().cloned().collect();
            if !syms.is_empty() {
                let ids: Vec<u64> = resolve_to_ids(&syms, &id_to_symbol, &market_id_cache).await;
                if !ids.is_empty() {
                    match build_subscribe_msg(&ids) {
                        Ok(msg) => {
                            if let Err(e) = ws_sink.send(Message::Text(msg.into())).await {
                                tracing::error!("RisexOrderbookManager: re-subscribe error: {}", e);
                            } else {
                                tracing::info!(
                                    "RisexOrderbookManager: re-subscribed {} markets",
                                    ids.len()
                                );
                            }
                        }
                        Err(e) => {
                            tracing::error!(
                                "RisexOrderbookManager: build re-subscribe msg failed: {}",
                                e
                            );
                        }
                    }
                }
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
                            let reconnect = handle_message(
                                &text,
                                &id_to_symbol,
                                &mut market_states,
                                &snapshots,
                                &notifiers,
                            )
                            .await;
                            if reconnect {
                                break 'connection;
                            }
                        }
                        Some(Ok(Message::Ping(payload))) => {
                            last_message_at = Instant::now();
                            if let Err(e) = ws_sink.send(Message::Pong(payload)).await {
                                tracing::error!("RisexOrderbookManager: pong error: {}", e);
                                break 'connection;
                            }
                        }
                        Some(Ok(Message::Pong(_))) => {
                            last_message_at = Instant::now();
                        }
                        Some(Ok(Message::Close(_))) => {
                            tracing::info!("RisexOrderbookManager: server closed connection");
                            break 'connection;
                        }
                        Some(Ok(_)) => {}
                        Some(Err(e)) => {
                            tracing::error!("RisexOrderbookManager: ws error: {}", e);
                            break 'connection;
                        }
                        None => {
                            tracing::info!("RisexOrderbookManager: stream ended");
                            break 'connection;
                        }
                    }
                }

                req = subscribe_rx.recv() => {
                    match req {
                        Some(new_syms) => {
                            // Drain any additional pending requests before locking.
                            let mut all_new = new_syms;
                            while let Ok(more) = subscribe_rx.try_recv() {
                                all_new.extend(more);
                            }

                            // Filter to only truly new symbols, add to tracked set.
                            let truly_new: Vec<String> = {
                                let mut sub = subscribed.lock().await;
                                all_new.into_iter().filter(|s| sub.insert(s.clone())).collect()
                            };

                            if truly_new.is_empty() {
                                continue;
                            }

                            let ids = resolve_to_ids(&truly_new, &id_to_symbol, &market_id_cache).await;
                            if ids.is_empty() {
                                continue;
                            }

                            match build_subscribe_msg(&ids) {
                                Ok(msg) => {
                                    if let Err(e) = ws_sink.send(Message::Text(msg.into())).await {
                                        tracing::error!(
                                            "RisexOrderbookManager: subscribe send error: {}",
                                            e
                                        );
                                        break 'connection;
                                    }
                                    tracing::debug!(
                                        "RisexOrderbookManager: subscribed {:?} → {} markets",
                                        truly_new,
                                        ids.len()
                                    );
                                }
                                Err(e) => {
                                    tracing::error!(
                                        "RisexOrderbookManager: build subscribe msg failed: {}",
                                        e
                                    );
                                }
                            }
                        }
                        None => {
                            tracing::debug!("RisexOrderbookManager: subscribe_rx closed, shutting down");
                            return;
                        }
                    }
                }

                _ = watchdog.tick() => {
                    if last_message_at.elapsed() > Duration::from_secs(INACTIVITY_TIMEOUT_SECS) {
                        tracing::warn!(
                            "RisexOrderbookManager: no message for {}s, reconnecting",
                            INACTIVITY_TIMEOUT_SECS
                        );
                        break 'connection;
                    }
                }
            }
        }

        tracing::info!(
            "RisexOrderbookManager: reconnecting in {}s",
            RECONNECT_DELAY_SECS
        );
        time::sleep(Duration::from_secs(RECONNECT_DELAY_SECS)).await;
    }
}

/// Resolve a list of global symbols to market_ids using both the reverse map (fast path,
/// already built for this connection) and the cache (fallback for newly added symbols).
async fn resolve_to_ids(
    symbols: &[String],
    id_to_symbol: &HashMap<u64, String>,
    cache: &ContractCache<u64>,
) -> Vec<u64> {
    let mut ids = Vec::with_capacity(symbols.len());
    for sym in symbols {
        // Fast path: reverse map already built from cache.
        if let Some(id) = id_to_symbol.iter().find(|(_, s)| *s == sym).map(|(id, _)| *id) {
            ids.push(id);
        } else if let Some(id) = cache.get(sym).await {
            // Fallback: direct cache lookup (handles symbols added after map was built).
            ids.push(id);
        } else {
            tracing::warn!(
                "RisexOrderbookManager: no market_id for '{}' — cache initialized?",
                sym
            );
        }
    }
    ids
}

/// Returns true if the caller should reconnect (checksum mismatch detected).
async fn handle_message(
    text: &str,
    id_to_symbol: &HashMap<u64, String>,
    market_states: &mut HashMap<u64, MarketState>,
    snapshots: &Arc<RwLock<HashMap<String, SnapshotEntry>>>,
    notifiers: &Arc<Mutex<HashMap<String, Arc<Notify>>>>,
) -> bool {
    let env: WsMsgType = match serde_json::from_str(text) {
        Ok(e) => e,
        Err(e) => {
            tracing::trace!("RisexOrderbookManager: envelope parse error: {}", e);
            return false;
        }
    };

    match env.msg_type.as_str() {
        "snapshot" => {
            let snap: WsSnapshot = match serde_json::from_str(text) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("RisexOrderbookManager: snapshot parse error: {}", e);
                    return false;
                }
            };
            let market_id: u64 = match snap.market_id.parse() {
                Ok(id) => id,
                Err(_) => {
                    tracing::warn!(
                        "RisexOrderbookManager: non-integer market_id {:?} in snapshot",
                        snap.market_id
                    );
                    return false;
                }
            };
            let symbol = match id_to_symbol.get(&market_id) {
                Some(s) => s.clone(),
                None => {
                    tracing::warn!(
                        "RisexOrderbookManager: unknown market_id {} in snapshot",
                        market_id
                    );
                    return false;
                }
            };
            let state = market_states
                .entry(market_id)
                .or_insert_with(|| MarketState::new(symbol.clone()));
            if let Err(e) = state.reset_from_snapshot(&snap.data.bids, &snap.data.asks) {
                tracing::warn!(
                    "RisexOrderbookManager: snapshot reset error for {}: {}",
                    symbol,
                    e
                );
                return false;
            }
            let ob = state.to_orderbook();
            let count_add = snap.data.bids.len() + snap.data.asks.len();
            push_snapshot(symbol, ob, "snap", count_add, 0, snapshots, notifiers).await;
        }

        "update" => {
            let upd: WsUpdate = match serde_json::from_str(text) {
                Ok(u) => u,
                Err(e) => {
                    tracing::warn!("RisexOrderbookManager: update parse error: {}", e);
                    return false;
                }
            };
            let market_id: u64 = match upd.market_id.parse() {
                Ok(id) => id,
                Err(_) => {
                    tracing::warn!(
                        "RisexOrderbookManager: non-integer market_id {:?} in update",
                        upd.market_id
                    );
                    return false;
                }
            };
            // Drop updates that arrive before the initial snapshot for this market.
            let state = match market_states.get_mut(&market_id) {
                Some(s) => s,
                None => {
                    tracing::debug!(
                        "RisexOrderbookManager: update before snapshot for market_id {}",
                        market_id
                    );
                    return false;
                }
            };
            let symbol = state.symbol.clone();
            if let Err(e) = state.apply_update(&upd.data.bids, &upd.data.asks) {
                tracing::warn!(
                    "RisexOrderbookManager: update apply error for {}: {}",
                    symbol,
                    e
                );
                return false;
            }
            let computed = state.checksum();
            if computed != upd.checksum {
                tracing::warn!(
                    "RisexOrderbookManager: checksum mismatch for {} (computed={} expected={}) — reconnecting",
                    symbol,
                    computed,
                    upd.checksum,
                );
                // Drop stale state; reconnect delivers a fresh snapshot.
                market_states.remove(&market_id);
                return true;
            }

            let ob = state.to_orderbook();
            let count_add = upd.data.bids.iter().filter(|l| l.quantity != "0").count()
                + upd.data.asks.iter().filter(|l| l.quantity != "0").count();
            let count_remove = upd.data.bids.iter().filter(|l| l.quantity == "0").count()
                + upd.data.asks.iter().filter(|l| l.quantity == "0").count();
            push_snapshot(symbol, ob, "upd", count_add, count_remove, snapshots, notifiers).await;
        }

        other => {
            tracing::trace!("RisexOrderbookManager: unhandled msg type={:?}", other);
        }
    }

    false
}

async fn push_snapshot(
    symbol: String,
    ob: Orderbook,
    msg_type: &str,
    count_add: usize,
    count_remove: usize,
    snapshots: &Arc<RwLock<HashMap<String, SnapshotEntry>>>,
    notifiers: &Arc<Mutex<HashMap<String, Arc<Notify>>>>,
) {
    let best_bid = ob.bids.first().map(|l| l.price);
    let best_ask = ob.asks.first().map(|l| l.price);
    let mid = best_bid.zip(best_ask).map(|(b, a)| (b + a) / Decimal::TWO);
    let bid_liq: Decimal = ob.bids.iter().map(|l| l.quantity * l.price).sum();
    let ask_liq: Decimal = ob.asks.iter().map(|l| l.quantity * l.price).sum();
    tracing::debug!(
        "RisexOrderbookManager: {} {} bestBid={} bestAsk={} mid={} bidLiq={:.4} askLiq={:.4} +{}/−{}",
        symbol,
        msg_type,
        best_bid.map(|p| format!("{:.2}", p)).unwrap_or_else(|| "-".to_string()),
        best_ask.map(|p| format!("{:.2}", p)).unwrap_or_else(|| "-".to_string()),
        mid.map(|m| format!("{:.2}", m)).unwrap_or_else(|| "-".to_string()),
        bid_liq,
        ask_liq,
        count_add,
        count_remove,
    );
    snapshots.write().await.insert(
        symbol.clone(),
        SnapshotEntry {
            orderbook: MultiResolutionOrderbook::from_single(ob),
            updated_at: Instant::now(),
        },
    );
    if let Some(n) = notifiers.lock().await.get(&symbol) {
        n.notify_waiters();
    }
}

// ─── RisexWsClient ────────────────────────────────────────────────────────────

/// RISEx WebSocket streaming client.
///
/// Only the orderbook channel is exposed via `RisexOrderbookManager`. ticker/trade/multi
/// streaming is not available over the RISEx WS API.
#[derive(Clone)]
pub struct RisexWsClient {
    pub orderbook_manager: Arc<RisexOrderbookManager>,
}

impl RisexWsClient {
    pub fn new(market_id_cache: ContractCache<u64>) -> Self {
        Self {
            orderbook_manager: Arc::new(RisexOrderbookManager::new(market_id_cache)),
        }
    }
}

#[async_trait]
impl IPerpsStream for RisexWsClient {
    fn get_name(&self) -> &str {
        "risex"
    }

    async fn stream_tickers(&self, _symbols: Vec<String>) -> Result<DataStream<Ticker>> {
        Err(anyhow!("RISEx WS exposes orderbook channel only; use RisexOrderbookManager"))
    }

    async fn stream_trades(&self, _symbols: Vec<String>) -> Result<DataStream<Trade>> {
        Err(anyhow!("RISEx WS exposes orderbook channel only; use RisexOrderbookManager"))
    }

    async fn stream_orderbooks(&self, _symbols: Vec<String>) -> Result<DataStream<Orderbook>> {
        Err(anyhow!("use RisexOrderbookManager::get_orderbook for snapshot-based access"))
    }

    async fn stream_multi(&self, _config: StreamConfig) -> Result<DataStream<StreamEvent>> {
        Err(anyhow!("RISEx WS exposes orderbook channel only; use RisexOrderbookManager"))
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn decimal_to_wei_str_no_trailing_dot() {
        // Ensure no ".0" suffix — Go's big.Int.String() produces plain integers.
        assert_eq!(decimal_to_wei_str(dec!(64043.4)), "64043400000000000000000");
        assert_eq!(decimal_to_wei_str(dec!(1.300806)), "1300806000000000000");
        assert_eq!(decimal_to_wei_str(Decimal::ZERO), "0");
    }

    #[test]
    fn parse_price_basic() {
        assert_eq!(parse_price("64043.4").unwrap(), dec!(64043.4));
    }

    #[test]
    fn parse_price_zero() {
        assert_eq!(parse_price("0").unwrap(), Decimal::ZERO);
    }

    #[test]
    fn parse_price_integer() {
        assert_eq!(parse_price("3300").unwrap(), dec!(3300));
    }

    #[test]
    fn market_state_snapshot_sorts_correctly() {
        let mut state = MarketState::new("BTC".to_string());
        let bids = vec![
            WsLevel { price: "95000".to_string(), quantity: "1.0".to_string() },
            WsLevel { price: "94000".to_string(), quantity: "2.0".to_string() },
            WsLevel { price: "96000".to_string(), quantity: "0.5".to_string() },
        ];
        let asks = vec![
            WsLevel { price: "97000".to_string(), quantity: "1.0".to_string() },
            WsLevel { price: "98000".to_string(), quantity: "1.5".to_string() },
        ];
        state.reset_from_snapshot(&bids, &asks).unwrap();

        let ob = state.to_orderbook();

        // Bids descending
        assert!(ob.bids[0].price > ob.bids[1].price, "bids must be descending");
        assert!(ob.bids[1].price > ob.bids[2].price, "bids must be descending");

        // Asks ascending
        assert!(ob.asks[0].price < ob.asks[1].price, "asks must be ascending");

        // No cross
        assert!(ob.bids[0].price < ob.asks[0].price, "book must not be crossed");
    }

    #[test]
    fn market_state_delete_level_on_zero_qty() {
        let mut state = MarketState::new("BTC".to_string());
        let bids = vec![
            WsLevel { price: "95000".to_string(), quantity: "1.0".to_string() },
        ];
        state.reset_from_snapshot(&bids, &[]).unwrap();
        assert_eq!(state.bids.len(), 1);

        let delete = vec![
            WsLevel { price: "95000".to_string(), quantity: "0".to_string() },
        ];
        state.apply_update(&delete, &[]).unwrap();
        assert_eq!(state.bids.len(), 0, "zero qty must remove the level");
    }

    #[test]
    fn market_state_update_replaces_qty() {
        let mut state = MarketState::new("BTC".to_string());
        let bids = vec![
            WsLevel { price: "95000".to_string(), quantity: "1.0".to_string() },
        ];
        state.reset_from_snapshot(&bids, &[]).unwrap();

        let update = vec![
            WsLevel { price: "95000".to_string(), quantity: "3.0".to_string() },
        ];
        state.apply_update(&update, &[]).unwrap();

        let ob = state.to_orderbook();
        assert_eq!(ob.bids[0].quantity, dec!(3.0), "qty must be replaced by update");
    }
}
