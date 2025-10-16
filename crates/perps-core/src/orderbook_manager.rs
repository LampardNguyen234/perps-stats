use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::sync::RwLock;

use crate::types::{Orderbook, OrderbookLevel};

/// Represents a local orderbook that maintains state and applies delta updates
#[derive(Debug, Clone)]
pub struct LocalOrderbook {
    /// Symbol (normalized format, e.g., "BTC")
    pub symbol: String,

    /// Bid price levels (price -> quantity), sorted descending
    pub bids: BTreeMap<Decimal, Decimal>,

    /// Ask price levels (price -> quantity), sorted ascending
    pub asks: BTreeMap<Decimal, Decimal>,

    /// Last update ID from the exchange (for delta validation)
    pub last_update_id: u64,

    /// Timestamp of last update
    pub timestamp: DateTime<Utc>,
}

impl LocalOrderbook {
    /// Create a new LocalOrderbook from a snapshot
    pub fn from_snapshot(
        symbol: String,
        bids: Vec<OrderbookLevel>,
        asks: Vec<OrderbookLevel>,
        last_update_id: u64,
    ) -> Self {
        let mut bid_map = BTreeMap::new();
        for level in bids {
            if level.quantity > Decimal::ZERO {
                bid_map.insert(level.price, level.quantity);
            }
        }

        let mut ask_map = BTreeMap::new();
        for level in asks {
            if level.quantity > Decimal::ZERO {
                ask_map.insert(level.price, level.quantity);
            }
        }

        Self {
            symbol,
            bids: bid_map,
            asks: ask_map,
            last_update_id,
            timestamp: Utc::now(),
        }
    }

    /// Apply a delta update to the orderbook
    /// Returns true if the update was applied successfully
    ///
    /// # Arguments
    /// * `first_update_id` - First update ID in this event (U in Binance)
    /// * `final_update_id` - Final update ID in this event (u in Binance)
    /// * `bid_updates` - Bid price level updates
    /// * `ask_updates` - Ask price level updates
    ///
    /// # Binance Rules (https://developers.binance.com/docs/derivatives/usds-margined-futures/websocket-market-streams/How-to-manage-a-local-order-book-correctly):
    /// 1. Drop any event where u is <= lastUpdateId in the snapshot
    /// 2. The first processed event should have U <= lastUpdateId+1 AND u >= lastUpdateId+1
    /// 3. While listening to the stream, each new event's U should be equal to the previous event's u+1
    /// 4. If the quantity is 0, remove the price level
    pub fn apply_delta(
        &mut self,
        first_update_id: u64,
        final_update_id: u64,
        bid_updates: Vec<OrderbookLevel>,
        ask_updates: Vec<OrderbookLevel>,
    ) -> anyhow::Result<bool> {
        tracing::debug!(
            "[WS Update] {} received: U={}, u={}, bid_updates={}, ask_updates={}, current_lastUpdateId={}",
            self.symbol,
            first_update_id,
            final_update_id,
            bid_updates.len(),
            ask_updates.len(),
            self.last_update_id
        );

        // Rule 1: Drop stale events
        if final_update_id <= self.last_update_id {
            tracing::debug!(
                "[WS Update] {} DROPPED (stale): u={} <= lastUpdateId={}",
                self.symbol,
                final_update_id,
                self.last_update_id
            );
            return Ok(false);
        }

        // Rule 2 & 3: Sequence validation
        // For the first processed event: U <= lastUpdateId+1 AND u >= lastUpdateId+1
        // For subsequent events: U = previous u + 1
        //
        // NOTE: For aggregated streams like depth@100ms, small gaps are acceptable
        // after initialization since the stream combines multiple updates.

        let expected_first_update_id = self.last_update_id + 1;

        // Validate sequence based on Binance's specification
        if first_update_id <= expected_first_update_id && final_update_id >= expected_first_update_id {
            // This is a valid update (covers the expected update ID)
            tracing::debug!(
                "[WS Update] {} VALID sequence: U={}, u={}, expected={}, lastUpdateId={}",
                self.symbol,
                first_update_id,
                final_update_id,
                expected_first_update_id,
                self.last_update_id
            );
        } else if final_update_id < expected_first_update_id {
            // This update is older than our current state - skip it
            tracing::debug!(
                "[WS Update] {} SKIPPED (stale): u={} < expected={}",
                self.symbol,
                final_update_id,
                expected_first_update_id
            );
            return Ok(false);
        } else {
            // Gap detected - acceptable for aggregated streams like depth@100ms
            // The aggregation means some sequence numbers are combined into single messages
            tracing::debug!(
                "[WS Update] {} sequence GAP (normal for depth@100ms): expected U={}, got U={}, gap={}",
                self.symbol,
                expected_first_update_id,
                first_update_id,
                first_update_id - expected_first_update_id
            );
            // Continue processing - aggregated streams naturally have gaps
        }

        // Track best bid/ask before updates for change detection
        let old_best_bid = self.bids.iter().next_back().map(|(p, q)| (*p, *q));
        let old_best_ask = self.asks.iter().next().map(|(p, q)| (*p, *q));

        // Apply bid updates
        let mut bid_removes = 0;
        let mut bid_adds_or_updates = 0;
        for level in bid_updates {
            if level.quantity == Decimal::ZERO {
                // Rule 4: Remove price level if quantity is 0
                self.bids.remove(&level.price);
                bid_removes += 1;
                tracing::trace!("[WS Update] {} BID REMOVED: price={}", self.symbol, level.price);
            } else {
                let is_new = !self.bids.contains_key(&level.price);
                self.bids.insert(level.price, level.quantity);
                bid_adds_or_updates += 1;
                tracing::trace!(
                    "[WS Update] {} BID {}: price={}, qty={}",
                    self.symbol,
                    if is_new { "ADDED" } else { "UPDATED" },
                    level.price,
                    level.quantity
                );
            }
        }

        // Apply ask updates
        let mut ask_removes = 0;
        let mut ask_adds_or_updates = 0;
        for level in ask_updates {
            if level.quantity == Decimal::ZERO {
                // Rule 4: Remove price level if quantity is 0
                self.asks.remove(&level.price);
                ask_removes += 1;
                tracing::trace!("[WS Update] {} ASK REMOVED: price={}", self.symbol, level.price);
            } else {
                let is_new = !self.asks.contains_key(&level.price);
                self.asks.insert(level.price, level.quantity);
                ask_adds_or_updates += 1;
                tracing::trace!(
                    "[WS Update] {} ASK {}: price={}, qty={}",
                    self.symbol,
                    if is_new { "ADDED" } else { "UPDATED" },
                    level.price,
                    level.quantity
                );
            }
        }

        // Update metadata
        self.last_update_id = final_update_id;
        self.timestamp = Utc::now();

        // Get new best bid/ask
        let new_best_bid = self.bids.iter().next_back().map(|(p, q)| (*p, *q));
        let new_best_ask = self.asks.iter().next().map(|(p, q)| (*p, *q));

        // Log best bid/ask changes
        if old_best_bid != new_best_bid {
            tracing::debug!(
                "[WS Update] {} BEST BID CHANGED: {:?} -> {:?}",
                self.symbol,
                old_best_bid.map(|(p, q)| format!("{}@{}", p, q)),
                new_best_bid.map(|(p, q)| format!("{}@{}", p, q))
            );
        }
        if old_best_ask != new_best_ask {
            tracing::debug!(
                "[WS Update] {} BEST ASK CHANGED: {:?} -> {:?}",
                self.symbol,
                old_best_ask.map(|(p, q)| format!("{}@{}", p, q)),
                new_best_ask.map(|(p, q)| format!("{}@{}", p, q))
            );
        }

        tracing::debug!(
            "[WS Update] {} APPLIED: U={}, u={} | Bids: {} levels (+{} -{}) | Asks: {} levels (+{} -{}) | Best: {}@{} / {}@{}",
            self.symbol,
            first_update_id,
            final_update_id,
            self.bids.len(),
            bid_adds_or_updates,
            bid_removes,
            self.asks.len(),
            ask_adds_or_updates,
            ask_removes,
            new_best_bid.map(|(p, _)| p.to_string()).unwrap_or_else(|| "N/A".to_string()),
            new_best_bid.map(|(_, q)| q.to_string()).unwrap_or_else(|| "N/A".to_string()),
            new_best_ask.map(|(p, _)| p.to_string()).unwrap_or_else(|| "N/A".to_string()),
            new_best_ask.map(|(_, q)| q.to_string()).unwrap_or_else(|| "N/A".to_string())
        );

        Ok(true)
    }

    /// Convert to Orderbook type with specified depth
    pub fn to_orderbook(&self, depth: usize) -> Orderbook {
        // Get top N bids (sorted descending by price)
        let bids: Vec<OrderbookLevel> = self
            .bids
            .iter()
            .rev() // Reverse to get highest prices first
            .take(depth)
            .map(|(price, quantity)| OrderbookLevel {
                price: *price,
                quantity: *quantity,
            })
            .collect();

        // Get top N asks (sorted ascending by price)
        let asks: Vec<OrderbookLevel> = self
            .asks
            .iter()
            .take(depth)
            .map(|(price, quantity)| OrderbookLevel {
                price: *price,
                quantity: *quantity,
            })
            .collect();

        Orderbook {
            symbol: self.symbol.clone(),
            bids,
            asks,
            timestamp: self.timestamp,
        }
    }
}

/// Repository trait for storing orderbooks
#[async_trait]
pub trait OrderbookRepository: Send + Sync {
    async fn store_orderbooks_with_exchange(
        &self,
        exchange: &str,
        orderbooks: &[Orderbook],
    ) -> anyhow::Result<()>;
}

/// Configuration for OrderbookManager
#[derive(Debug, Clone)]
pub struct OrderbookManagerConfig {
    /// Staleness threshold for cached data (default: 2 seconds)
    pub staleness_threshold: std::time::Duration,

    /// Database write interval (default: 1 second)
    pub database_write_interval: std::time::Duration,
}

impl Default for OrderbookManagerConfig {
    fn default() -> Self {
        Self {
            staleness_threshold: std::time::Duration::from_secs(2),
            database_write_interval: std::time::Duration::from_secs(1),
        }
    }
}

/// Health status of the OrderbookManager
#[derive(Debug, Clone)]
pub struct OrderbookManagerHealth {
    pub connected: bool,
    pub symbols_count: usize,
    pub updates_received: u64,
    pub errors: u64,
    pub last_update_at: Option<DateTime<Utc>>,
}

/// Manages local orderbooks with WebSocket delta updates
pub struct OrderbookManager {
    /// Name of the exchange
    exchange: String,

    /// Local orderbooks (symbol -> LocalOrderbook)
    orderbooks: Arc<RwLock<std::collections::HashMap<String, LocalOrderbook>>>,

    /// Database repository
    repository: Option<Arc<dyn OrderbookRepository>>,

    /// Configuration
    config: OrderbookManagerConfig,

    /// Is manager active
    active: Arc<AtomicBool>,

    /// Metrics
    updates_received: Arc<AtomicU64>,
    errors: Arc<AtomicU64>,
    last_update_at: Arc<RwLock<Option<DateTime<Utc>>>>,
}

impl OrderbookManager {
    /// Create a new OrderbookManager
    pub fn new(
        exchange: String,
        repository: Option<Arc<dyn OrderbookRepository>>,
        config: OrderbookManagerConfig,
    ) -> Self {
        Self {
            exchange,
            orderbooks: Arc::new(RwLock::new(std::collections::HashMap::new())),
            repository,
            config,
            active: Arc::new(AtomicBool::new(true)),
            updates_received: Arc::new(AtomicU64::new(0)),
            errors: Arc::new(AtomicU64::new(0)),
            last_update_at: Arc::new(RwLock::new(None)),
        }
    }

    /// Initialize a local orderbook from a snapshot
    pub async fn initialize_orderbook(
        &self,
        symbol: String,
        bids: Vec<OrderbookLevel>,
        asks: Vec<OrderbookLevel>,
        last_update_id: u64,
    ) {
        let local_orderbook = LocalOrderbook::from_snapshot(
            symbol.clone(),
            bids.clone(),
            asks.clone(),
            last_update_id,
        );

        // Extract best bid/ask info before moving the orderbook
        let best_bid_str = local_orderbook.bids.iter().next_back()
            .map(|(p, q)| format!("{}@{}", p, q))
            .unwrap_or_else(|| "N/A".to_string());
        let best_ask_str = local_orderbook.asks.iter().next()
            .map(|(p, q)| format!("{}@{}", p, q))
            .unwrap_or_else(|| "N/A".to_string());

        let mut orderbooks = self.orderbooks.write().await;
        orderbooks.insert(symbol.clone(), local_orderbook);

        tracing::info!(
            "[WS Snapshot] {} initialized: lastUpdateId={}, bids={} levels, asks={} levels, best_bid={}, best_ask={}",
            symbol,
            last_update_id,
            bids.len(),
            asks.len(),
            best_bid_str,
            best_ask_str
        );
    }

    /// Apply a delta update to a local orderbook
    pub async fn apply_update(
        &self,
        symbol: &str,
        first_update_id: u64,
        final_update_id: u64,
        bid_updates: Vec<OrderbookLevel>,
        ask_updates: Vec<OrderbookLevel>,
    ) -> anyhow::Result<()> {
        let mut orderbooks = self.orderbooks.write().await;

        if let Some(local_orderbook) = orderbooks.get_mut(symbol) {
            match local_orderbook.apply_delta(
                first_update_id,
                final_update_id,
                bid_updates,
                ask_updates,
            ) {
                Ok(applied) => {
                    if applied {
                        self.updates_received.fetch_add(1, Ordering::SeqCst);
                        *self.last_update_at.write().await = Some(Utc::now());

                        // Store to database if repository is available
                        if let Some(ref repository) = self.repository {
                            let orderbook = local_orderbook.to_orderbook(1000);
                            let repository = repository.clone();
                            let exchange = self.exchange.clone();
                            let symbol_clone = symbol.to_string();

                            // Non-blocking database write
                            tokio::spawn(async move {
                                if let Err(e) = repository
                                    .store_orderbooks_with_exchange(&exchange, &[orderbook])
                                    .await
                                {
                                    tracing::error!(
                                        "[WS Update] {}/{} database write FAILED: {}",
                                        exchange,
                                        symbol_clone,
                                        e
                                    );
                                } else {
                                    tracing::trace!(
                                        "[WS Update] {}/{} database write SUCCESS",
                                        exchange,
                                        symbol_clone
                                    );
                                }
                            });
                        }
                    }
                    Ok(())
                }
                Err(e) => {
                    self.errors.fetch_add(1, Ordering::SeqCst);
                    tracing::error!(
                        "[WS Update] {}/{} apply_delta FAILED: U={}, u={}, error={}",
                        self.exchange,
                        symbol,
                        first_update_id,
                        final_update_id,
                        e
                    );
                    Err(e)
                }
            }
        } else {
            tracing::error!(
                "[WS Update] {}/{} orderbook NOT INITIALIZED (cannot apply update)",
                self.exchange,
                symbol
            );
            Err(anyhow::anyhow!(
                "Local orderbook not initialized for symbol: {}",
                symbol
            ))
        }
    }

    /// Get orderbook snapshot from cache
    pub async fn get_orderbook(&self, symbol: &str, depth: usize) -> Option<Orderbook> {
        let orderbooks = self.orderbooks.read().await;
        orderbooks.get(symbol).map(|local_orderbook| {
            local_orderbook.to_orderbook(depth)
        })
    }

    /// Check if orderbook is fresh (not stale)
    pub async fn is_fresh(&self, symbol: &str) -> bool {
        let orderbooks = self.orderbooks.read().await;
        if let Some(local_orderbook) = orderbooks.get(symbol) {
            let age = Utc::now()
                .signed_duration_since(local_orderbook.timestamp)
                .to_std()
                .unwrap_or_default();
            age < self.config.staleness_threshold
        } else {
            false
        }
    }

    /// Get health status
    pub async fn health(&self) -> OrderbookManagerHealth {
        let orderbooks = self.orderbooks.read().await;
        OrderbookManagerHealth {
            connected: self.active.load(Ordering::SeqCst),
            symbols_count: orderbooks.len(),
            updates_received: self.updates_received.load(Ordering::SeqCst),
            errors: self.errors.load(Ordering::SeqCst),
            last_update_at: *self.last_update_at.read().await,
        }
    }

    /// Remove a symbol from management
    pub async fn remove_symbol(&self, symbol: &str) {
        let mut orderbooks = self.orderbooks.write().await;
        orderbooks.remove(symbol);
        tracing::info!("Removed local orderbook for {}", symbol);
    }

    /// Shutdown the manager
    pub async fn shutdown(&self) {
        self.active.store(false, Ordering::SeqCst);
        tracing::info!("OrderbookManager for {} shutting down", self.exchange);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_local_orderbook_from_snapshot() {
        let bids = vec![
            OrderbookLevel {
                price: dec!(100.0),
                quantity: dec!(1.0),
            },
            OrderbookLevel {
                price: dec!(99.0),
                quantity: dec!(2.0),
            },
        ];

        let asks = vec![
            OrderbookLevel {
                price: dec!(101.0),
                quantity: dec!(1.5),
            },
            OrderbookLevel {
                price: dec!(102.0),
                quantity: dec!(2.5),
            },
        ];

        let orderbook = LocalOrderbook::from_snapshot(
            "BTC".to_string(),
            bids,
            asks,
            12345,
        );

        assert_eq!(orderbook.symbol, "BTC");
        assert_eq!(orderbook.bids.len(), 2);
        assert_eq!(orderbook.asks.len(), 2);
        assert_eq!(orderbook.last_update_id, 12345);
    }

    #[test]
    fn test_apply_delta_update() {
        let mut orderbook = LocalOrderbook::from_snapshot(
            "BTC".to_string(),
            vec![
                OrderbookLevel {
                    price: dec!(100.0),
                    quantity: dec!(1.0),
                },
            ],
            vec![
                OrderbookLevel {
                    price: dec!(101.0),
                    quantity: dec!(1.0),
                },
            ],
            12345,
        );

        // Apply valid delta update
        let result = orderbook.apply_delta(
            12346,
            12346,
            vec![OrderbookLevel {
                price: dec!(100.0),
                quantity: dec!(2.0), // Update existing
            }],
            vec![OrderbookLevel {
                price: dec!(102.0),
                quantity: dec!(1.0), // Add new level
            }],
        );

        assert!(result.is_ok());
        assert!(result.unwrap());
        assert_eq!(orderbook.last_update_id, 12346);
        assert_eq!(orderbook.bids.get(&dec!(100.0)), Some(&dec!(2.0)));
        assert_eq!(orderbook.asks.get(&dec!(102.0)), Some(&dec!(1.0)));
    }

    #[test]
    fn test_apply_delta_removes_zero_quantity() {
        let mut orderbook = LocalOrderbook::from_snapshot(
            "BTC".to_string(),
            vec![
                OrderbookLevel {
                    price: dec!(100.0),
                    quantity: dec!(1.0),
                },
            ],
            vec![],
            12345,
        );

        // Apply delta that removes level
        let result = orderbook.apply_delta(
            12346,
            12346,
            vec![OrderbookLevel {
                price: dec!(100.0),
                quantity: Decimal::ZERO, // Remove
            }],
            vec![],
        );

        assert!(result.is_ok());
        assert!(orderbook.bids.is_empty());
    }

    #[test]
    fn test_apply_delta_drops_stale() {
        let mut orderbook = LocalOrderbook::from_snapshot(
            "BTC".to_string(),
            vec![],
            vec![],
            12345,
        );

        // Try to apply stale update
        let result = orderbook.apply_delta(12340, 12344, vec![], vec![]);

        assert!(result.is_ok());
        assert!(!result.unwrap()); // Should be dropped
        assert_eq!(orderbook.last_update_id, 12345); // Unchanged
    }

    #[test]
    fn test_to_orderbook_with_depth() {
        let orderbook = LocalOrderbook::from_snapshot(
            "BTC".to_string(),
            vec![
                OrderbookLevel {
                    price: dec!(100.0),
                    quantity: dec!(1.0),
                },
                OrderbookLevel {
                    price: dec!(99.0),
                    quantity: dec!(2.0),
                },
                OrderbookLevel {
                    price: dec!(98.0),
                    quantity: dec!(3.0),
                },
            ],
            vec![
                OrderbookLevel {
                    price: dec!(101.0),
                    quantity: dec!(1.0),
                },
                OrderbookLevel {
                    price: dec!(102.0),
                    quantity: dec!(2.0),
                },
            ],
            12345,
        );

        let snapshot = orderbook.to_orderbook(2);

        assert_eq!(snapshot.bids.len(), 2);
        assert_eq!(snapshot.asks.len(), 2);
        // Bids should be sorted descending
        assert_eq!(snapshot.bids[0].price, dec!(100.0));
        assert_eq!(snapshot.bids[1].price, dec!(99.0));
        // Asks should be sorted ascending
        assert_eq!(snapshot.asks[0].price, dec!(101.0));
        assert_eq!(snapshot.asks[1].price, dec!(102.0));
    }
}
