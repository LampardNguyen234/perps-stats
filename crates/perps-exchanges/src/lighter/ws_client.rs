use super::ws_types::*;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use futures::{SinkExt, StreamExt};
use perps_core::streaming::*;
use perps_core::types::*;
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Mutex, Notify, RwLock};
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

const WS_BASE_URL: &str = "wss://mainnet.zklighter.elliot.ai/stream";
const SNAPSHOT_TTL: Duration = Duration::from_secs(5);

fn clip_orderbook(mut ob: Orderbook, depth: u32) -> Orderbook {
    let d = depth as usize;
    ob.bids.truncate(d);
    ob.asks.truncate(d);
    ob
}

struct SnapshotEntry {
    orderbook: Orderbook,
    captured_at: Instant,
}

/// Per-market local L2 state maintained by the WS background task.
///
/// First message for a market = full snapshot → replaces maps.
/// Subsequent messages = incremental delta → apply changes, check nonce.
/// size "0" → delete that price level.
struct MarketState {
    bids: BTreeMap<Decimal, Decimal>, // price → size, ascending (iter().rev() = best-bid-first)
    asks: BTreeMap<Decimal, Decimal>, // price → size, ascending (iter() = best-ask-first)
    nonce: u64,
}

impl MarketState {
    fn new() -> Self {
        Self { bids: BTreeMap::new(), asks: BTreeMap::new(), nonce: 0 }
    }

    fn apply_levels(
        map: &mut BTreeMap<Decimal, Decimal>,
        levels: &[LighterOrderbookLevel],
    ) -> Result<()> {
        for l in levels {
            let price = Decimal::from_str(&l.price)
                .map_err(|e| anyhow!("price parse '{}': {}", l.price, e))?;
            let size = Decimal::from_str(&l.size)
                .map_err(|e| anyhow!("size parse '{}': {}", l.size, e))?;
            if size.is_zero() {
                map.remove(&price);
            } else {
                map.insert(price, size);
            }
        }
        Ok(())
    }

    /// Populate from the first (full-snapshot) message.
    fn apply_snapshot(&mut self, ob: &LighterOrderBook) -> Result<()> {
        self.bids.clear();
        self.asks.clear();
        Self::apply_levels(&mut self.bids, &ob.bids)?;
        Self::apply_levels(&mut self.asks, &ob.asks)?;
        self.nonce = ob.nonce;
        Ok(())
    }

    /// Apply an incremental delta.  Returns `Err` on nonce gap → caller reconnects.
    fn apply_delta(&mut self, ob: &LighterOrderBook) -> Result<()> {
        if ob.begin_nonce != self.nonce {
            return Err(anyhow!(
                "nonce gap: expected {} got begin_nonce {}",
                self.nonce,
                ob.begin_nonce
            ));
        }
        Self::apply_levels(&mut self.bids, &ob.bids)?;
        Self::apply_levels(&mut self.asks, &ob.asks)?;
        self.nonce = ob.nonce;
        Ok(())
    }

    fn to_orderbook(&self, symbol: &str) -> Orderbook {
        Orderbook {
            symbol: symbol.to_string(),
            bids: self
                .bids
                .iter()
                .rev()
                .map(|(p, q)| OrderbookLevel { price: *p, quantity: *q })
                .collect(),
            asks: self
                .asks
                .iter()
                .map(|(p, q)| OrderbookLevel { price: *p, quantity: *q })
                .collect(),
            timestamp: Utc::now(),
        }
    }
}

/// Persistent WS-backed orderbook manager for Lighter.
///
/// Maintains a reconnecting WS connection, applies incremental deltas onto
/// per-market BTreeMaps, and caches the resulting Orderbook for pull-based
/// `get_orderbook` calls.  Wired into `LighterClient::get_orderbook` when
/// `ENABLE_ORDERBOOK_STREAMING=true`.
pub struct LighterOrderbookManager {
    snapshots: Arc<RwLock<HashMap<String, SnapshotEntry>>>,
    notifiers: Arc<Mutex<HashMap<String, Arc<Notify>>>>,
    subscribed: Arc<Mutex<HashSet<String>>>,
    subscribe_tx: mpsc::Sender<Vec<String>>,
}

impl LighterOrderbookManager {
    pub fn new(market_id_cache: Arc<RwLock<HashMap<String, u64>>>, base_url: String) -> Self {
        let snapshots: Arc<RwLock<HashMap<String, SnapshotEntry>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let notifiers: Arc<Mutex<HashMap<String, Arc<Notify>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let subscribed: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
        let (subscribe_tx, subscribe_rx) = mpsc::channel::<Vec<String>>(64);

        tokio::spawn(run_background_task(
            snapshots.clone(),
            notifiers.clone(),
            subscribed.clone(),
            subscribe_rx,
            market_id_cache,
            base_url,
        ));

        Self { snapshots, notifiers, subscribed, subscribe_tx }
    }

    pub async fn get_orderbook(&self, symbol: &str, depth: u32) -> Result<MultiResolutionOrderbook> {
        let notifier = {
            let mut g = self.notifiers.lock().await;
            g.entry(symbol.to_string()).or_insert_with(|| Arc::new(Notify::new())).clone()
        };

        let newly = { self.subscribed.lock().await.insert(symbol.to_string()) };
        if newly {
            let _ = self.subscribe_tx.send(vec![symbol.to_string()]).await;
        }

        {
            let snap = self.snapshots.read().await;
            if let Some(e) = snap.get(symbol) {
                if e.captured_at.elapsed() < SNAPSHOT_TTL {
                    return Ok(MultiResolutionOrderbook::from_single(clip_orderbook(
                        e.orderbook.clone(),
                        depth,
                    )));
                }
            }
        }

        tokio::time::timeout(Duration::from_secs(10), notifier.notified())
            .await
            .map_err(|_| anyhow!("Lighter orderbook timeout for {}", symbol))?;

        let snap = self.snapshots.read().await;
        let e = snap
            .get(symbol)
            .ok_or_else(|| anyhow!("No Lighter snapshot for {} after notify", symbol))?;
        Ok(MultiResolutionOrderbook::from_single(clip_orderbook(e.orderbook.clone(), depth)))
    }
}

/// Fetch market_id → symbol reverse map.  Uses the shared cache; falls back to
/// a one-shot REST call when the cache is empty (e.g. on first connect before
/// any REST call has populated it).
async fn resolve_id_to_symbol(
    cache: &Arc<RwLock<HashMap<String, u64>>>,
    base_url: &str,
) -> HashMap<u64, String> {
    {
        let g = cache.read().await;
        if !g.is_empty() {
            return g.iter().map(|(s, &id)| (id, s.clone())).collect();
        }
    }
    let url = format!("{}/orderBooks", base_url);
    match reqwest::get(&url).await {
        Ok(resp) => {
            // LighterResponse uses #[serde(flatten)] so JSON is:
            // {"code":200,"order_books":[...]} — no nested "data" wrapper.
            #[derive(serde::Deserialize)]
            struct OB {
                symbol: String,
                market_id: u64,
            }
            #[derive(serde::Deserialize)]
            struct Wrap {
                order_books: Vec<OB>,
            }
            match resp.json::<Wrap>().await {
                Ok(w) => {
                    let mut g = cache.write().await;
                    for ob in &w.order_books {
                        // Normalize via alias table so keys match what LighterClient::get_market_id
                        // inserts (e.g. "CL" → "WTI"). Lighter API returns simple uppercase symbols
                        // so only alias resolution is needed here, not full parse_symbol logic.
                        let sym = crate::symbol_aliases::resolve_alias("lighter", &ob.symbol)
                            .to_string();
                        g.insert(sym, ob.market_id);
                    }
                    g.iter().map(|(s, &id)| (id, s.clone())).collect()
                }
                Err(e) => {
                    tracing::warn!("LighterOrderbookManager: market_id parse error: {}", e);
                    HashMap::new()
                }
            }
        }
        Err(e) => {
            tracing::warn!("LighterOrderbookManager: market_id fetch error: {}", e);
            HashMap::new()
        }
    }
}

async fn run_background_task(
    snapshots: Arc<RwLock<HashMap<String, SnapshotEntry>>>,
    notifiers: Arc<Mutex<HashMap<String, Arc<Notify>>>>,
    subscribed: Arc<Mutex<HashSet<String>>>,
    mut subscribe_rx: mpsc::Receiver<Vec<String>>,
    market_id_cache: Arc<RwLock<HashMap<String, u64>>>,
    base_url: String,
) {
    loop {
        let id_to_symbol = resolve_id_to_symbol(&market_id_cache, &base_url).await;
        let symbol_to_id: HashMap<String, u64> =
            id_to_symbol.iter().map(|(id, s)| (s.clone(), *id)).collect();

        let ws_stream = match connect_async(WS_BASE_URL).await {
            Ok((s, _)) => s,
            Err(e) => {
                tracing::warn!("LighterOrderbookManager: connect error: {}", e);
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        tracing::info!("LighterOrderbookManager: connected");

        // Stale snapshots cleared so callers wait for fresh data on reconnect.
        snapshots.write().await.clear();

        let (mut write, mut read) = ws_stream.split();

        // Local book state per market; rebuilt fresh on every connection cycle.
        let mut market_states: HashMap<u64, MarketState> = HashMap::new();

        // Re-subscribe all tracked symbols.
        {
            let syms: Vec<String> = subscribed.lock().await.iter().cloned().collect();
            for sym in &syms {
                if let Some(&id) = symbol_to_id.get(sym) {
                    send_subscribe(&mut write, id).await;
                } else {
                    tracing::warn!("LighterOrderbookManager: no market_id for {}", sym);
                }
            }
        }

        let mut keepalive = tokio::time::interval(Duration::from_secs(30));
        keepalive.tick().await;

        'conn: loop {
            tokio::select! {
                maybe_msg = read.next() => {
                    match maybe_msg {
                        Some(Ok(Message::Text(text))) => {
                            // Ignore non-orderbook messages (acks, market_stats, etc.)
                            let ob_msg = match serde_json::from_str::<LighterWsOrderbook>(&text) {
                                Ok(m) => m,
                                Err(_) => continue,
                            };
                            // Channel on receive: "order_book:{market_id}"
                            let market_id: u64 = match ob_msg.channel
                                .strip_prefix("order_book:")
                                .and_then(|s| s.parse().ok())
                            {
                                Some(id) => id,
                                None => continue,
                            };
                            let symbol = match id_to_symbol.get(&market_id) {
                                Some(s) => s.clone(),
                                None => continue,
                            };

                            let apply_result = if let Some(state) = market_states.get_mut(&market_id) {
                                // Incremental delta
                                state.apply_delta(&ob_msg.order_book)
                            } else {
                                // First message = full snapshot
                                let mut state = MarketState::new();
                                let r = state.apply_snapshot(&ob_msg.order_book);
                                if r.is_ok() {
                                    market_states.insert(market_id, state);
                                }
                                r
                            };

                            match apply_result {
                                Err(e) => {
                                    // Nonce gap or parse error — drop state, reconnect.
                                    tracing::warn!(
                                        "LighterOrderbookManager: {} error: {} — reconnecting",
                                        symbol, e
                                    );
                                    market_states.remove(&market_id);
                                    snapshots.write().await.remove(&symbol);
                                    break 'conn;
                                }
                                Ok(()) => {
                                    if let Some(state) = market_states.get(&market_id) {
                                        let ob = state.to_orderbook(&symbol);
                                        let best_bid = ob.bids.first().map(|l| l.price);
                                        let best_ask = ob.asks.first().map(|l| l.price);
                                        let mid = best_bid.zip(best_ask).map(|(b, a)| (b + a) / Decimal::TWO);
                                        let bid_liq: Decimal = ob.bids.iter().map(|l| l.price * l.quantity).sum();
                                        let ask_liq: Decimal = ob.asks.iter().map(|l| l.price * l.quantity).sum();
                                        tracing::debug!(
                                            "LighterOrderbookManager: {} bestBid={} bestAsk={} mid={} bidLiq={:.4} askLiq={:.4}",
                                            symbol,
                                            best_bid.map(|p| format!("{:.2}", p)).unwrap_or_else(|| "-".to_string()),
                                            best_ask.map(|p| format!("{:.2}", p)).unwrap_or_else(|| "-".to_string()),
                                            mid.map(|m| format!("{:.2}", m)).unwrap_or_else(|| "-".to_string()),
                                            bid_liq, ask_liq,
                                        );
                                        snapshots.write().await.insert(
                                            symbol.clone(),
                                            SnapshotEntry { orderbook: ob, captured_at: Instant::now() },
                                        );
                                        let g = notifiers.lock().await;
                                        if let Some(n) = g.get(&symbol) {
                                            n.notify_waiters();
                                        }
                                    }
                                }
                            }
                        }
                        Some(Ok(Message::Ping(p))) => {
                            let _ = write.send(Message::Pong(p)).await;
                        }
                        Some(Ok(Message::Close(_))) | None => {
                            tracing::info!("LighterOrderbookManager: disconnected, reconnecting");
                            break 'conn;
                        }
                        Some(Err(e)) => {
                            tracing::warn!("LighterOrderbookManager: ws error: {}", e);
                            break 'conn;
                        }
                        _ => {}
                    }
                }
                Some(new_syms) = subscribe_rx.recv() => {
                    for sym in new_syms {
                        if let Some(&id) = symbol_to_id.get(&sym) {
                            send_subscribe(&mut write, id).await;
                        } else {
                            tracing::warn!("LighterOrderbookManager: no market_id for {}", sym);
                        }
                    }
                }
                _ = keepalive.tick() => {
                    if let Err(e) = write.send(Message::Ping(vec![])).await {
                        tracing::warn!("LighterOrderbookManager: keepalive error: {}", e);
                        break 'conn;
                    }
                }
            }
        }

        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn send_subscribe(
    write: &mut futures::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
        Message,
    >,
    market_id: u64,
) {
    let req = LighterWsSubscribeRequest {
        msg_type: "subscribe".to_string(),
        channel: format!("order_book/{}", market_id),
    };
    if let Ok(msg) = serde_json::to_string(&req) {
        if let Err(e) = write.send(Message::Text(msg)).await {
            tracing::warn!("LighterOrderbookManager: subscribe send error: {}", e);
        }
    }
}

// ---------------------------------------------------------------------------
// IPerpsStream impl (existing streaming API — unchanged)
// ---------------------------------------------------------------------------

/// Lighter WebSocket streaming client
#[derive(Clone)]
pub struct LighterWsClient {
    base_url: String,
    /// Cache for symbol to market_id mapping
    symbol_to_market_id: HashMap<String, u64>,
}

impl LighterWsClient {
    pub fn new() -> Self {
        Self {
            base_url: WS_BASE_URL.to_string(),
            symbol_to_market_id: HashMap::new(),
        }
    }

    /// Connect to WebSocket and return the stream
    async fn connect(&self) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>> {
        tracing::info!("Connecting to Lighter WebSocket: {}", self.base_url);
        let (ws_stream, response) = connect_async(&self.base_url).await?;
        tracing::info!(
            "Connected to Lighter WebSocket (status: {:?})",
            response.status()
        );
        Ok(ws_stream)
    }

    /// Subscribe to a channel
    async fn subscribe(
        &self,
        ws_stream: &mut WebSocketStream<MaybeTlsStream<TcpStream>>,
        channel: String,
    ) -> Result<()> {
        let request = LighterWsSubscribeRequest {
            msg_type: "subscribe".to_string(),
            channel,
        };
        let sub_message = serde_json::to_string(&request)?;
        tracing::debug!("Subscribing: {}", sub_message);
        ws_stream.send(Message::Text(sub_message)).await?;
        Ok(())
    }

    /// Get market_id for a symbol by fetching from REST API
    async fn get_market_id(&mut self, symbol: &str) -> Result<u64> {
        // Check cache first
        if let Some(&market_id) = self.symbol_to_market_id.get(symbol) {
            return Ok(market_id);
        }

        // Fetch from REST API
        let url = "https://mainnet.zklighter.elliot.ai/api/v1/orderBooks";
        let response = reqwest::get(url).await?;
        let data: serde_json::Value = response.json().await?;

        // Parse the response (note: response structure is different - no "data" wrapper)
        if let Some(order_books) = data["order_books"].as_array() {
            for ob in order_books {
                if let (Some(sym), Some(id)) = (ob["symbol"].as_str(), ob["market_id"].as_u64()) {
                    self.symbol_to_market_id.insert(sym.to_string(), id);
                }
            }
        }

        // Try again from cache
        self.symbol_to_market_id
            .get(symbol)
            .copied()
            .ok_or_else(|| anyhow!("Symbol {} not found", symbol))
    }

    /// Convert Lighter market stats to our Ticker type
    fn convert_ticker(&self, stats: &LighterMarketStatsData) -> Result<Ticker> {
        let last_price = Decimal::from_f64(stats.last_trade_price)
            .ok_or_else(|| anyhow!("Invalid last_trade_price"))?;
        let mark_price =
            Decimal::from_f64(stats.mark_price).ok_or_else(|| anyhow!("Invalid mark_price"))?;
        let index_price =
            Decimal::from_f64(stats.index_price).ok_or_else(|| anyhow!("Invalid index_price"))?;

        // Calculate price change from percentage
        let price_change_pct = Decimal::from_f64(stats.daily_price_change).unwrap_or(Decimal::ZERO)
            / Decimal::from(100);
        let price_change_24h = price_change_pct * last_price;

        Ok(Ticker {
            symbol: stats.symbol.clone(),
            last_price,
            mark_price,
            index_price,
            best_bid_price: Decimal::ZERO, // Not available in market stats
            best_bid_qty: Decimal::ZERO,
            best_ask_price: Decimal::ZERO,
            best_ask_qty: Decimal::ZERO,
            volume_24h: Decimal::from_f64(stats.daily_volume).unwrap_or(Decimal::ZERO),
            turnover_24h: Decimal::from_f64(stats.daily_quote_volume).unwrap_or(Decimal::ZERO),
            open_interest: Decimal::ZERO,
            open_interest_notional: Decimal::ZERO,
            price_change_24h,
            price_change_pct,
            high_price_24h: Decimal::from_f64(stats.daily_price_high).unwrap_or(Decimal::ZERO),
            low_price_24h: Decimal::from_f64(stats.daily_price_low).unwrap_or(Decimal::ZERO),
            timestamp: if let Some(ts) = stats.timestamp {
                Utc.timestamp_millis_opt(ts).unwrap()
            } else {
                Utc::now()
            },
        })
    }

    /// Convert Lighter trade to our Trade type
    fn convert_trade(&self, trade: &LighterTradeData) -> Result<Trade> {
        Ok(Trade {
            id: trade.trade_id.clone(),
            symbol: trade.symbol.clone(),
            price: Decimal::from_str(&trade.price)?,
            quantity: Decimal::from_str(&trade.size)?,
            side: match trade.side.as_str() {
                "buy" => OrderSide::Buy,
                "sell" => OrderSide::Sell,
                _ => OrderSide::Buy,
            },
            timestamp: Utc.timestamp_millis_opt(trade.timestamp).unwrap(),
        })
    }

}

impl Default for LighterWsClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IPerpsStream for LighterWsClient {
    fn get_name(&self) -> &str {
        "lighter"
    }

    async fn stream_tickers(&self, symbols: Vec<String>) -> Result<DataStream<Ticker>> {
        let mut ws_stream = self.connect().await?;
        let mut client = self.clone();

        // Get market IDs for all symbols
        let mut market_ids = Vec::new();
        for symbol in &symbols {
            let market_id = client.get_market_id(symbol).await?;
            market_ids.push(market_id);
        }

        // Subscribe to market stats for each market
        for market_id in &market_ids {
            let channel = format!("market_stats/{}", market_id);
            self.subscribe(&mut ws_stream, channel).await?;
        }

        let client = self.clone();

        let stream = async_stream::stream! {
            use tokio::time::{interval, Duration};
            let mut keepalive = interval(Duration::from_secs(30));
            keepalive.tick().await; // Skip first tick

            loop {
                tokio::select! {
                    maybe_msg = ws_stream.next() => {
                        match maybe_msg {
                            Some(Ok(Message::Text(text))) => {
                                tracing::debug!("Received message: {}", &text[..text.len().min(200)]);

                                if let Ok(stats_msg) = serde_json::from_str::<LighterWsMarketStats>(&text) {
                                    match client.convert_ticker(&stats_msg.market_stats) {
                                        Ok(ticker) => yield Ok(ticker),
                                        Err(e) => yield Err(anyhow!("Failed to convert ticker: {}", e)),
                                    }
                                }
                            }
                            Some(Ok(Message::Ping(payload))) => {
                                if let Err(e) = ws_stream.send(Message::Pong(payload)).await {
                                    tracing::error!("Failed to send pong: {}", e);
                                    yield Err(anyhow!("Failed to send pong: {}", e));
                                    break;
                                }
                            }
                            Some(Ok(Message::Pong(_))) => {
                                // Received pong response to our ping
                                tracing::trace!("Received pong from Lighter");
                            }
                            Some(Ok(Message::Close(frame))) => {
                                tracing::info!("WebSocket connection closed: {:?}", frame);
                                break;
                            }
                            Some(Err(e)) => {
                                yield Err(anyhow!("WebSocket error: {}", e));
                                break;
                            }
                            None => {
                                tracing::warn!("Lighter WebSocket stream ended (connection closed by server)");
                                yield Err(anyhow!("WebSocket stream ended"));
                                break;
                            }
                            _ => {}
                        }
                    }
                    _ = keepalive.tick() => {
                        // Send periodic ping to keep connection alive
                        if let Err(e) = ws_stream.send(Message::Ping(vec![])).await {
                            tracing::error!("Failed to send keepalive ping: {}", e);
                            yield Err(anyhow!("Failed to send keepalive ping: {}", e));
                            break;
                        }
                        tracing::trace!("Sent keepalive ping to Lighter");
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }

    async fn stream_trades(&self, symbols: Vec<String>) -> Result<DataStream<Trade>> {
        let mut ws_stream = self.connect().await?;
        let mut client = self.clone();

        // Get market IDs for all symbols
        let mut market_ids = Vec::new();
        for symbol in &symbols {
            let market_id = client.get_market_id(symbol).await?;
            market_ids.push(market_id);
        }

        // Subscribe to trades for each market
        for market_id in &market_ids {
            let channel = format!("trade/{}", market_id);
            self.subscribe(&mut ws_stream, channel).await?;
        }

        let client = self.clone();

        let stream = async_stream::stream! {
            use tokio::time::{interval, Duration};
            let mut keepalive = interval(Duration::from_secs(30));
            keepalive.tick().await; // Skip first tick

            loop {
                tokio::select! {
                    maybe_msg = ws_stream.next() => {
                        match maybe_msg {
                            Some(Ok(Message::Text(text))) => {
                                if let Ok(trade_msg) = serde_json::from_str::<LighterWsTrade>(&text) {
                                    for trade in &trade_msg.trades {
                                        match client.convert_trade(trade) {
                                            Ok(trade) => yield Ok(trade),
                                            Err(e) => yield Err(anyhow!("Failed to convert trade: {}", e)),
                                        }
                                    }
                                }
                            }
                            Some(Ok(Message::Ping(payload))) => {
                                if let Err(e) = ws_stream.send(Message::Pong(payload)).await {
                                    yield Err(anyhow!("Failed to send pong: {}", e));
                                    break;
                                }
                            }
                            Some(Ok(Message::Pong(_))) => {
                                tracing::trace!("Received pong from Lighter");
                            }
                            Some(Ok(Message::Close(frame))) => {
                                tracing::info!("WebSocket connection closed: {:?}", frame);
                                break;
                            }
                            Some(Err(e)) => {
                                yield Err(anyhow!("WebSocket error: {}", e));
                                break;
                            }
                            None => {
                                tracing::warn!("Lighter WebSocket stream ended (connection closed by server)");
                                yield Err(anyhow!("WebSocket stream ended"));
                                break;
                            }
                            _ => {}
                        }
                    }
                    _ = keepalive.tick() => {
                        if let Err(e) = ws_stream.send(Message::Ping(vec![])).await {
                            tracing::error!("Failed to send keepalive ping: {}", e);
                            yield Err(anyhow!("Failed to send keepalive ping: {}", e));
                            break;
                        }
                        tracing::trace!("Sent keepalive ping to Lighter");
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }

    async fn stream_orderbooks(&self, symbols: Vec<String>) -> Result<DataStream<Orderbook>> {
        let mut ws_stream = self.connect().await?;
        let mut client = self.clone();

        let mut market_id_to_symbol: HashMap<u64, String> = HashMap::new();
        for symbol in &symbols {
            let market_id = client.get_market_id(symbol).await?;
            market_id_to_symbol.insert(market_id, symbol.clone());
            let channel = format!("order_book/{}", market_id);
            self.subscribe(&mut ws_stream, channel).await?;
        }

        let stream = async_stream::stream! {
            use tokio::time::{interval, Duration};
            let mut keepalive = interval(Duration::from_secs(30));
            keepalive.tick().await;

            // Per-market BTreeMap state: snapshot on first message, delta after.
            let mut market_states: HashMap<u64, MarketState> = HashMap::new();

            loop {
                tokio::select! {
                    maybe_msg = ws_stream.next() => {
                        match maybe_msg {
                            Some(Ok(Message::Text(text))) => {
                                let ob_msg = match serde_json::from_str::<LighterWsOrderbook>(&text) {
                                    Ok(m) => m,
                                    Err(_) => continue,
                                };
                                let market_id: u64 = match ob_msg.channel
                                    .strip_prefix("order_book:")
                                    .and_then(|s| s.parse().ok())
                                {
                                    Some(id) => id,
                                    None => continue,
                                };
                                let symbol = match market_id_to_symbol.get(&market_id) {
                                    Some(s) => s.clone(),
                                    None => continue,
                                };

                                let result = if let Some(state) = market_states.get_mut(&market_id) {
                                    state.apply_delta(&ob_msg.order_book)
                                } else {
                                    let mut state = MarketState::new();
                                    let r = state.apply_snapshot(&ob_msg.order_book);
                                    if r.is_ok() { market_states.insert(market_id, state); }
                                    r
                                };
                                match result {
                                    Ok(()) => {
                                        if let Some(state) = market_states.get(&market_id) {
                                            yield Ok(state.to_orderbook(&symbol));
                                        }
                                    }
                                    Err(e) => {
                                        // Nonce gap — yield error and drop stale state.
                                        market_states.remove(&market_id);
                                        yield Err(anyhow!("Lighter orderbook nonce gap for {}: {}", symbol, e));
                                    }
                                }
                            }
                            Some(Ok(Message::Ping(payload))) => {
                                if let Err(e) = ws_stream.send(Message::Pong(payload)).await {
                                    yield Err(anyhow!("Failed to send pong: {}", e));
                                    break;
                                }
                            }
                            Some(Ok(Message::Pong(_))) => {
                                tracing::trace!("Received pong from Lighter");
                            }
                            Some(Ok(Message::Close(frame))) => {
                                tracing::info!("WebSocket connection closed: {:?}", frame);
                                break;
                            }
                            Some(Err(e)) => {
                                yield Err(anyhow!("WebSocket error: {}", e));
                                break;
                            }
                            None => {
                                tracing::warn!("Lighter WebSocket stream ended");
                                yield Err(anyhow!("WebSocket stream ended"));
                                break;
                            }
                            _ => {}
                        }
                    }
                    _ = keepalive.tick() => {
                        if let Err(e) = ws_stream.send(Message::Ping(vec![])).await {
                            tracing::error!("Failed to send keepalive ping: {}", e);
                            yield Err(anyhow!("Failed to send keepalive ping: {}", e));
                            break;
                        }
                        tracing::trace!("Sent keepalive ping to Lighter");
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }

    async fn stream_multi(&self, config: StreamConfig) -> Result<DataStream<StreamEvent>> {
        let mut ws_stream = self.connect().await?;
        let mut client = self.clone();

        // Get market IDs and create lookups
        let mut market_id_to_symbol: HashMap<u64, String> = HashMap::new();
        for symbol in &config.symbols {
            let market_id = client.get_market_id(symbol).await?;
            market_id_to_symbol.insert(market_id, symbol.clone());

            // Subscribe to requested topics for this market
            for data_type in &config.data_types {
                let channel = match data_type {
                    StreamDataType::Ticker => format!("market_stats/{}", market_id),
                    StreamDataType::Trade => format!("trade/{}", market_id),
                    StreamDataType::Orderbook => format!("order_book/{}", market_id),
                    StreamDataType::FundingRate => continue, // Not available via WebSocket
                    StreamDataType::Kline => continue, // Klines not yet implemented for Lighter WebSocket
                };
                self.subscribe(&mut ws_stream, channel).await?;
            }
        }

        let client = self.clone();

        let stream = async_stream::stream! {
            use tokio::time::{interval, Duration};
            let mut keepalive = interval(Duration::from_secs(30));
            keepalive.tick().await;

            // Per-market BTreeMap state for orderbook delta merging.
            let mut market_states: HashMap<u64, MarketState> = HashMap::new();

            loop {
                tokio::select! {
                    maybe_msg = ws_stream.next() => {
                        match maybe_msg {
                            Some(Ok(Message::Text(text))) => {
                                if let Ok(stats_msg) = serde_json::from_str::<LighterWsMarketStats>(&text) {
                                    if let Ok(ticker) = client.convert_ticker(&stats_msg.market_stats) {
                                        yield Ok(StreamEvent::Ticker(ticker));
                                    }
                                } else if let Ok(trade_msg) = serde_json::from_str::<LighterWsTrade>(&text) {
                                    for trade in &trade_msg.trades {
                                        if let Ok(trade) = client.convert_trade(trade) {
                                            yield Ok(StreamEvent::Trade(trade));
                                        }
                                    }
                                } else if let Ok(ob_msg) = serde_json::from_str::<LighterWsOrderbook>(&text) {
                                    let market_id: u64 = match ob_msg.channel
                                        .strip_prefix("order_book:")
                                        .and_then(|s| s.parse().ok())
                                    {
                                        Some(id) => id,
                                        None => continue,
                                    };
                                    let symbol = match market_id_to_symbol.get(&market_id) {
                                        Some(s) => s.clone(),
                                        None => continue,
                                    };
                                    let result = if let Some(state) = market_states.get_mut(&market_id) {
                                        state.apply_delta(&ob_msg.order_book)
                                    } else {
                                        let mut state = MarketState::new();
                                        let r = state.apply_snapshot(&ob_msg.order_book);
                                        if r.is_ok() { market_states.insert(market_id, state); }
                                        r
                                    };
                                    match result {
                                        Ok(()) => {
                                            if let Some(state) = market_states.get(&market_id) {
                                                yield Ok(StreamEvent::Orderbook(state.to_orderbook(&symbol)));
                                            }
                                        }
                                        Err(e) => {
                                            market_states.remove(&market_id);
                                            yield Err(anyhow!("Lighter orderbook nonce gap for {}: {}", symbol, e));
                                        }
                                    }
                                }
                            }
                            Some(Ok(Message::Ping(payload))) => {
                                if let Err(e) = ws_stream.send(Message::Pong(payload)).await {
                                    yield Err(anyhow!("Failed to send pong: {}", e));
                                    break;
                                }
                            }
                            Some(Ok(Message::Pong(_))) => {
                                tracing::trace!("Received pong from Lighter");
                            }
                            Some(Ok(Message::Close(frame))) => {
                                tracing::info!("WebSocket connection closed: {:?}", frame);
                                break;
                            }
                            Some(Err(e)) => {
                                yield Err(anyhow!("WebSocket error: {}", e));
                                break;
                            }
                            None => {
                                tracing::warn!("Lighter WebSocket stream ended (connection closed by server)");
                                yield Err(anyhow!("WebSocket stream ended"));
                                break;
                            }
                            _ => {}
                        }
                    }
                    _ = keepalive.tick() => {
                        if let Err(e) = ws_stream.send(Message::Ping(vec![])).await {
                            tracing::error!("Failed to send keepalive ping: {}", e);
                            yield Err(anyhow!("Failed to send keepalive ping: {}", e));
                            break;
                        }
                        tracing::trace!("Sent keepalive ping to Lighter");
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }
}
