//! Push-based orderbook cache for exchanges whose `IPerpsStream::stream_orderbooks` already
//! emits ready-to-use, fully-merged `Orderbook`s - no external delta/replay logic needed.
//!
//! `StreamManager`/`OrderbookManager` assume a Binance-style protocol: buffer WS deltas,
//! fetch a REST snapshot with a `lastUpdateId`, replay only the deltas past it, gap-detect on
//! a numeric update-id sequence. Exchanges like EdgeX and Arcus don't work that way - their WS
//! client already maintains the local book internally (see
//! `crates/perps-exchanges/src/edgex/ws_client.rs`'s `DepthBook`) and yields a complete,
//! correctly-sorted `Orderbook` on every update. Forcing that through `OrderbookManager` would
//! mean synthesizing a fake update-id and a snapshot-reconciliation step that serves no
//! purpose. This cache just stores whatever `Orderbook` the WS client yields, keyed by symbol.
//!
//! All subscribed symbols share **one** underlying `stream_orderbooks` connection (that call
//! already accepts a symbol list and multiplexes it over a single WebSocket - see
//! `EdgexWsClient`/`ArcusWsClient`'s `connect_and_subscribe`). A background driver task debounces
//! `subscribe` calls (`SUBSCRIBE_DEBOUNCE`) so a burst of symbols registered close together (e.g.
//! `start` warming up its whole symbol list) collapses into a single connect instead of one
//! connection per symbol; a symbol arriving after the desired set has gone quiet triggers a
//! reconnect covering the old set plus the new symbol.

use crate::streaming::IPerpsStream;
use crate::types::Orderbook;
use futures::StreamExt;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, RwLock};
use tokio::task::JoinHandle;

/// Delay before re-opening a dropped `stream_orderbooks` connection. Fixed, not
/// exponential-backoff: this cache runs for the lifetime of a long-lived process (`start`),
/// where a fast, simple retry matters more than backing off from a transient blip.
const RECONNECT_DELAY: Duration = Duration::from_secs(5);

/// Quiet period the driver waits for after a subscribed symbol before (re)connecting. Reset on
/// every new symbol, so a whole burst (e.g. `start` iterating its symbol list at startup) is
/// batched into one `stream_orderbooks` call instead of reconnecting once per symbol.
const SUBSCRIBE_DEBOUNCE: Duration = Duration::from_millis(500);

/// Push-based orderbook cache backed by a single shared `IPerpsStream::stream_orderbooks`
/// connection covering every subscribed symbol. `symbol` keys are whatever the caller passes to
/// `subscribe`/`get` - callers are responsible for using one canonical form (e.g. the client's
/// own `normalize_symbol`) consistently.
pub struct OrderbookPushCache {
    cache: Arc<RwLock<HashMap<String, Orderbook>>>,
    tx: mpsc::UnboundedSender<String>,
}

impl OrderbookPushCache {
    pub fn new(ws: Arc<dyn IPerpsStream>) -> Self {
        let cache = Arc::new(RwLock::new(HashMap::new()));
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(driver(ws, cache.clone(), rx));
        Self { cache, tx }
    }

    /// Registers `symbol` for streaming. Cheap, non-blocking, and never fails: it just enqueues
    /// the symbol for the background driver (see module docs), which debounces and batches it
    /// with whatever else is subscribed into the single shared connection. Repeat calls for a
    /// symbol already covered by the current connection are a harmless no-op once the driver
    /// dedupes them.
    pub async fn subscribe(&self, symbol: &str) {
        let _ = self.tx.send(symbol.to_string());
    }

    /// Cached orderbook for `symbol`, truncated to `depth` levels per side (`0` = whatever the
    /// WS client's own native depth is, untruncated - matches every REST `get_orderbook`'s
    /// `depth == 0` convention). `None` on a cache miss (not yet subscribed, or no frame has
    /// arrived yet) - callers should fall back to a REST fetch in that case.
    pub async fn get(&self, symbol: &str, depth: u32) -> Option<Orderbook> {
        let mut orderbook = self.cache.read().await.get(symbol).cloned()?;
        if depth > 0 {
            orderbook.bids.truncate(depth as usize);
            orderbook.asks.truncate(depth as usize);
        }
        Some(orderbook)
    }
}

/// Owns the cache's single logical connection: accumulates subscribed symbols (debounced) and,
/// whenever the desired set grows, aborts whatever connection is currently running and restarts
/// one `stream_orderbooks` call covering every symbol subscribed so far.
async fn driver(
    ws: Arc<dyn IPerpsStream>,
    cache: Arc<RwLock<HashMap<String, Orderbook>>>,
    mut rx: mpsc::UnboundedReceiver<String>,
) {
    let mut symbols: HashSet<String> = HashSet::new();
    let mut current: Option<JoinHandle<()>> = None;

    loop {
        let first = match rx.recv().await {
            Some(symbol) => symbol,
            None => return,
        };
        let mut changed = symbols.insert(first);

        // Drain further arrivals, resetting the quiet-period clock each time, so a whole burst
        // of subscribes lands in the same batch.
        loop {
            match tokio::time::timeout(SUBSCRIBE_DEBOUNCE, rx.recv()).await {
                Ok(Some(symbol)) => {
                    if symbols.insert(symbol) {
                        changed = true;
                    }
                }
                Ok(None) => break,
                Err(_elapsed) => break,
            }
        }

        if !changed {
            continue;
        }

        if let Some(task) = current.take() {
            task.abort();
        }

        let snapshot: Vec<String> = symbols.iter().cloned().collect();
        tracing::info!(
            count = snapshot.len(),
            "orderbook push-cache: (re)connecting with {} symbol(s)",
            snapshot.len()
        );
        current = Some(tokio::spawn(run_connection(
            ws.clone(),
            cache.clone(),
            snapshot,
        )));
    }
}

/// Runs one `stream_orderbooks` connection for `symbols` until it errors or ends, then
/// reconnects after `RECONNECT_DELAY` - forever, until the task is aborted by the driver (when
/// the desired symbol set changes).
async fn run_connection(
    ws: Arc<dyn IPerpsStream>,
    cache: Arc<RwLock<HashMap<String, Orderbook>>>,
    symbols: Vec<String>,
) {
    loop {
        match ws.stream_orderbooks(symbols.clone()).await {
            Ok(mut stream) => {
                tracing::debug!(symbols = ?symbols, "orderbook push-cache connected");
                while let Some(item) = stream.next().await {
                    match item {
                        Ok(orderbook) => {
                            cache.write().await.insert(orderbook.symbol.clone(), orderbook);
                        }
                        Err(error) => {
                            tracing::warn!(symbols = ?symbols, "orderbook push-cache stream error: {}", error);
                            break;
                        }
                    }
                }
            }
            Err(error) => {
                tracing::warn!(symbols = ?symbols, "orderbook push-cache failed to connect: {}", error);
            }
        }
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streaming::{DataStream, StreamConfig, StreamEvent};
    use crate::types::OrderbookLevel;
    use async_trait::async_trait;
    use chrono::Utc;
    use rust_decimal::Decimal;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeWs {
        calls: Arc<AtomicUsize>,
    }

    fn fake_orderbook(symbol: &str, n: usize) -> Orderbook {
        Orderbook {
            symbol: symbol.to_string(),
            bids: (0..n)
                .map(|i| OrderbookLevel {
                    price: Decimal::from(100 - i as i64),
                    quantity: Decimal::ONE,
                })
                .collect(),
            asks: (0..n)
                .map(|i| OrderbookLevel {
                    price: Decimal::from(101 + i as i64),
                    quantity: Decimal::ONE,
                })
                .collect(),
            timestamp: Utc::now(),
        }
    }

    #[async_trait]
    impl IPerpsStream for FakeWs {
        fn get_name(&self) -> &str {
            "fake"
        }
        async fn stream_tickers(
            &self,
            _symbols: Vec<String>,
        ) -> anyhow::Result<DataStream<crate::types::Ticker>> {
            unimplemented!()
        }
        async fn stream_trades(
            &self,
            _symbols: Vec<String>,
        ) -> anyhow::Result<DataStream<crate::types::Trade>> {
            unimplemented!()
        }
        async fn stream_orderbooks(&self, symbols: Vec<String>) -> anyhow::Result<DataStream<Orderbook>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let books: Vec<anyhow::Result<Orderbook>> = symbols
                .iter()
                .map(|s| Ok(fake_orderbook(s, 5)))
                .collect();
            Ok(Box::pin(futures::stream::iter(books)))
        }
        async fn stream_multi(&self, _config: StreamConfig) -> anyhow::Result<DataStream<StreamEvent>> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn repeat_subscribes_for_one_symbol_share_a_single_connection() {
        let calls = Arc::new(AtomicUsize::new(0));
        let ws: Arc<dyn IPerpsStream> = Arc::new(FakeWs { calls: calls.clone() });
        let cache = OrderbookPushCache::new(ws);

        cache.subscribe("BTC").await;
        cache.subscribe("BTC").await;
        cache.subscribe("BTC").await;
        // Past the debounce window: driver should have connected exactly once.
        tokio::time::sleep(SUBSCRIBE_DEBOUNCE + Duration::from_millis(200)).await;

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(cache.get("BTC", 0).await.is_some());
    }

    #[tokio::test]
    async fn symbols_subscribed_within_debounce_window_batch_into_one_connection() {
        let calls = Arc::new(AtomicUsize::new(0));
        let ws: Arc<dyn IPerpsStream> = Arc::new(FakeWs { calls: calls.clone() });
        let cache = OrderbookPushCache::new(ws);

        cache.subscribe("BTC").await;
        cache.subscribe("ETH").await;
        cache.subscribe("SOL").await;
        tokio::time::sleep(SUBSCRIBE_DEBOUNCE + Duration::from_millis(200)).await;

        // All three symbols arrived well within one debounce window: a single
        // `stream_orderbooks` call should cover all of them, not one connection each.
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(cache.get("BTC", 0).await.is_some());
        assert!(cache.get("ETH", 0).await.is_some());
        assert!(cache.get("SOL", 0).await.is_some());
    }

    #[tokio::test]
    async fn symbol_after_quiet_period_triggers_reconnect_with_full_set() {
        let calls = Arc::new(AtomicUsize::new(0));
        let ws: Arc<dyn IPerpsStream> = Arc::new(FakeWs { calls: calls.clone() });
        let cache = OrderbookPushCache::new(ws);

        cache.subscribe("BTC").await;
        tokio::time::sleep(SUBSCRIBE_DEBOUNCE + Duration::from_millis(200)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // A symbol arriving after the previous batch already went quiet reconnects.
        cache.subscribe("ETH").await;
        tokio::time::sleep(SUBSCRIBE_DEBOUNCE + Duration::from_millis(200)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(cache.get("BTC", 0).await.is_some());
        assert!(cache.get("ETH", 0).await.is_some());
    }

    #[tokio::test]
    async fn get_truncates_to_requested_depth() {
        let calls = Arc::new(AtomicUsize::new(0));
        let ws: Arc<dyn IPerpsStream> = Arc::new(FakeWs { calls });
        let cache = OrderbookPushCache::new(ws);
        cache.subscribe("BTC").await;
        tokio::time::sleep(SUBSCRIBE_DEBOUNCE + Duration::from_millis(200)).await;

        let full = cache.get("BTC", 0).await.unwrap();
        assert_eq!(full.bids.len(), 5);

        let truncated = cache.get("BTC", 2).await.unwrap();
        assert_eq!(truncated.bids.len(), 2);
        assert_eq!(truncated.asks.len(), 2);
    }

    #[tokio::test]
    async fn get_is_none_before_any_subscribe() {
        let calls = Arc::new(AtomicUsize::new(0));
        let ws: Arc<dyn IPerpsStream> = Arc::new(FakeWs { calls });
        let cache = OrderbookPushCache::new(ws);
        assert!(cache.get("BTC", 0).await.is_none());
    }
}
