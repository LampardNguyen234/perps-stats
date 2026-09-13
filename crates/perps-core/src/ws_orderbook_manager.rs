//! Bounded, append-only WebSocket shards with session-local recovery.
//!
//! Adapters return an owned stream: its socket, parser state, control writes and heartbeat
//! timers live together and are dropped on reconnect. No detached exchange driver is needed.
use crate::{
    DataStream, DepthUpdate, IPerpsStream, MultiResolutionOrderbook, Orderbook, OrderbookManager,
    OrderbookManagerConfig, OrderbookStreamer,
};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::StreamExt;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    sync::{mpsc, oneshot, Mutex, Notify},
    task::JoinHandle,
};

pub const DEFAULT_MAX_SYMBOLS_PER_CONNECTION: usize = 20;

pub enum OrderbookUpdate {
    Full(MultiResolutionOrderbook),
    Delta(DepthUpdate),
}

#[async_trait]
pub trait WsOrderbookAdapter: Send + Sync {
    fn exchange_name(&self) -> &str;
    /// A fresh session for exactly these canonical symbols. Errors terminate the session.
    async fn connect_and_subscribe(
        &self,
        symbols: Vec<String>,
    ) -> Result<DataStream<OrderbookUpdate>>;
    fn is_incremental_delta(&self) -> bool {
        false
    }
}

pub struct FullOrderbookAdapter(pub Arc<dyn IPerpsStream>);
#[async_trait]
impl WsOrderbookAdapter for FullOrderbookAdapter {
    fn exchange_name(&self) -> &str {
        self.0.get_name()
    }
    async fn connect_and_subscribe(
        &self,
        symbols: Vec<String>,
    ) -> Result<DataStream<OrderbookUpdate>> {
        Ok(Box::pin(self.0.stream_orderbooks(symbols).await?.map(
            |item| {
                item.map(|book| OrderbookUpdate::Full(MultiResolutionOrderbook::from_single(book)))
            },
        )))
    }
}

pub struct DeltaOrderbookAdapter(pub Arc<dyn OrderbookStreamer>);
#[async_trait]
impl WsOrderbookAdapter for DeltaOrderbookAdapter {
    fn exchange_name(&self) -> &str {
        self.0.exchange_name()
    }
    fn is_incremental_delta(&self) -> bool {
        self.0.is_incremental_delta()
    }
    async fn connect_and_subscribe(
        &self,
        symbols: Vec<String>,
    ) -> Result<DataStream<OrderbookUpdate>> {
        Ok(Box::pin(
            self.0
                .stream_depth_updates(symbols)
                .await?
                .map(|item| item.map(OrderbookUpdate::Delta)),
        ))
    }
}

#[derive(Clone, Debug)]
pub struct WsOrderbookConfig {
    pub max_symbols_per_connection: usize,
    pub with_delta: bool,
    pub reconnect_delay: Duration,
    pub subscribe_debounce: Duration,
    pub staleness_threshold: Duration,
    pub wait_timeout: Duration,
    /// Preserve the existing REST/WS synchronization window around delta snapshots.
    pub snapshot_buffer_delay: Duration,
    /// Bounds connection establishment and waiting for book updates. Session heartbeats
    /// remain adapter-owned; a healthy socket with no book data is retried after this bound.
    pub inactivity_timeout: Duration,
}
impl Default for WsOrderbookConfig {
    fn default() -> Self {
        Self {
            max_symbols_per_connection: DEFAULT_MAX_SYMBOLS_PER_CONNECTION,
            with_delta: false,
            reconnect_delay: Duration::from_secs(5),
            subscribe_debounce: Duration::from_millis(500),
            staleness_threshold: Duration::from_secs(5),
            wait_timeout: Duration::from_secs(30),
            snapshot_buffer_delay: Duration::from_secs(1),
            inactivity_timeout: Duration::from_secs(90),
        }
    }
}

#[derive(Default)]
struct SymbolState {
    generation: u64,
    connected: bool,
    ready: bool,
    book: Option<(MultiResolutionOrderbook, Instant)>,
    refill: Arc<Mutex<()>>,
}
struct Shared {
    symbols: Mutex<HashMap<String, SymbolState>>,
    reconstruction: OrderbookManager,
    changed: Notify,
    config: WsOrderbookConfig,
}
impl Shared {
    async fn reset(&self, symbols: &[String]) {
        let mut states = self.symbols.lock().await;
        for symbol in symbols {
            let state = states.entry(symbol.clone()).or_default();
            state.generation += 1;
            state.connected = false;
            state.ready = false;
            state.book = None;
            if self.config.with_delta {
                self.reconstruction
                    .initialize_empty_orderbook(symbol.clone())
                    .await;
            }
        }
    }
}

enum Command {
    Subscribe(Vec<String>),
    Prewarm(Vec<String>, oneshot::Sender<()>),
}

pub struct WsOrderbookManager {
    shared: Arc<Shared>,
    tx: mpsc::UnboundedSender<Command>,
    driver: parking_lot::Mutex<Driver>,
}
struct Driver {
    task: Option<JoinHandle<()>>,
    receiver: Option<mpsc::UnboundedReceiver<Command>>,
    adapter: Arc<dyn WsOrderbookAdapter>,
}
impl Drop for WsOrderbookManager {
    fn drop(&mut self) {
        if let Some(task) = self.driver.get_mut().task.take() {
            task.abort();
        }
    }
}
impl WsOrderbookManager {
    pub fn new(
        adapter: Arc<dyn WsOrderbookAdapter>,
        config: WsOrderbookConfig,
        initial_symbols: Vec<String>,
    ) -> Self {
        assert!(
            config.max_symbols_per_connection > 0,
            "shard capacity must be positive"
        );
        let name = adapter.exchange_name().to_string();
        let reconstruction_config = match name.as_str() {
            "binance" | "aster" => OrderbookManagerConfig::for_binance_aster(),
            "kucoin" => OrderbookManagerConfig::for_kucoin(),
            "extended" => OrderbookManagerConfig::for_extended(),
            "nado" => OrderbookManagerConfig::for_nado(),
            _ => OrderbookManagerConfig::default(),
        };
        let shared = Arc::new(Shared {
            symbols: Mutex::new(HashMap::new()),
            reconstruction: OrderbookManager::new(name, reconstruction_config),
            changed: Notify::new(),
            config,
        });
        let (tx, rx) = mpsc::unbounded_channel();
        let manager = Self {
            shared,
            tx,
            driver: parking_lot::Mutex::new(Driver {
                task: None,
                receiver: Some(rx),
                adapter,
            }),
        };
        if !initial_symbols.is_empty() {
            manager.start_driver(initial_symbols);
        }
        manager
    }

    fn start_driver(&self, initial: Vec<String>) {
        let mut state = self.driver.lock();
        if let Some(receiver) = state.receiver.take() {
            state.task = Some(tokio::spawn(driver(
                state.adapter.clone(),
                self.shared.clone(),
                receiver,
                initial,
            )));
        }
    }

    /// Batch registration bypasses debounce; returns once shard assignment is installed.
    pub async fn prewarm(&self, symbols: Vec<String>) -> Result<()> {
        if symbols.is_empty() {
            return Ok(());
        }
        self.start_driver(vec![]);
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Command::Prewarm(symbols, tx))
            .map_err(|_| anyhow!("orderbook driver stopped"))?;
        rx.await.map_err(|_| anyhow!("orderbook driver stopped"))
    }
    pub async fn subscribe(&self, symbol: impl Into<String>) -> Result<()> {
        self.start_driver(vec![]);
        self.tx
            .send(Command::Subscribe(vec![symbol.into()]))
            .map_err(|_| anyhow!("orderbook driver stopped"))
    }
    pub async fn get_multi(&self, symbol: &str, depth: u32) -> Option<MultiResolutionOrderbook> {
        let states = self.shared.symbols.lock().await;
        let state = states.get(symbol)?;
        if !state.connected || !state.ready {
            return None;
        }
        let (book, received) = state.book.as_ref()?;
        if received.elapsed() >= self.shared.config.staleness_threshold {
            return None;
        }
        // Checked live at read time rather than latched when detected: a stale price
        // level (no gap signal on exchanges like Extended, previous_id always 0) can
        // resolve itself on a later delta just as easily as it appeared. Treating a
        // currently-crossed book as a cache miss forces this call through to the REST
        // fallback immediately, without permanently pinning the symbol to REST if the
        // WS stream had already self-corrected by the time of the next read.
        if self.shared.config.with_delta && self.shared.reconstruction.is_crossed(symbol).await {
            return None;
        }
        let mut book = book.clone();
        if depth > 0 {
            for book in &mut book.orderbooks {
                book.bids.truncate(depth as usize);
                book.asks.truncate(depth as usize);
            }
        }
        Some(book)
    }
    pub async fn get(&self, symbol: &str, depth: u32) -> Option<Orderbook> {
        self.get_multi(symbol, depth)
            .await?
            .orderbooks
            .into_iter()
            .next()
    }
    pub async fn wait_for_orderbook(
        &self,
        symbol: &str,
        depth: u32,
    ) -> Result<MultiResolutionOrderbook> {
        self.subscribe(symbol).await?;
        tokio::time::timeout(self.shared.config.wait_timeout, async {
            loop {
                // Register before checking the cache to avoid losing a notification.
                let notified = self.shared.changed.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if let Some(book) = self.get_multi(symbol, depth).await {
                    return book;
                }
                notified.await;
            }
        })
        .await
        .map_err(|_| anyhow!("Timeout waiting for orderbook data for {symbol}"))
    }

    /// REST fetches run outside the state lock so WS deltas continue buffering. A snapshot
    /// from an older connection generation is returned to its caller but never cached.
    pub async fn get_orderbook<F, Fut>(
        &self,
        symbol: &str,
        depth: u32,
        fallback: F,
    ) -> Result<Orderbook>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<(Orderbook, u64)>>,
    {
        if let Some(book) = self.get(symbol, depth).await {
            return Ok(book);
        }
        let refill = self
            .shared
            .symbols
            .lock()
            .await
            .entry(symbol.into())
            .or_default()
            .refill
            .clone();
        let _refill = refill.lock().await;
        if let Some(book) = self.get(symbol, depth).await {
            return Ok(book);
        }
        if self.shared.config.with_delta {
            tokio::time::sleep(self.shared.config.snapshot_buffer_delay).await;
            // A stream-provided snapshot may have arrived (e.g. Extended).
            if let Some(book) = self.get(symbol, depth).await {
                return Ok(book);
            }
        }
        let (generation, started_connected) = {
            let states = self.shared.symbols.lock().await;
            let state = states.get(symbol).unwrap();
            (state.generation, state.connected)
        };
        let (mut snapshot, sequence) = fallback().await?;
        if self.shared.config.with_delta {
            // Let the stream buffer the event spanning lastUpdateId before replay.
            // This was part of the old REST fallback contract and matters for 500ms feeds.
            tokio::time::sleep(self.shared.config.snapshot_buffer_delay).await;
        }
        let mut states = self.shared.symbols.lock().await;
        let state = states.get_mut(symbol).unwrap();
        if started_connected
            && state.generation == generation
            && state.connected
            && self.shared.config.with_delta
        {
            match self
                .shared
                .reconstruction
                .apply_snapshot(
                    symbol,
                    snapshot.bids.clone(),
                    snapshot.asks.clone(),
                    sequence,
                )
                .await
            {
                Ok(_) => {
                    state.ready = true;
                    if let Some(book) = self.shared.reconstruction.get_orderbook(symbol, 0).await {
                        state.book =
                            Some((MultiResolutionOrderbook::from_single(book), Instant::now()));
                        self.shared.changed.notify_waiters();
                    }
                }
                Err(error) => {
                    state.ready = false;
                    state.book = None;
                    self.shared
                        .reconstruction
                        .initialize_empty_orderbook(symbol.into())
                        .await;
                    tracing::warn!(%symbol, %error, "orderbook snapshot replay failed; next read will resync");
                }
            }
        }
        if depth > 0 {
            snapshot.bids.truncate(depth as usize);
            snapshot.asks.truncate(depth as usize);
        }
        Ok(snapshot)
    }
}

struct Shard {
    symbols: Vec<String>,
    task: Option<JoinHandle<()>>,
}
impl Drop for Shard {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
async fn assign(
    adapter: &Arc<dyn WsOrderbookAdapter>,
    shared: &Arc<Shared>,
    shards: &mut Vec<Shard>,
    known: &mut HashSet<String>,
    symbols: Vec<String>,
) {
    let mut changed = HashSet::new();
    for symbol in symbols {
        if !known.insert(symbol.clone()) {
            continue;
        }
        if shards.last().map_or(true, |s| {
            s.symbols.len() == shared.config.max_symbols_per_connection
        }) {
            shards.push(Shard {
                symbols: vec![],
                task: None,
            });
        }
        let index = shards.len() - 1;
        shards[index].symbols.push(symbol);
        changed.insert(index);
    }
    let mut changed: Vec<_> = changed.into_iter().collect();
    changed.sort_unstable();
    for index in changed {
        let shard = &mut shards[index];
        if let Some(task) = shard.task.take() {
            task.abort();
            // Wait for cancellation before resetting state: the old session must never
            // publish into the replacement generation, even on a multi-thread runtime.
            let _ = task.await;
        }
        // Reset before returning the prewarm acknowledgement; stale data cannot be served
        // while the replacement task waits to be scheduled.
        shared.reset(&shard.symbols).await;
        tracing::info!(
            exchange = adapter.exchange_name(),
            shard = index,
            count = shard.symbols.len(),
            "orderbook shard assigned"
        );
        shard.task = Some(tokio::spawn(run_shard(
            adapter.clone(),
            shared.clone(),
            shard.symbols.clone(),
            index,
        )));
    }
}
async fn driver(
    adapter: Arc<dyn WsOrderbookAdapter>,
    shared: Arc<Shared>,
    mut rx: mpsc::UnboundedReceiver<Command>,
    initial: Vec<String>,
) {
    let mut shards = Vec::new();
    let mut known = HashSet::new();
    assign(&adapter, &shared, &mut shards, &mut known, initial).await;
    let mut pending = Vec::new();
    let mut deadline = None;
    loop {
        let sleep_until =
            deadline.unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_secs(86400));
        tokio::select! {
            command = rx.recv() => match command {
                Some(Command::Subscribe(symbols)) => {
                    for symbol in symbols {
                        if !known.contains(&symbol) && !pending.contains(&symbol) {
                            pending.push(symbol);
                            deadline = Some(tokio::time::Instant::now() + shared.config.subscribe_debounce);
                        }
                    }
                }
                Some(Command::Prewarm(symbols, ack)) => {
                    pending.extend(symbols);
                    assign(&adapter, &shared, &mut shards, &mut known, std::mem::take(&mut pending)).await;
                    deadline = None;
                    let _ = ack.send(());
                }
                None => break,
            },
            _ = tokio::time::sleep_until(sleep_until), if deadline.is_some() => {
                assign(&adapter, &shared, &mut shards, &mut known, std::mem::take(&mut pending)).await;
                deadline = None;
            }

        }
    }
}
async fn run_shard(
    adapter: Arc<dyn WsOrderbookAdapter>,
    shared: Arc<Shared>,
    symbols: Vec<String>,
    shard: usize,
) {
    loop {
        let result = run_session(&adapter, &shared, &symbols).await;
        shared.reset(&symbols).await;
        tracing::warn!(exchange = adapter.exchange_name(), shard, error = ?result.err(), "orderbook shard disconnected; retrying");
        tokio::time::sleep(shared.config.reconnect_delay).await;
    }
}
async fn run_session(
    adapter: &Arc<dyn WsOrderbookAdapter>,
    shared: &Arc<Shared>,
    symbols: &[String],
) -> Result<()> {
    let mut stream = tokio::time::timeout(
        shared.config.inactivity_timeout,
        adapter.connect_and_subscribe(symbols.to_vec()),
    )
    .await??;
    {
        let mut states = shared.symbols.lock().await;
        for symbol in symbols {
            states.get_mut(symbol).unwrap().connected = true;
        }
    }
    tracing::info!(
        exchange = adapter.exchange_name(),
        count = symbols.len(),
        "orderbook shard connected"
    );
    loop {
        let update = tokio::time::timeout(shared.config.inactivity_timeout, stream.next())
            .await?
            .ok_or_else(|| anyhow!("orderbook stream ended"))??;
        let symbol = match &update {
            OrderbookUpdate::Full(book) => &book.symbol,
            OrderbookUpdate::Delta(delta) => &delta.symbol,
        };
        if !symbols.contains(symbol) {
            return Err(anyhow!("adapter emitted unsubscribed symbol {symbol}"));
        }
        let mut states = shared.symbols.lock().await;
        let state = states.get_mut(symbol).unwrap();
        match update {
            OrderbookUpdate::Full(book) => {
                if shared.config.with_delta {
                    return Err(anyhow!("full update from delta adapter"));
                }
                state.ready = true;
                state.book = Some((book, Instant::now()));
            }
            OrderbookUpdate::Delta(delta) => {
                if !shared.config.with_delta {
                    return Err(anyhow!("delta update from full adapter"));
                }
                if delta.is_snapshot {
                    shared
                        .reconstruction
                        .apply_snapshot(
                            &delta.symbol,
                            delta.bids,
                            delta.asks,
                            delta.final_update_id,
                        )
                        .await?;
                    state.ready = true;
                } else {
                    shared
                        .reconstruction
                        .apply_update(
                            &delta.symbol,
                            delta.first_update_id,
                            delta.final_update_id,
                            delta.previous_id,
                            delta.bids,
                            delta.asks,
                            adapter.is_incremental_delta(),
                        )
                        .await?;
                }
                if state.ready {
                    if let Some(book) = shared.reconstruction.get_orderbook(&delta.symbol, 0).await
                    {
                        if state
                            .book
                            .as_ref()
                            .map_or(true, |(previous, _)| previous.timestamp != book.timestamp)
                        {
                            state.book =
                                Some((MultiResolutionOrderbook::from_single(book), Instant::now()));
                        }
                    }
                }
            }
        }
        shared.changed.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OrderbookLevel;
    use rust_decimal::Decimal;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Connection {
        symbols: Vec<String>,
        updates: mpsc::UnboundedSender<Result<OrderbookUpdate>>,
    }
    struct FakeAdapter {
        opened: mpsc::UnboundedSender<Connection>,
        dropped: Arc<AtomicUsize>,
    }
    struct DropCount(Arc<AtomicUsize>);
    impl Drop for DropCount {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    #[async_trait]
    impl WsOrderbookAdapter for FakeAdapter {
        fn exchange_name(&self) -> &str {
            "binance"
        }
        async fn connect_and_subscribe(
            &self,
            symbols: Vec<String>,
        ) -> Result<DataStream<OrderbookUpdate>> {
            let (tx, rx) = mpsc::unbounded_channel();
            self.opened
                .send(Connection {
                    symbols,
                    updates: tx,
                })
                .unwrap();
            let guard = DropCount(self.dropped.clone());
            Ok(Box::pin(futures::stream::unfold(
                (rx, guard),
                |(mut rx, guard)| async move { rx.recv().await.map(|value| (value, (rx, guard))) },
            )))
        }
    }
    fn setup(
        delta: bool,
        initial: &[&str],
    ) -> (
        WsOrderbookManager,
        mpsc::UnboundedReceiver<Connection>,
        Arc<AtomicUsize>,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        let dropped = Arc::new(AtomicUsize::new(0));
        let manager = WsOrderbookManager::new(
            Arc::new(FakeAdapter {
                opened: tx,
                dropped: dropped.clone(),
            }),
            WsOrderbookConfig {
                max_symbols_per_connection: 2,
                with_delta: delta,
                snapshot_buffer_delay: Duration::ZERO,
                subscribe_debounce: Duration::from_millis(20),
                reconnect_delay: Duration::from_millis(20),
                staleness_threshold: Duration::from_secs(5),
                ..Default::default()
            },
            initial.iter().map(|s| s.to_string()).collect(),
        );
        (manager, rx, dropped)
    }
    fn book(symbol: &str, quantity: i64) -> Orderbook {
        Orderbook {
            symbol: symbol.into(),
            timestamp: chrono::Utc::now(),
            bids: vec![OrderbookLevel {
                price: Decimal::from(100),
                quantity: Decimal::from(quantity),
            }],
            asks: vec![OrderbookLevel {
                price: Decimal::from(101),
                quantity: Decimal::ONE,
            }],
        }
    }
    fn full(symbol: &str) -> Result<OrderbookUpdate> {
        Ok(OrderbookUpdate::Full(
            MultiResolutionOrderbook::from_single(book(symbol, 1)),
        ))
    }
    fn delta(symbol: &str, sequence: u64, previous: u64, quantity: i64) -> Result<OrderbookUpdate> {
        Ok(OrderbookUpdate::Delta(DepthUpdate {
            symbol: symbol.into(),
            first_update_id: sequence,
            final_update_id: sequence,
            previous_id: previous,
            bids: book(symbol, quantity).bids,
            asks: vec![],
            is_snapshot: false,
        }))
    }
    async fn settle() {
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
    }
    async fn connection(rx: &mut mpsc::UnboundedReceiver<Connection>) -> Connection {
        tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap()
    }

    struct ControlConnection {
        symbol: String,
        incoming: mpsc::UnboundedSender<()>,
        outgoing: mpsc::UnboundedReceiver<usize>,
    }
    struct ControlAdapter(mpsc::UnboundedSender<ControlConnection>);
    #[async_trait]
    impl WsOrderbookAdapter for ControlAdapter {
        fn exchange_name(&self) -> &str {
            "control-test"
        }
        async fn connect_and_subscribe(
            &self,
            symbols: Vec<String>,
        ) -> Result<DataStream<OrderbookUpdate>> {
            let (incoming, mut input) = mpsc::unbounded_channel();
            let (output, outgoing) = mpsc::unbounded_channel();
            self.0
                .send(ControlConnection {
                    symbol: symbols[0].clone(),
                    incoming,
                    outgoing,
                })
                .unwrap();
            // This owned stream models a protocol session writing pongs on its own socket.
            // Its local counter survives control frames and starts over for a new connection.
            Ok(Box::pin(futures::stream::once(async move {
                let mut count = 0;
                while input.recv().await.is_some() {
                    count += 1;
                    output.send(count).unwrap();
                }
                Err(anyhow!("control socket closed"))
            })))
        }
    }

    #[tokio::test]
    async fn session_control_writes_are_isolated_and_state_resets_on_reconnect() {
        let (tx, mut opened) = mpsc::unbounded_channel();
        let _manager = WsOrderbookManager::new(
            Arc::new(ControlAdapter(tx)),
            WsOrderbookConfig {
                max_symbols_per_connection: 1,
                reconnect_delay: Duration::from_millis(10),
                ..Default::default()
            },
            vec!["A".into(), "B".into()],
        );
        let mut a = opened.recv().await.unwrap();
        let mut b = opened.recv().await.unwrap();
        assert_eq!(a.symbol, "A");
        assert_eq!(b.symbol, "B");
        a.incoming.send(()).unwrap();
        assert_eq!(a.outgoing.recv().await, Some(1));
        a.incoming.send(()).unwrap();
        assert_eq!(a.outgoing.recv().await, Some(2));
        assert!(b.outgoing.try_recv().is_err());
        b.incoming.send(()).unwrap();
        assert_eq!(b.outgoing.recv().await, Some(1));
        drop(a.incoming);
        let mut replacement = opened.recv().await.unwrap();
        assert_eq!(replacement.symbol, "A");
        replacement.incoming.send(()).unwrap();
        assert_eq!(replacement.outgoing.recv().await, Some(1));
    }

    #[test]
    fn empty_construction_needs_no_runtime() {
        let (_manager, mut opened, dropped) = setup(false, &[]);
        assert!(opened.try_recv().is_err());
        assert_eq!(dropped.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn eager_shards_are_bounded_deduplicated_and_stable() {
        let (manager, mut rx, dropped) = setup(false, &["A", "B", "C", "A"]);
        let a = connection(&mut rx).await;
        let b = connection(&mut rx).await;
        assert_eq!(a.symbols, ["A", "B"]);
        assert_eq!(b.symbols, ["C"]);
        manager
            .prewarm(vec!["A".into(), "D".into(), "E".into()])
            .await
            .unwrap();
        let c = connection(&mut rx).await;
        let d = connection(&mut rx).await;
        assert_eq!(c.symbols, ["C", "D"]);
        assert_eq!(d.symbols, ["E"]);
        settle().await;
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        a.updates.send(full("A")).unwrap();
        settle().await;
        assert!(manager.get("A", 0).await.is_some());
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn construction_is_inert_and_lazy_subscriptions_batch() {
        let (manager, mut rx, _) = setup(false, &[]);
        settle().await;
        assert!(rx.try_recv().is_err());
        for symbol in ["A", "B", "A", "C"] {
            manager.subscribe(symbol).await.unwrap();
        }
        let a = connection(&mut rx).await;
        let b = connection(&mut rx).await;
        assert_eq!(a.symbols, ["A", "B"]);
        assert_eq!(b.symbols, ["C"]);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn reconnect_invalidates_only_failed_shard_and_resets_session() {
        let (manager, mut rx, dropped) = setup(false, &["A", "B", "C"]);
        let a = connection(&mut rx).await;
        let b = connection(&mut rx).await;
        a.updates.send(full("A")).unwrap();
        b.updates.send(full("C")).unwrap();
        settle().await;
        a.updates.send(Err(anyhow!("disconnected"))).unwrap();
        settle().await;
        assert!(manager.get("A", 0).await.is_none());
        assert!(manager.get("C", 0).await.is_some());
        let retry = connection(&mut rx).await;
        assert_eq!(retry.symbols, ["A", "B"]);
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        assert!(manager.get("A", 0).await.is_none());
        retry.updates.send(full("A")).unwrap();
        assert!(manager.wait_for_orderbook("A", 1).await.is_ok());
    }

    #[tokio::test]
    async fn delta_startup_replays_buffer_and_fresh_hit_skips_rest() {
        let (manager, mut rx, _) = setup(true, &["A"]);
        let session = connection(&mut rx).await;
        session.updates.send(delta("A", 10, 9, 3)).unwrap();
        session.updates.send(delta("A", 11, 10, 4)).unwrap();
        settle().await;
        assert!(manager.get("A", 0).await.is_none());
        manager
            .get_orderbook("A", 0, || async { Ok((book("A", 1), 10)) })
            .await
            .unwrap();
        let cached = manager
            .get_orderbook("A", 1, || async { panic!("unexpected fallback") })
            .await
            .unwrap();
        assert_eq!(cached.bids[0].quantity, Decimal::from(4));
        session.updates.send(delta("A", 12, 11, 0)).unwrap();
        settle().await;
        assert!(manager.get("A", 0).await.unwrap().bids.is_empty());
    }

    #[tokio::test]
    async fn missed_delta_requires_new_snapshot_after_reconnect() {
        let (manager, mut rx, _) = setup(true, &["A"]);
        let a = connection(&mut rx).await;
        settle().await;
        manager
            .get_orderbook("A", 0, || async { Ok((book("A", 1), 10)) })
            .await
            .unwrap();
        a.updates.send(delta("A", 13, 12, 9)).unwrap();
        settle().await;
        assert!(manager.get("A", 0).await.is_none());
        let retry = connection(&mut rx).await;
        retry.updates.send(delta("A", 20, 19, 7)).unwrap();
        settle().await;
        assert!(manager.get("A", 0).await.is_none());
        manager
            .get_orderbook("A", 0, || async { Ok((book("A", 5), 20)) })
            .await
            .unwrap();
        retry.updates.send(delta("A", 21, 20, 8)).unwrap();
        settle().await;
        assert_eq!(
            manager.get("A", 0).await.unwrap().bids[0].quantity,
            Decimal::from(8)
        );
    }

    #[tokio::test]
    async fn snapshot_from_old_connection_cannot_initialize_replacement() {
        let (manager, mut rx, _) = setup(true, &["A"]);
        let session = connection(&mut rx).await;
        let manager = Arc::new(manager);
        let (started_tx, started_rx) = oneshot::channel();
        let (finish_tx, finish_rx) = oneshot::channel();
        let reader = manager.clone();
        let pending = tokio::spawn(async move {
            reader
                .get_orderbook("A", 0, || async {
                    started_tx.send(()).unwrap();
                    finish_rx.await.unwrap();
                    Ok((book("A", 1), 10))
                })
                .await
                .unwrap()
        });
        started_rx.await.unwrap();
        session.updates.send(Err(anyhow!("disconnect"))).unwrap();
        let _new_session = connection(&mut rx).await;
        finish_tx.send(()).unwrap();
        pending.await.unwrap();
        assert!(manager.get("A", 0).await.is_none());
        manager
            .get_orderbook("A", 0, || async { Ok((book("A", 8), 30)) })
            .await
            .unwrap();
        assert_eq!(
            manager.get("A", 0).await.unwrap().bids[0].quantity,
            Decimal::from(8)
        );
    }

    #[tokio::test]
    async fn concurrent_reads_share_snapshot_refill_and_depth_zero_keeps_levels() {
        let (manager, mut rx, _) = setup(true, &["A"]);
        let _session = connection(&mut rx).await;
        settle().await;
        let calls = AtomicUsize::new(0);
        let fetch = || async {
            calls.fetch_add(1, Ordering::SeqCst);
            tokio::task::yield_now().await;
            Ok((book("A", 3), 10))
        };
        let (a, b) = tokio::join!(
            manager.get_orderbook("A", 0, fetch),
            manager.get_orderbook("A", 0, fetch)
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(a.unwrap().bids.len(), 1);
        assert_eq!(b.unwrap().asks.len(), 1);
    }

    #[tokio::test]
    async fn cache_ttl_and_manager_drop_release_sessions() {
        let (manager, mut rx, dropped) = setup(false, &["A", "B", "C"]);
        let a = connection(&mut rx).await;
        let _b = connection(&mut rx).await;
        a.updates.send(full("A")).unwrap();
        settle().await;
        assert!(manager.get("A", 0).await.is_some());
        manager
            .shared
            .symbols
            .lock()
            .await
            .get_mut("A")
            .unwrap()
            .book
            .as_mut()
            .unwrap()
            .1 = Instant::now() - Duration::from_secs(6);
        assert!(manager.get("A", 0).await.is_none());
        drop(manager);
        settle().await;
        assert_eq!(dropped.load(Ordering::SeqCst), 2);
    }
}
