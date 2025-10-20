use crate::orderbook_manager::{OrderbookManager, OrderbookManagerConfig};
use crate::streaming::{DepthUpdate, OrderbookStreamer};
use crate::types::Orderbook;
use anyhow::{Result};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// Configuration for StreamManager behavior
#[derive(Debug, Clone)]
pub struct StreamConfig {
    /// How long to wait before considering cached data stale (default: 1s)
    pub staleness_threshold: Duration,

    /// Maximum time to wait for orderbook in wait_for_orderbook (default: 5s)
    pub wait_timeout: Duration,

    /// Polling interval for wait_for_orderbook (default: 100ms)
    pub poll_interval: Duration,

    /// Whether to auto-reconnect on WebSocket errors (default: true)
    pub auto_reconnect: bool,

    /// Delay before reconnecting after error (default: 5s)
    pub reconnect_delay: Duration,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            staleness_threshold: Duration::from_secs(1),
            wait_timeout: Duration::from_secs(5),
            poll_interval: Duration::from_millis(100),
            auto_reconnect: true,
            reconnect_delay: Duration::from_secs(5),
        }
    }
}

/// Statistics for monitoring StreamManager health
#[derive(Debug, Clone, Default)]
pub struct StreamStats {
    pub total_updates: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub rest_fallbacks: u64,
    pub errors: u64,
    pub last_update: Option<Instant>,
    pub is_connected: bool,
}

/// Manages WebSocket streaming for orderbooks with smart caching and REST fallback
///
/// StreamManager provides a simplified interface for consuming orderbook data:
/// - Subscribes to WebSocket streams for requested symbols
/// - Maintains in-memory cache of latest orderbooks
/// - Automatically falls back to REST API when cache is stale or missing
/// - Handles reconnection and error recovery
///
/// # Example
/// ```rust,no_run
/// use perps_core::stream_manager::{StreamManager, StreamConfig};
/// use perps_exchanges::aster::ws_client::AsterWsClient;
///
/// async fn example() -> anyhow::Result<()> {
///     let ws_client = Arc::new(AsterWsClient::new());
///     let manager = StreamManager::new(ws_client, StreamConfig::default());
///
///     // Subscribe to BTC orderbook
///     manager.subscribe("BTC".to_string()).await?;
///
///     // Get orderbook (uses cache if fresh, otherwise REST fallback)
///     let orderbook = manager.get_orderbook("BTC", 20, || async {
///         // REST API fallback
///         my_exchange_client.fetch_orderbook_rest("BTC", 20).await
///     }).await?;
///
///     Ok(())
/// }
/// ```
pub struct StreamManager {
    /// The OrderbookStreamer implementation (exchange-specific)
    streamer: Arc<dyn OrderbookStreamer>,

    /// Configuration
    config: StreamConfig,

    /// OrderbookManager for proper local orderbook maintenance
    orderbook_manager: Arc<OrderbookManager>,

    /// Currently subscribed symbols
    subscribed_symbols: Arc<RwLock<HashSet<String>>>,

    /// Statistics for monitoring
    stats: Arc<RwLock<StreamStats>>,

    /// Per-symbol shutdown signals
    /// Each symbol has its own shutdown channel to allow independent stream control
    shutdown_txs: Arc<RwLock<HashMap<String, tokio::sync::broadcast::Sender<()>>>>,
}

impl StreamManager {
    /// Create a new StreamManager with the given OrderbookStreamer and configuration
    pub fn new(streamer: Arc<dyn OrderbookStreamer>, config: StreamConfig) -> Self {
        let exchange_name = streamer.exchange_name().to_string();

        // Create OrderbookManager with appropriate config based on exchange
        let orderbook_config = match exchange_name.as_str() {
            "binance" | "aster" => OrderbookManagerConfig::for_binance_aster(),
            "extended" => OrderbookManagerConfig::for_extended(),
            "kucoin" => OrderbookManagerConfig::for_kucoin(),
            _ => OrderbookManagerConfig::default(),
        };

        let orderbook_manager = Arc::new(OrderbookManager::new(exchange_name, orderbook_config));

        Self {
            streamer,
            config,
            orderbook_manager,
            subscribed_symbols: Arc::new(RwLock::new(HashSet::new())),
            stats: Arc::new(RwLock::new(StreamStats::default())),
            shutdown_txs: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Subscribe to orderbook updates for a symbol (idempotent)
    ///
    /// If the symbol is already subscribed, this is a no-op.
    /// Otherwise, starts a background task that streams orderbook updates.
    ///
    /// Following Aster/Binance specification for local orderbook management:
    /// 1. Initialize empty orderbook (starts buffering WebSocket events)
    /// 2. Start WebSocket stream
    /// 3. Snapshot initialization handled by first get_orderbook() call
    pub async fn subscribe(&self, symbol: String) -> Result<()> {
        let mut subscribed = self.subscribed_symbols.write().await;

        if subscribed.contains(&symbol) {
            debug!("Symbol {} already subscribed", symbol);
            return Ok(());
        }

        info!("Subscribing to {} orderbook stream", symbol);

        // Step 1: Initialize empty orderbook for this symbol (starts buffering)
        self.orderbook_manager
            .initialize_empty_orderbook(symbol.clone())
            .await;

        // Step 2: Start streaming task for this symbol
        let stream_task = self.start_streaming_task(symbol.clone()).await?;

        // Add to subscribed set
        subscribed.insert(symbol.clone());

        // Spawn the streaming task
        tokio::spawn(stream_task);

        Ok(())
    }

    /// Check if a symbol is currently subscribed
    pub async fn is_subscribed(&self, symbol: &str) -> bool {
        let subscribed = self.subscribed_symbols.read().await;
        subscribed.contains(symbol)
    }

    /// Get orderbook from cache or REST fallback
    ///
    /// This method implements the full Aster/Binance local orderbook specification:
    /// 1. Check OrderbookManager cache (maintains local orderbook with delta updates)
    /// 2. If not initialized, fetch REST snapshot and apply to OrderbookManager
    /// 3. Return trimmed orderbook
    ///
    /// # Arguments
    /// * `symbol` - The symbol to get orderbook for
    /// * `limit` - Number of levels to return
    /// * `rest_snapshot_fallback` - Async closure to fetch orderbook snapshot with lastUpdateId via REST API
    ///   Should return (Orderbook, last_update_id)
    pub async fn get_orderbook<F, Fut>(
        &self,
        symbol: &str,
        limit: u32,
        rest_snapshot_fallback: F,
    ) -> Result<Orderbook>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<(Orderbook, u64)>>,
    {
        // Try OrderbookManager first (local orderbook maintained with delta updates)
        if let Some(cached_orderbook) = self
            .orderbook_manager
            .get_orderbook(symbol, limit as usize)
            .await
        {
            // Check if data is fresh
            let (bid_size, ask_size) = cached_orderbook.size();
            if bid_size + ask_size != 0 && self.orderbook_manager.is_fresh(symbol).await {
                debug!(
                    "OrderbookManager cache hit for {} (age: {:?})",
                    symbol,
                    chrono::Utc::now().signed_duration_since(cached_orderbook.timestamp)
                );

                let mut stats = self.stats.write().await;
                stats.cache_hits += 1;

                return Ok(cached_orderbook);
            } else {
                // OrderbookManager not initialized or stale - fetch REST snapshot
                debug!(
                    "OrderbookManager cache miss for {}, bid_size {}, ask_size {}, age {:?}",
                    symbol,
                    bid_size,
                    ask_size,
                    chrono::Utc::now().signed_duration_since(cached_orderbook.timestamp).to_string()
                );
            }
        }

        // OrderbookManager not initialized or stale - fetch REST snapshot
        debug!(
            "OrderbookManager cache miss for {} - fetching REST snapshot",
            symbol
        );

        let mut stats = self.stats.write().await;
        stats.cache_misses += 1;
        stats.rest_fallbacks += 1;
        drop(stats);

        sleep(Duration::from_millis(1000)).await;

        // Call REST fallback to get snapshot with lastUpdateId
        let (orderbook, last_update_id) = rest_snapshot_fallback().await?;

        sleep(Duration::from_millis(1000)).await;

        // Apply snapshot to OrderbookManager (will replay buffered WebSocket events)
        match self
            .orderbook_manager
            .apply_snapshot(
                symbol,
                orderbook.bids.clone(),
                orderbook.asks.clone(),
                last_update_id,
            )
            .await
        {
            Ok(replayed_count) => {
                info!(
                    "[{}] Snapshot applied for {} (lastUpdateId={}, replayed {} buffered events)",
                    self.streamer.exchange_name(),
                    symbol,
                    last_update_id,
                    replayed_count
                );
            }
            Err(e) => {
                error!(
                    "[{}] Failed to apply snapshot for {}: {}",
                    self.streamer.exchange_name(),
                    symbol,
                    e
                );
                return Err(e);
            }
        }

        // Return the snapshot (trimmed to requested limit)
        Ok(Orderbook {
            symbol: orderbook.symbol,
            bids: orderbook
                .bids
                .iter()
                .take(limit as usize)
                .cloned()
                .collect(),
            asks: orderbook
                .asks
                .iter()
                .take(limit as usize)
                .cloned()
                .collect(),
            timestamp: orderbook.timestamp,
        })
    }

    /// Wait for orderbook to be available in OrderbookManager (with timeout)
    ///
    /// Polls OrderbookManager until orderbook is initialized or timeout is reached.
    /// Useful for waiting for initial snapshot after subscription.
    pub async fn wait_for_orderbook(
        &self,
        symbol: &str,
        timeout: Option<Duration>,
    ) -> Result<Orderbook> {
        let timeout = timeout.unwrap_or(self.config.wait_timeout);
        let start = Instant::now();

        loop {
            if let Some(orderbook) = self.orderbook_manager.get_orderbook(symbol, 1000).await {
                if self.orderbook_manager.is_fresh(symbol).await {
                    return Ok(orderbook);
                }
            }

            if start.elapsed() >= timeout {
                return Err(anyhow::anyhow!(
                    "Timeout waiting for {} orderbook after {:?}",
                    symbol,
                    timeout
                ));
            }

            tokio::time::sleep(self.config.poll_interval).await;
        }
    }

    /// Get current statistics
    pub async fn stats(&self) -> StreamStats {
        self.stats.read().await.clone()
    }

    /// Shutdown all streaming tasks
    pub async fn shutdown(&self) -> Result<()> {
        info!(
            "Shutting down StreamManager for {}",
            self.streamer.exchange_name()
        );

        // Send shutdown signal to all streaming tasks
        {
            let mut shutdown_txs = self.shutdown_txs.write().await;
            for (symbol, tx) in shutdown_txs.drain() {
                info!("Sending shutdown signal to {}", symbol);
                let _ = tx.send(());
            }
        }

        // Clear subscriptions
        self.subscribed_symbols.write().await.clear();

        // Update stats
        let mut stats = self.stats.write().await;
        stats.is_connected = false;

        Ok(())
    }

    // ========== Private methods ==========

    /// Start a streaming task for a symbol
    async fn start_streaming_task(
        &self,
        symbol: String,
    ) -> Result<impl std::future::Future<Output = ()>> {
        let mut stream = self
            .streamer
            .stream_depth_updates(vec![symbol.clone()])
            .await?;

        let orderbook_manager = self.orderbook_manager.clone();
        let stats = self.stats.clone();
        let config = self.config.clone();
        let exchange = self.streamer.exchange_name().to_string();
        let is_incremental = self.streamer.is_incremental_delta();
        let shutdown_txs = self.shutdown_txs.clone(); // Clone for cleanup

        // Create per-symbol shutdown channel
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::broadcast::channel(1);

        // Store shutdown sender in the per-symbol HashMap
        {
            let mut txs = shutdown_txs.write().await;
            txs.insert(symbol.clone(), shutdown_tx);
        }

        Ok(async move {
            info!("{}: Started streaming task for {}", exchange, symbol);

            // Update stats
            {
                let mut s = stats.write().await;
                s.is_connected = true;
            }

            loop {
                tokio::select! {
                    // Check for shutdown signal
                    _ = shutdown_rx.recv() => {
                        info!("{}: Shutdown signal received for {}", exchange, symbol);
                        break;
                    }

                    // Process depth updates
                    update_result = stream.next() => {
                        match update_result {
                            Some(Ok(depth_update)) => {
                                if let Err(e) = Self::process_depth_update(
                                    &depth_update,
                                    &orderbook_manager,
                                    &stats,
                                    is_incremental,
                                    &exchange,
                                ).await {
                                    error!("{}: Failed to process depth update for {}: {}", exchange, symbol, e);

                                    let mut s = stats.write().await;
                                    s.errors += 1;
                                }
                            }
                            Some(Err(e)) => {
                                error!("{}: Stream error for {}: {}", exchange, symbol, e);

                                let mut s = stats.write().await;
                                s.errors += 1;
                                s.is_connected = false;
                                drop(s);

                                if config.auto_reconnect {
                                    warn!("{}: Reconnecting after error in {:?}", exchange, config.reconnect_delay);
                                    tokio::time::sleep(config.reconnect_delay).await;
                                    // TODO: Implement reconnection logic
                                    break;
                                } else {
                                    break;
                                }
                            }
                            None => {
                                warn!("{}: Stream ended for {}", exchange, symbol);

                                let mut s = stats.write().await;
                                s.is_connected = false;
                                drop(s);

                                if config.auto_reconnect {
                                    warn!("{}: Reconnecting in {:?}", exchange, config.reconnect_delay);
                                    tokio::time::sleep(config.reconnect_delay).await;
                                    // TODO: Implement reconnection logic
                                }
                                break;
                            }
                        }
                    }
                }
            }

            info!("{}: Streaming task ended for {}", exchange, symbol);

            // Cleanup: Remove shutdown sender from HashMap
            {
                let mut txs = shutdown_txs.write().await;
                txs.remove(&symbol);
            }
        })
    }

    /// Process a depth update and apply to OrderbookManager
    ///
    /// This implements the Aster/Binance specification:
    /// - All events are buffered automatically by OrderbookManager
    /// - If snapshot is applied, events are applied as delta updates
    /// - Maintains local orderbook state with proper continuity checking
    async fn process_depth_update(
        depth_update: &DepthUpdate,
        orderbook_manager: &Arc<OrderbookManager>,
        stats: &Arc<RwLock<StreamStats>>,
        is_incremental: bool,
        exchange: &str,
    ) -> Result<()> {
        if depth_update.is_snapshot {
            match orderbook_manager.apply_snapshot(
                &depth_update.symbol,
                depth_update.bids.clone(),
                depth_update.asks.clone(),
                depth_update.final_update_id,
            ).await {
                Ok(replayed_count) => {
                        info!(
                        "[{}] Snapshot applied for {} (lastUpdateId={}, replayed {} buffered events)",
                        exchange,
                        depth_update.symbol,
                        depth_update.final_update_id,
                        replayed_count
                    );
                }
                Err(e) => {
                    return Err(e);
                }
            }

            // Update stats
            let mut stats_write = stats.write().await;
            stats_write.total_updates += 1;
            stats_write.last_update = Some(Instant::now());
            stats_write.is_connected = true;

            Ok(())
        } else {
            // Apply delta update to OrderbookManager
            // OrderbookManager handles:
            // - Buffering events before snapshot
            // - Applying deltas after snapshot with continuity checking
            // - Maintaining local orderbook state
            match orderbook_manager
                .apply_update(
                    &depth_update.symbol,
                    depth_update.first_update_id,
                    depth_update.final_update_id,
                    depth_update.previous_id,
                    depth_update.bids.clone(),
                    depth_update.asks.clone(),
                    is_incremental,
                )
                .await
            {
                Ok(()) => {
                    // Update processed successfully (buffered or applied)
                    debug!(
                    "{}: Processed update for {} (seq: {}-{}, pu: {})",
                    exchange,
                    depth_update.symbol,
                    depth_update.first_update_id,
                    depth_update.final_update_id,
                    depth_update.previous_id
                );
                }
                Err(e) => {
                    // Gap detected or other error - OrderbookManager will handle reconnection
                    warn!(
                    "{}: Failed to apply update for {} (seq: {}-{}): {}",
                    exchange,
                    depth_update.symbol,
                    depth_update.first_update_id,
                    depth_update.final_update_id,
                    e
                );

                    let mut stats_write = stats.write().await;
                    stats_write.errors += 1;
                    return Err(e);
                }
            }

            // Update stats
            let mut stats_write = stats.write().await;
            stats_write.total_updates += 1;
            stats_write.last_update = Some(Instant::now());
            stats_write.is_connected = true;

            Ok(())
        }
    }
}

// Import StreamExt for .next() method
use futures::StreamExt;
use tokio::time::sleep;
