use crate::types::{Orderbook, OrderbookLevel};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

// LocalOrderbook uses parking_lot (sync, fast, CPU-bound operations)
use parking_lot::{Mutex as ParkingMutex, RwLock as ParkingRwLock};

// OrderbookManager uses tokio (async, for async context integration)
use tokio::sync::RwLock as TokioRwLock;

/// Buffered update event for replay after snapshot
#[derive(Debug, Clone)]
struct BufferedUpdate {
    first_update_id: u64,
    final_update_id: u64,
    #[allow(dead_code)]
    previous_id: u64,
    bid_updates: Vec<OrderbookLevel>,
    ask_updates: Vec<OrderbookLevel>,
    is_incremental_delta: bool,
    #[allow(dead_code)]
    is_snapshot: bool,
}

/// Internal orderbook data (protected by RwLock)
#[derive(Debug)]
struct OrderbookData {
    /// Exchange name (for logging context)
    exchange: String,

    /// Symbol (normalized format, e.g., "BTC")
    symbol: String,

    /// Bid price levels (price -> quantity), sorted descending
    bids: BTreeMap<Decimal, Decimal>,

    /// Ask price levels (price -> quantity), sorted ascending
    asks: BTreeMap<Decimal, Decimal>,

    /// Last update ID from the exchange (for delta validation)
    last_update_id: u64,

    /// Timestamp of last update
    timestamp: DateTime<Utc>,

    /// Circular buffer of recent updates for snapshot replay
    /// Following Binance spec: buffer events before snapshot, replay after
    update_buffer: VecDeque<BufferedUpdate>,

    /// Maximum buffer size (configured per exchange)
    buffer_size: usize,

    /// Set when the last applied update left best_bid >= best_ask (a stale price level
    /// that never received its removal/update message — see apply_delta_internal).
    /// Cleared on the next clean snapshot. Callers use this to force a REST resync
    /// instead of waiting on Extended's periodic snapshot, since exchanges with no
    /// gap/sequence signal (previous_id always 0) can't detect a dropped message any
    /// other way.
    crossed: bool,

    /// Set by `apply_snapshot` when its replay bridged zero buffered events (every
    /// buffered delta was stale/pre-snapshot). On some exchanges (Nado) the REST
    /// snapshot's sequence value is a wall-clock sample of when it was computed, not a
    /// checkpoint drawn from the same discrete id space as `previous_id` chains through
    /// deltas - so it will never equal a delta's `previous_id`, even with zero data
    /// loss. When true, `apply_delta`'s Rule 4 check is skipped exactly once (for the
    /// first live delta that clears Rule 3's staleness bar), mirroring how Binance's own
    /// snapshot spec never validates continuity for the snapshot-to-stream seam itself,
    /// only for deltas relative to each other from that point on. Left false (and thus
    /// never triggered) for exchanges like Binance/Aster where the snapshot's id already
    /// is a valid point on the delta chain, so this never changes their behavior.
    bootstrap_pending: bool,
}

/// Represents a local orderbook that maintains state and applies delta updates
/// Thread-safe with internal locking for concurrent operations
pub struct LocalOrderbook {
    /// Serializes snapshot vs update operations
    /// Ensures apply_snapshot() blocks apply_delta() and vice versa
    snapshot_lock: ParkingMutex<()>,

    /// Protects the actual orderbook data
    /// Allows concurrent reads, exclusive writes
    data: ParkingRwLock<OrderbookData>,

    /// Set once at construction (never mutated afterward, so no lock needed). See
    /// `OrderbookData::bootstrap_pending`'s doc comment - only exchanges whose REST
    /// snapshot sequence is NOT drawn from the same id space as delta `previous_id` chains
    /// (currently: Nado) opt into this; everyone else keeps the original strict Rule 4.
    tolerate_snapshot_seam: bool,
}

impl LocalOrderbook {
    /// Create a new empty LocalOrderbook for buffering before snapshot
    /// This is used when starting WebSocket stream before fetching snapshot
    pub fn new_empty(exchange: String, symbol: String, buffer_size: usize) -> Self {
        let data = OrderbookData {
            exchange,
            symbol,
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            last_update_id: 0,
            timestamp: Utc::now(),
            update_buffer: VecDeque::with_capacity(buffer_size),
            buffer_size,
            crossed: false,
            bootstrap_pending: false,
        };

        Self {
            snapshot_lock: ParkingMutex::new(()),
            data: ParkingRwLock::new(data),
            tolerate_snapshot_seam: false,
        }
    }

    /// Opt this orderbook into the snapshot-seam Rule 4 exemption (see
    /// `tolerate_snapshot_seam`'s doc comment). Builder-style so existing construction call
    /// sites (tests included) are unaffected; only `OrderbookManager`'s init methods call this,
    /// gated on `OrderbookManagerConfig::tolerate_snapshot_seam`.
    pub(crate) fn with_tolerate_snapshot_seam(mut self, tolerate: bool) -> Self {
        self.tolerate_snapshot_seam = tolerate;
        self
    }

    /// Create a new LocalOrderbook from a snapshot
    pub fn from_snapshot(
        exchange: String,
        symbol: String,
        bids: Vec<OrderbookLevel>,
        asks: Vec<OrderbookLevel>,
        last_update_id: u64,
        buffer_size: usize,
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

        let data = OrderbookData {
            exchange,
            symbol,
            bids: bid_map,
            asks: ask_map,
            last_update_id,
            timestamp: Utc::now(),
            update_buffer: VecDeque::with_capacity(buffer_size),
            buffer_size,
            crossed: false,
            bootstrap_pending: false,
        };

        Self {
            snapshot_lock: ParkingMutex::new(()),
            data: ParkingRwLock::new(data),
            tolerate_snapshot_seam: false,
        }
    }

    /// Apply a snapshot and replay buffered events
    ///
    /// Following Binance/Aster specification (Step 5):
    /// 1. WebSocket stream starts buffering events
    /// 2. REST API snapshot is fetched (with lastUpdateId)
    /// 3. This method applies snapshot and replays buffered events where final_update_id >= lastUpdateId
    ///    - First event processed should straddle the snapshot: U <= lastUpdateId AND u >= lastUpdateId
    ///    - Subsequent events have U > lastUpdateId
    ///
    /// For Extended: This is called when SNAPSHOT event is received from WebSocket
    pub fn apply_snapshot(
        &self,
        bids: Vec<OrderbookLevel>,
        asks: Vec<OrderbookLevel>,
        last_update_id: u64,
    ) -> anyhow::Result<usize> {
        // Acquire snapshot lock to serialize with apply_delta
        let _snapshot_guard = self.snapshot_lock.lock();

        // Acquire write lock on data
        let mut data = self.data.write();

        tracing::debug!(
            "[Snapshot] {}/{} applying snapshot: lastUpdateId={}, bids={} levels, asks={} levels, buffered_events={}",
            data.exchange,
            data.symbol,
            last_update_id,
            bids.len(),
            asks.len(),
            data.update_buffer.len()
        );

        // Clear current state
        data.bids.clear();
        data.asks.clear();
        data.crossed = false;

        // Apply snapshot
        for level in bids {
            if level.quantity > Decimal::ZERO {
                data.bids.insert(level.price, level.quantity);
            }
        }

        for level in asks {
            if level.quantity > Decimal::ZERO {
                data.asks.insert(level.price, level.quantity);
            }
        }

        data.last_update_id = last_update_id;
        data.timestamp = Utc::now();

        // Replay buffered events following Binance Step 5:
        // "The first processed event should have U <= lastUpdateId AND u >= lastUpdateId"
        // This means we process straddling events and events after the snapshot
        let mut replayed = 0;
        let mut skipped = 0;
        let buffered_events: Vec<_> = data.update_buffer.drain(..).collect();

        for event in buffered_events {
            // Skip events entirely before the snapshot (final_update_id < lastUpdateId)
            if event.final_update_id < last_update_id {
                tracing::trace!(
                    "[Snapshot] {}/{} skipping old event: U={}, u={} < lastUpdateId={}",
                    data.exchange,
                    data.symbol,
                    event.first_update_id,
                    event.final_update_id,
                    last_update_id
                );
                skipped += 1;
                continue;
            }

            let is_straddling =
                event.first_update_id <= last_update_id && event.final_update_id >= last_update_id;

            if is_straddling {
                tracing::trace!(
                    "[Snapshot] {}/{} replaying STRADDLING event: U={}, u={}, lastUpdateId={}",
                    data.exchange,
                    data.symbol,
                    event.first_update_id,
                    event.final_update_id,
                    last_update_id
                );
            } else {
                tracing::debug!(
                    "[Snapshot] {}/{} replaying buffered event: U={}, u={}",
                    data.exchange,
                    data.symbol,
                    event.first_update_id,
                    event.final_update_id
                );
            }

            // Replay the event (no buffering, no validation - just apply)
            match Self::apply_delta_internal(
                &mut data,
                event.final_update_id,
                event.bid_updates,
                event.ask_updates,
                event.is_incremental_delta,
            ) {
                Ok(true) => replayed += 1,
                Ok(false) => {
                    tracing::debug!(
                        "[Snapshot] {}/{} replayed event was stale: u={}",
                        data.exchange,
                        data.symbol,
                        event.final_update_id
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "[Snapshot] {}/{} failed to replay event U={}, u={}: {}",
                        data.exchange,
                        data.symbol,
                        event.first_update_id,
                        event.final_update_id,
                        e
                    );
                }
            }
        }

        // If nothing replayed, last_update_id is still the snapshot's own timestamp - not
        // a checkpoint any delta's previous_id can ever equal on Nado (see bootstrap_pending's
        // doc comment). Exempt exactly the next delta that clears Rule 3 from Rule 4. Gated on
        // tolerate_snapshot_seam so exchanges with a chain-derived snapshot id (Binance/Aster/
        // etc, where this same zero-replay case can legitimately mean a real missed delta)
        // keep the original strict check.
        data.bootstrap_pending = self.tolerate_snapshot_seam && replayed == 0;

        tracing::debug!(
            "[Snapshot] {}/{} replay complete: replayed={}, skipped={}, final_lastUpdateId={}",
            data.exchange,
            data.symbol,
            replayed,
            skipped,
            data.last_update_id
        );

        Ok(replayed)
    }

    /// Apply a delta update to the orderbook
    /// Returns true if the update was applied successfully
    ///
    /// This method always buffers events for potential snapshot replay.
    ///
    /// # Arguments
    /// * `first_update_id` - First update ID in this event (U in Binance)
    /// * `final_update_id` - Final update ID in this event (u in Binance)
    /// * `previous_id` - Previous update ID (pu in Binance/Aster, 0 for other exchanges)
    /// * `bid_updates` - Bid price level updates
    /// * `ask_updates` - Ask price level updates
    /// * `is_incremental_delta` - Update mode (controls quantity arithmetic only, both require strict sequence):
    ///   - `true`: Incremental delta (Extended) - quantities represent changes (new_qty = existing_qty + delta_qty)
    ///   - `false`: Full price update (Binance/Aster) - quantities are absolute replacements (new_qty = delta_qty)
    ///
    /// # Batched Updates
    /// WebSocket messages can contain batched updates where first_update_id != final_update_id.
    /// The message represents all updates from U to u (inclusive).
    pub fn apply_delta(
        &self,
        first_update_id: u64,
        final_update_id: u64,
        previous_id: u64,
        bid_updates: Vec<OrderbookLevel>,
        ask_updates: Vec<OrderbookLevel>,
        is_incremental_delta: bool,
    ) -> anyhow::Result<bool> {
        // Acquire snapshot lock to serialize with apply_snapshot
        let _snapshot_guard = self.snapshot_lock.lock();

        // Acquire write lock on data
        let mut data = self.data.write();

        // Rule 1: ALWAYS buffer first (even if validation fails)
        // This ensures we can replay events after snapshot
        let buffered = BufferedUpdate {
            first_update_id,
            final_update_id,
            previous_id,
            bid_updates: bid_updates.clone(),
            ask_updates: ask_updates.clone(),
            is_incremental_delta,
            is_snapshot: false,
        };

        // Maintain circular buffer (drop oldest if full)
        if data.update_buffer.len() >= data.buffer_size {
            data.update_buffer.pop_front();
        }
        data.update_buffer.push_back(buffered);

        // Rule 2: If no snapshot yet (lastUpdateId=0), only buffer
        if data.last_update_id == 0 {
            tracing::debug!(
                "[WS Update] {}/{} BUFFERED (no snapshot yet): U={}, u={}, buffered_count={}",
                data.exchange,
                data.symbol,
                first_update_id,
                final_update_id,
                data.update_buffer.len()
            );
            return Ok(false);
        }

        // Rule 3: Staleness check - both first_update_id and final_update_id must be > lastUpdateId
        if final_update_id < data.last_update_id {
            tracing::debug!(
                "[WS Update] {}/{} DROPPED (stale final_update_id): u={} <= lastUpdateId={}",
                data.exchange,
                data.symbol,
                final_update_id,
                data.last_update_id
            );
            return Ok(false);
        }

        if first_update_id < data.last_update_id {
            tracing::debug!(
                "[WS Update] {}/{} DROPPED (stale first_update_id): U={} <= lastUpdateId={}",
                data.exchange,
                data.symbol,
                first_update_id,
                data.last_update_id
            );
            return Ok(false);
        }

        // Rule 4: Continuity validation. The first delta to reach this point after a
        // snapshot whose replay bridged zero events is exempted once (bootstrap_pending) -
        // see its doc comment on OrderbookData for why that specific case is a false
        // positive, not a real gap.
        let bootstrap_exempt = data.bootstrap_pending;
        data.bootstrap_pending = false;
        if previous_id != 0 && previous_id != data.last_update_id {
            if bootstrap_exempt {
                tracing::debug!(
                    "[WS Update] {}/{} snapshot-seam pu mismatch ignored (bootstrap replay bridged zero events): pu={}, snapshot_lastUpdateId={}",
                    data.exchange,
                    data.symbol,
                    previous_id,
                    data.last_update_id
                );
            } else {
                tracing::error!(
                    "[WS Update] {}/{} CRITICAL: previous_id mismatch (Rule 4)! pu={}, lastUpdateId={}. Reconnection required!",
                    data.exchange,
                    data.symbol,
                    previous_id,
                    data.last_update_id
                );
                return Err(anyhow::anyhow!(
                    "Previous ID mismatch for {}/{} (Rule 4): pu={}, expected={}. Sequence integrity violated.",
                    data.exchange,
                    data.symbol,
                    previous_id,
                    data.last_update_id
                ));
            }
        }

        // All validation passed, apply the update
        Self::apply_delta_internal(
            &mut data,
            final_update_id,
            bid_updates,
            ask_updates,
            is_incremental_delta,
        )
    }

    /// Internal delta application - pure state update without validation
    ///
    /// This method applies delta updates directly to the orderbook state.
    /// ALL validation (staleness, continuity, gaps) must be performed by the caller.
    ///
    /// This is used by:
    /// - `apply_delta()` - after full validation
    /// - `apply_snapshot()` - when replaying buffered events
    fn apply_delta_internal(
        data: &mut OrderbookData,
        final_update_id: u64,
        bid_updates: Vec<OrderbookLevel>,
        ask_updates: Vec<OrderbookLevel>,
        is_incremental_delta: bool,
    ) -> anyhow::Result<bool> {
        // Track best bid/ask before updates for change detection
        let old_best_bid = data.bids.iter().next_back().map(|(p, q)| (*p, *q));
        let old_best_ask = data.asks.iter().next().map(|(p, q)| (*p, *q));

        // Apply bid updates
        let mut bid_removes = 0;
        let mut bid_adds_or_updates = 0;
        for level in bid_updates {
            // Calculate final quantity based on update mode
            let final_quantity = if is_incremental_delta {
                // Incremental delta: add the delta to existing quantity
                let existing_quantity = data
                    .bids
                    .get(&level.price)
                    .copied()
                    .unwrap_or(Decimal::ZERO);
                let new_quantity = existing_quantity + level.quantity;

                tracing::trace!(
                    "[WS Update] {}/{} BID DELTA: price={}, existing={}, delta={}, new={}",
                    data.exchange,
                    data.symbol,
                    level.price,
                    existing_quantity,
                    level.quantity,
                    new_quantity
                );

                new_quantity
            } else {
                // Full price update: use the quantity as-is (absolute value)
                level.quantity
            };

            // Determine if this level should be removed
            let should_remove = final_quantity <= Decimal::ZERO;

            if should_remove {
                data.bids.remove(&level.price);
                bid_removes += 1;
                if final_quantity < Decimal::ZERO {
                    tracing::warn!(
                        "[WS Update] {}/{} BID REMOVED: price={}, final_qty={}, mode={}",
                        data.exchange,
                        data.symbol,
                        level.price,
                        final_quantity,
                        if is_incremental_delta {
                            "incremental"
                        } else {
                            "full_price"
                        }
                    );
                }
            } else {
                let is_new = !data.bids.contains_key(&level.price);
                data.bids.insert(level.price, final_quantity);
                bid_adds_or_updates += 1;
                tracing::trace!(
                    "[WS Update] {}/{} BID {}: price={}, qty={}, mode={}",
                    data.exchange,
                    data.symbol,
                    if is_new { "ADDED" } else { "UPDATED" },
                    level.price,
                    final_quantity,
                    if is_incremental_delta {
                        "incremental"
                    } else {
                        "full_price"
                    }
                );
            }
        }

        // Apply ask updates
        let mut ask_removes = 0;
        let mut ask_adds_or_updates = 0;
        for level in ask_updates {
            // Calculate final quantity based on update mode
            let final_quantity = if is_incremental_delta {
                // Incremental delta: add the delta to existing quantity
                let existing_quantity = data
                    .asks
                    .get(&level.price)
                    .copied()
                    .unwrap_or(Decimal::ZERO);
                let new_quantity = existing_quantity + level.quantity;

                tracing::trace!(
                    "[WS Update] {}/{} ASK DELTA: price={}, existing={}, delta={}, new={}",
                    data.exchange,
                    data.symbol,
                    level.price,
                    existing_quantity,
                    level.quantity,
                    new_quantity
                );

                new_quantity
            } else {
                // Full price update: use the quantity as-is (absolute value)
                level.quantity
            };

            // Determine if this level should be removed
            let should_remove = final_quantity <= Decimal::ZERO;

            if should_remove {
                data.asks.remove(&level.price);
                ask_removes += 1;
                if final_quantity < Decimal::ZERO {
                    tracing::warn!(
                        "[WS Update] {}/{} ASK REMOVED: price={}, final_qty={}, mode={}",
                        data.exchange,
                        data.symbol,
                        level.price,
                        final_quantity,
                        if is_incremental_delta {
                            "incremental"
                        } else {
                            "full_price"
                        }
                    );
                }
            } else {
                let is_new = !data.asks.contains_key(&level.price);
                data.asks.insert(level.price, final_quantity);
                ask_adds_or_updates += 1;
                tracing::trace!(
                    "[WS Update] {}/{} ASK {}: price={}, qty={}, mode={}",
                    data.exchange,
                    data.symbol,
                    if is_new { "ADDED" } else { "UPDATED" },
                    level.price,
                    final_quantity,
                    if is_incremental_delta {
                        "incremental"
                    } else {
                        "full_price"
                    }
                );
            }
        }

        // Update metadata (use final_update_id as the new last_update_id)
        data.last_update_id = final_update_id;
        data.timestamp = Utc::now();

        // Get new best bid/ask
        let new_best_bid = data.bids.iter().next_back().map(|(p, q)| (*p, *q));
        let new_best_ask = data.asks.iter().next().map(|(p, q)| (*p, *q));

        if old_best_bid != new_best_bid {
            tracing::trace!(
                "[WS Update] {}/{} BEST BID CHANGED: {:?} -> {:?}",
                data.exchange,
                data.symbol,
                old_best_bid.map(|(p, q)| format!("{}@{}", p, q)),
                new_best_bid.map(|(p, q)| format!("{}@{}", p, q))
            );
        }
        if old_best_ask != new_best_ask {
            tracing::trace!(
                "[WS Update] {}/{} BEST ASK CHANGED: {:?} -> {:?}",
                data.exchange,
                data.symbol,
                old_best_ask.map(|(p, q)| format!("{}@{}", p, q)),
                new_best_ask.map(|(p, q)| format!("{}@{}", p, q))
            );
        }

        // Reflects whether the book is crossed *right now*, not "was ever crossed" —
        // cleared here the moment a later update resolves it, same as it's set here the
        // moment one appears.
        data.crossed = new_best_ask < new_best_bid;
        if data.crossed {
            let top_bids: Vec<String> = data
                .bids
                .iter()
                .rev()
                .take(5)
                .map(|(p, q)| format!("{}@{}", p, q))
                .collect();
            let top_asks: Vec<String> = data
                .asks
                .iter()
                .take(5)
                .map(|(p, q)| format!("{}@{}", p, q))
                .collect();

            tracing::warn!(
                "[WS Update] {}/{} Bid-Ask invariant VIOLATED: Best Bid {}@{} >= Best Ask {}@{} | Top 5 Bids: {:?} | Top 5 Asks: {:?}",
                data.exchange,
                data.symbol,
                new_best_bid.map(|(p, _)| p.to_string()).unwrap_or_else(|| "N/A".to_string()),
                new_best_bid.map(|(_, q)| q.to_string()).unwrap_or_else(|| "N/A".to_string()),
                new_best_ask.map(|(p, _)| p.to_string()).unwrap_or_else(|| "N/A".to_string()),
                new_best_ask.map(|(_, q)| q.to_string()).unwrap_or_else(|| "N/A".to_string()),
                top_bids,
                top_asks
            );
        }

        let bid_liq: Decimal = data.bids.iter().map(|(p, q)| p * q).sum();
        let ask_liq: Decimal = data.asks.iter().map(|(p, q)| p * q).sum();
        let mid = new_best_bid
            .zip(new_best_ask)
            .map(|((bp, _), (ap, _))| (bp + ap) / Decimal::TWO);
        tracing::debug!(
            "[{}] {} upd bestBid={} bestAsk={} mid={} bidLiq={:.4} askLiq={:.4} +{}/−{}",
            data.exchange,
            data.symbol,
            new_best_bid
                .map(|(p, _)| format!("{:.2}", p))
                .unwrap_or_else(|| "-".to_string()),
            new_best_ask
                .map(|(p, _)| format!("{:.2}", p))
                .unwrap_or_else(|| "-".to_string()),
            mid.map(|m| format!("{:.2}", m))
                .unwrap_or_else(|| "-".to_string()),
            bid_liq,
            ask_liq,
            bid_adds_or_updates + ask_adds_or_updates,
            bid_removes + ask_removes,
        );

        Ok(true)
    }

    /// Convert to Orderbook type with specified depth
    pub fn to_orderbook(&self, _depth: usize) -> Orderbook {
        // Acquire read lock on data (allows concurrent reads)
        let data = self.data.read();

        // Get top N bids (sorted descending by price)
        let bids: Vec<OrderbookLevel> = data
            .bids
            .iter()
            .rev() // Reverse to get highest prices first
            .map(|(price, quantity)| OrderbookLevel {
                price: *price,
                quantity: *quantity,
            })
            .collect();

        // Get top N asks (sorted ascending by price)
        let asks: Vec<OrderbookLevel> = data
            .asks
            .iter()
            .map(|(price, quantity)| OrderbookLevel {
                price: *price,
                quantity: *quantity,
            })
            .collect();

        Orderbook {
            symbol: data.symbol.clone(),
            bids,
            asks,
            timestamp: data.timestamp,
        }
    }

    /// Get timestamp of last update
    pub fn get_timestamp(&self) -> DateTime<Utc> {
        let data = self.data.read();
        data.timestamp
    }

    /// True if the last applied update left best_bid >= best_ask (see `crossed` on
    /// `OrderbookData`).
    pub fn is_crossed(&self) -> bool {
        self.data.read().crossed
    }
}

/// Configuration for OrderbookManager
#[derive(Debug, Clone)]
pub struct OrderbookManagerConfig {
    /// Staleness threshold for cached data (default: 2 seconds)
    pub staleness_threshold: std::time::Duration,

    /// Update buffer size for snapshot replay
    /// - Binance/Aster: Use 100 (handles race condition during REST snapshot fetch)
    /// - Extended: Use 1000 (SNAPSHOT arrives quickly via WebSocket)
    pub update_buffer_size: usize,

    /// See `LocalOrderbook::tolerate_snapshot_seam`'s doc comment. Only true for exchanges
    /// whose REST snapshot sequence value is independently sampled from the same clock as
    /// deltas but not a checkpoint drawn from their discrete id chain (currently: Nado).
    /// False for every other exchange, preserving the original strict Rule 4.
    pub tolerate_snapshot_seam: bool,
}

impl Default for OrderbookManagerConfig {
    fn default() -> Self {
        Self {
            staleness_threshold: std::time::Duration::from_secs(2),
            update_buffer_size: 100,
            tolerate_snapshot_seam: false,
        }
    }
}

impl OrderbookManagerConfig {
    /// Create config optimized for Binance/Aster (100 buffer for REST snapshot race condition)
    /// Higher throttle (100) due to frequent updates
    pub fn for_binance_aster() -> Self {
        Self {
            update_buffer_size: 100,
            ..Default::default()
        }
    }

    /// Nado's `market_liquidity` REST snapshot timestamp is a wall-clock sample, not a
    /// checkpoint drawn from the same discrete id space as the WS delta chain's
    /// `previous_id` - see `LocalOrderbook::tolerate_snapshot_seam`'s doc comment for the
    /// full mechanics of the false-positive Rule 4 this otherwise causes.
    pub fn for_nado() -> Self {
        Self {
            tolerate_snapshot_seam: true,
            ..Default::default()
        }
    }

    pub fn for_extended() -> Self {
        Self {
            update_buffer_size: 1000,
            ..Default::default()
        }
    }

    pub fn for_kucoin() -> Self {
        Self {
            update_buffer_size: 100000,
            ..Default::default()
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

    /// Local orderbooks (symbol -> Arc<LocalOrderbook>)
    /// LocalOrderbook is thread-safe with internal locking
    orderbooks: Arc<TokioRwLock<std::collections::HashMap<String, Arc<LocalOrderbook>>>>,

    /// Configuration
    config: OrderbookManagerConfig,

    /// Is manager active
    active: Arc<AtomicBool>,

    /// Metrics
    updates_received: Arc<AtomicU64>,
    errors: Arc<AtomicU64>,
    last_update_at: Arc<TokioRwLock<Option<DateTime<Utc>>>>,
}

impl OrderbookManager {
    /// Create a new OrderbookManager
    pub fn new(exchange: String, config: OrderbookManagerConfig) -> Self {
        Self {
            exchange,
            orderbooks: Arc::new(TokioRwLock::new(std::collections::HashMap::new())),
            config,
            active: Arc::new(AtomicBool::new(true)),
            updates_received: Arc::new(AtomicU64::new(0)),
            errors: Arc::new(AtomicU64::new(0)),
            last_update_at: Arc::new(TokioRwLock::new(None)),
        }
    }

    /// Initialize an empty local orderbook for buffering before snapshot
    ///
    /// This should be called when starting WebSocket stream before fetching snapshot.
    /// The orderbook will buffer incoming events until apply_snapshot is called.
    pub async fn initialize_empty_orderbook(&self, symbol: String) {
        let local_orderbook = LocalOrderbook::new_empty(
            self.exchange.clone(),
            symbol.clone(),
            self.config.update_buffer_size,
        )
        .with_tolerate_snapshot_seam(self.config.tolerate_snapshot_seam);

        let mut orderbooks = self.orderbooks.write().await;
        orderbooks.insert(symbol.clone(), Arc::new(local_orderbook));

        tracing::debug!(
            "[WS Init] {}/{} empty orderbook created (buffering mode, buffer_size={})",
            self.exchange,
            symbol,
            self.config.update_buffer_size
        );
    }

    /// Apply a snapshot and replay buffered events
    ///
    /// Following Binance/Aster specification (Step 5):
    /// 1. WebSocket stream starts and buffers events
    /// 2. REST API snapshot is fetched (with lastUpdateId)
    /// 3. This method applies snapshot and replays buffered events where final_update_id >= lastUpdateId
    ///    - First event processed should straddle the snapshot: U <= lastUpdateId AND u >= lastUpdateId
    ///    - Subsequent events have U > lastUpdateId
    ///
    /// For Extended: This is called when SNAPSHOT event is received from WebSocket
    ///
    /// Note: Uses per-symbol locking - concurrent snapshots for different symbols are allowed
    pub async fn apply_snapshot(
        &self,
        symbol: &str,
        bids: Vec<OrderbookLevel>,
        asks: Vec<OrderbookLevel>,
        last_update_id: u64,
    ) -> anyhow::Result<usize> {
        // Get orderbook reference with READ lock (allows concurrent lookups for different symbols)
        let orderbook_arc = {
            let orderbooks = self.orderbooks.read().await;
            orderbooks.get(symbol).cloned()
        }; // READ lock released immediately

        if let Some(orderbook) = orderbook_arc {
            // LocalOrderbook handles its own locking internally
            orderbook.apply_snapshot(bids, asks, last_update_id)
        } else {
            Err(anyhow::anyhow!(
                "Local orderbook not initialized for symbol: {}. Call initialize_empty_orderbook first.",
                symbol
            ))
        }
    }

    /// Initialize a local orderbook from a snapshot (legacy method)
    ///
    /// **Note**: This method is deprecated in favor of the new buffering flow:
    /// 1. Call `initialize_empty_orderbook()` when starting WebSocket
    /// 2. Let WebSocket buffer events
    /// 3. Fetch REST snapshot
    /// 4. Call `apply_snapshot()` to apply snapshot and replay buffered events
    ///
    /// This method is kept for backward compatibility and simple use cases.
    pub async fn initialize_orderbook(
        &self,
        symbol: String,
        bids: Vec<OrderbookLevel>,
        asks: Vec<OrderbookLevel>,
        last_update_id: u64,
    ) {
        // Extract best bid/ask info from the provided vectors
        let best_bid_str = bids
            .last()
            .map(|level| format!("{}@{}", level.price, level.quantity))
            .unwrap_or_else(|| "N/A".to_string());
        let best_ask_str = asks
            .first()
            .map(|level| format!("{}@{}", level.price, level.quantity))
            .unwrap_or_else(|| "N/A".to_string());

        let local_orderbook = LocalOrderbook::from_snapshot(
            self.exchange.clone(),
            symbol.clone(),
            bids.clone(),
            asks.clone(),
            last_update_id,
            self.config.update_buffer_size,
        );

        let mut orderbooks = self.orderbooks.write().await;
        orderbooks.insert(symbol.clone(), Arc::new(local_orderbook));

        tracing::debug!(
            "[WS Snapshot] {}/{} initialized (legacy): lastUpdateId={}, bids={} levels, asks={} levels, best_bid={}, best_ask={}",
            self.exchange,
            symbol,
            last_update_id,
            bids.len(),
            asks.len(),
            best_bid_str,
            best_ask_str
        );
    }

    /// Apply a delta update to a local orderbook
    ///
    /// Note: Uses per-symbol locking - concurrent updates for different symbols are allowed
    pub async fn apply_update(
        &self,
        symbol: &str,
        first_update_id: u64,
        final_update_id: u64,
        previous_id: u64,
        bid_updates: Vec<OrderbookLevel>,
        ask_updates: Vec<OrderbookLevel>,
        is_incremental_delta: bool,
    ) -> anyhow::Result<()> {
        // Get orderbook reference with READ lock (allows concurrent lookups for different symbols)
        let orderbook_arc = {
            let orderbooks = self.orderbooks.read().await;
            orderbooks.get(symbol).cloned()
        }; // READ lock released immediately

        if let Some(orderbook) = orderbook_arc {
            // LocalOrderbook handles its own locking internally
            match orderbook.apply_delta(
                first_update_id,
                final_update_id,
                previous_id,
                bid_updates,
                ask_updates,
                is_incremental_delta,
            ) {
                Ok(applied) => {
                    if applied {
                        self.updates_received.fetch_add(1, Ordering::SeqCst);
                        *self.last_update_at.write().await = Some(Utc::now());
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
        // Get orderbook reference with READ lock (allows concurrent lookups for different symbols)
        let orderbook_arc = {
            let orderbooks = self.orderbooks.read().await;
            orderbooks.get(symbol).cloned()
        }; // READ lock released immediately

        // LocalOrderbook handles its own locking internally
        orderbook_arc.map(|orderbook| orderbook.to_orderbook(depth))
    }

    /// Check if orderbook is fresh (not stale)
    pub async fn is_fresh(&self, symbol: &str) -> bool {
        // Get orderbook reference with READ lock
        let orderbook_arc = {
            let orderbooks = self.orderbooks.read().await;
            orderbooks.get(symbol).cloned()
        };

        if let Some(orderbook) = orderbook_arc {
            // LocalOrderbook handles its own locking internally
            let timestamp = orderbook.get_timestamp();
            let age = Utc::now()
                .signed_duration_since(timestamp)
                .to_std()
                .unwrap_or_default();
            age < self.config.staleness_threshold
        } else {
            false
        }
    }

    /// True if the last applied update left the book crossed (best_bid >= best_ask) —
    /// a stale price level that never received its removal/update message. Callers use
    /// this to force a REST resync instead of waiting on the exchange's own periodic
    /// snapshot cadence.
    pub async fn is_crossed(&self, symbol: &str) -> bool {
        let orderbook_arc = {
            let orderbooks = self.orderbooks.read().await;
            orderbooks.get(symbol).cloned()
        };
        orderbook_arc.map_or(false, |orderbook| orderbook.is_crossed())
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

        tracing::info!("[{}] Removed local orderbook for {}", self.exchange, symbol);
    }

    /// Shutdown the manager
    pub async fn shutdown(&self) {
        self.active.store(false, Ordering::SeqCst);
        tracing::info!("[{}] OrderbookManager shutting down", self.exchange);
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
            "binance".to_string(),
            "BTC".to_string(),
            bids,
            asks,
            12345,
            100, // buffer_size
        );

        assert_eq!(orderbook.data.read().exchange, "binance");
        assert_eq!(orderbook.data.read().symbol, "BTC");
        assert_eq!(orderbook.data.read().bids.len(), 2);
        assert_eq!(orderbook.data.read().asks.len(), 2);
        assert_eq!(orderbook.data.read().last_update_id, 12345);
        assert_eq!(orderbook.data.read().buffer_size, 100);
    }

    #[test]
    fn test_apply_delta_update() {
        let orderbook = LocalOrderbook::from_snapshot(
            "binance".to_string(),
            "BTC".to_string(),
            vec![OrderbookLevel {
                price: dec!(100.0),
                quantity: dec!(1.0),
            }],
            vec![OrderbookLevel {
                price: dec!(101.0),
                quantity: dec!(1.0),
            }],
            12345,
            100,
        );

        // Apply valid delta update (Rule 6: strict sequence)
        let result = orderbook.apply_delta(
            12346,
            12346,
            0, // previous_id (not used for this exchange)
            vec![OrderbookLevel {
                price: dec!(100.0),
                quantity: dec!(2.0), // Update existing
            }],
            vec![OrderbookLevel {
                price: dec!(102.0),
                quantity: dec!(1.0), // Add new level
            }],
            false, // full price mode
        );

        assert!(result.is_ok());
        assert!(result.unwrap());
        assert_eq!(orderbook.data.read().last_update_id, 12346);
        assert_eq!(
            orderbook.data.read().bids.get(&dec!(100.0)),
            Some(&dec!(2.0))
        );
        assert_eq!(
            orderbook.data.read().asks.get(&dec!(102.0)),
            Some(&dec!(1.0))
        );
    }

    #[test]
    fn test_apply_delta_removes_zero_quantity() {
        let orderbook = LocalOrderbook::from_snapshot(
            "binance".to_string(),
            "BTC".to_string(),
            vec![OrderbookLevel {
                price: dec!(100.0),
                quantity: dec!(1.0),
            }],
            vec![],
            12345,
            100,
        );

        // Apply delta that removes level (Rule 6: strict sequence)
        let result = orderbook.apply_delta(
            12346,
            12346,
            0, // previous_id
            vec![OrderbookLevel {
                price: dec!(100.0),
                quantity: Decimal::ZERO, // Remove
            }],
            vec![],
            false, // full price mode
        );

        assert!(result.is_ok());
        assert!(orderbook.data.read().bids.is_empty());
    }

    #[test]
    fn test_apply_delta_incremental_arithmetic() {
        let orderbook = LocalOrderbook::from_snapshot(
            "binance".to_string(),
            "BTC".to_string(),
            vec![OrderbookLevel {
                price: dec!(100.0),
                quantity: dec!(1.0),
            }],
            vec![],
            12345,
            100,
        );

        // Apply delta that reduces quantity (Rule 6: strict sequence)
        let result = orderbook.apply_delta(
            12346,
            12346,
            0, // previous_id
            vec![OrderbookLevel {
                price: dec!(100.0),
                quantity: dec!(-0.5), // Reduce by 0.5: 1.0 - 0.5 = 0.5
            }],
            vec![],
            true, // incremental delta mode
        );

        assert!(result.is_ok());
        assert_eq!(
            orderbook.data.read().bids.get(&dec!(100.0)),
            Some(&dec!(0.5))
        );

        // Apply delta that removes level via incremental reduction
        let result2 = orderbook.apply_delta(
            12347,
            12347,
            0, // previous_id
            vec![OrderbookLevel {
                price: dec!(100.0),
                quantity: dec!(-0.6), // Reduce by 0.6: 0.5 - 0.6 = -0.1 (delete)
            }],
            vec![],
            true, // incremental delta mode
        );

        assert!(result2.is_ok());
        assert!(orderbook.data.read().bids.is_empty());
    }

    #[test]
    fn test_apply_delta_gap_without_previous_id_in_incremental_mode() {
        let orderbook = LocalOrderbook::from_snapshot(
            "binance".to_string(),
            "BTC".to_string(),
            vec![],
            vec![],
            12345,
            100,
        );

        // Apply update with gap when previous_id=0 (no gap detection)
        // Gap detection only works when previous_id is provided
        let result = orderbook.apply_delta(
            12350, // Gap: expected 12346, got 12350
            12350,
            0, // previous_id=0 means no gap detection
            vec![],
            vec![],
            true, // incremental delta mode
        );

        // Without previous_id, gaps are not detected - update is applied
        assert!(result.is_ok());
        assert!(result.unwrap()); // Should be applied (no gap detection)
        assert_eq!(orderbook.data.read().last_update_id, 12350); // Updated
    }

    #[test]
    fn test_apply_delta_gap_without_previous_id_in_full_price_mode() {
        let orderbook = LocalOrderbook::from_snapshot(
            "binance".to_string(),
            "BTC".to_string(),
            vec![],
            vec![],
            12345,
            100,
        );

        // Apply update with gap when previous_id=0 (no gap detection)
        // Gap detection only works when previous_id is provided
        let result = orderbook.apply_delta(
            12350, // Gap: expected 12346, got 12350
            12350,
            0, // previous_id=0 means no gap detection
            vec![],
            vec![],
            false, // full price mode
        );

        // Without previous_id, gaps are not detected - update is applied
        assert!(result.is_ok());
        assert!(result.unwrap()); // Should be applied (no gap detection)
        assert_eq!(orderbook.data.read().last_update_id, 12350); // Updated
    }

    #[test]
    fn test_apply_delta_drops_stale() {
        let orderbook = LocalOrderbook::from_snapshot(
            "binance".to_string(),
            "BTC".to_string(),
            vec![],
            vec![],
            12345,
            100,
        );

        // Try to apply stale update
        let result = orderbook.apply_delta(12344, 12344, 0, vec![], vec![], false);

        assert!(result.is_ok());
        assert!(!result.unwrap()); // Should be dropped
        assert_eq!(orderbook.data.read().last_update_id, 12345); // Unchanged
    }

    #[test]
    fn test_apply_delta_previous_id_validation() {
        let orderbook = LocalOrderbook::from_snapshot(
            "binance".to_string(),
            "BTC".to_string(),
            vec![],
            vec![],
            12345,
            100,
        );

        // Valid previous_id (matches last_update_id) - Rule 6
        let result = orderbook.apply_delta(
            12346,
            12346,
            12345, // previous_id matches last_update_id
            vec![],
            vec![],
            false,
        );

        assert!(result.is_ok());
        assert_eq!(orderbook.data.read().last_update_id, 12346);

        // Invalid previous_id (doesn't match last_update_id)
        let result2 = orderbook.apply_delta(
            12347,
            12347,
            12340, // previous_id doesn't match (should be 12346)
            vec![],
            vec![],
            false,
        );

        assert!(result2.is_err()); // Should error on previous_id mismatch
        assert_eq!(orderbook.data.read().last_update_id, 12346); // Unchanged
    }

    #[test]
    fn test_bootstrap_seam_exemption_requires_tolerate_flag() {
        // Without tolerate_snapshot_seam (the default - Binance/Aster/etc), a snapshot whose
        // replay bridges zero buffered events must still enforce Rule 4 strictly: this is
        // exactly what a real missed delta right after bootstrap looks like for those
        // exchanges, and must still force a reconnect.
        let orderbook = LocalOrderbook::new_empty("binance".to_string(), "BTC".to_string(), 100);
        let replayed = orderbook.apply_snapshot(vec![], vec![], 1000).unwrap();
        assert_eq!(replayed, 0);

        let result = orderbook.apply_delta(1005, 1005, 1002, vec![], vec![], false);
        assert!(
            result.is_err(),
            "non-Nado exchanges must not get the seam exemption"
        );
    }

    #[test]
    fn test_bootstrap_seam_exemption_applies_once_for_nado() {
        // Reproduces the exact live-captured Nado sequence: market_liquidity's snapshot
        // timestamp is a wall-clock sample, not a checkpoint on the WS delta chain, so when
        // the only buffered event predates it (replayed=0), the next live delta's previous_id
        // legitimately references that earlier discarded event instead of the snapshot's own
        // timestamp - a false-positive Rule 4 with zero actual data loss.
        let orderbook = LocalOrderbook::new_empty("nado".to_string(), "DOGE".to_string(), 100)
            .with_tolerate_snapshot_seam(true);
        let replayed = orderbook.apply_snapshot(vec![], vec![], 1000).unwrap();
        assert_eq!(replayed, 0);

        // previous_id=997 doesn't match snapshot_lastUpdateId=1000, but is exempted exactly once.
        let first = orderbook.apply_delta(1005, 1005, 997, vec![], vec![], false);
        assert!(
            first.is_ok(),
            "first post-snapshot delta must be exempted from Rule 4"
        );
        assert_eq!(orderbook.data.read().last_update_id, 1005);

        // The exemption does not persist: a genuine gap on the very next delta must still fail.
        let second = orderbook.apply_delta(1010, 1010, 1006, vec![], vec![], false);
        assert!(
            second.is_err(),
            "exemption must not carry over past the first delta"
        );
        assert_eq!(orderbook.data.read().last_update_id, 1005); // unchanged
    }

    #[test]
    fn test_apply_snapshot_with_straddling_event() {
        // Test Binance Step 5: "The first processed event should have U <= lastUpdateId AND u >= lastUpdateId"
        let orderbook = LocalOrderbook::new_empty("binance".to_string(), "BTC".to_string(), 100);

        // Simulate buffering before snapshot
        // Event 1: U=995, u=998 (entirely before snapshot)
        orderbook
            .apply_delta(
                995,
                998,
                0,
                vec![OrderbookLevel {
                    price: dec!(100.0),
                    quantity: dec!(1.0),
                }],
                vec![],
                false,
            )
            .ok();

        // Event 2: U=999, u=1002 (STRADDLES snapshot at 1000)
        orderbook
            .apply_delta(
                999,
                1002,
                0,
                vec![OrderbookLevel {
                    price: dec!(101.0),
                    quantity: dec!(2.0),
                }],
                vec![],
                false,
            )
            .ok();

        // Event 3: U=1003, u=1005 (entirely after snapshot)
        orderbook
            .apply_delta(
                1003,
                1005,
                0,
                vec![OrderbookLevel {
                    price: dec!(102.0),
                    quantity: dec!(3.0),
                }],
                vec![],
                false,
            )
            .ok();

        // Apply snapshot with lastUpdateId=1000
        let snapshot_bids = vec![OrderbookLevel {
            price: dec!(99.0),
            quantity: dec!(5.0),
        }];
        let result = orderbook.apply_snapshot(snapshot_bids, vec![], 1000);

        assert!(result.is_ok());
        let replayed = result.unwrap();

        // Should replay 2 events: the straddling event (U=999, u=1002) and the after event (U=1003, u=1005)
        assert_eq!(replayed, 2, "Should replay straddling event + after event");

        // Verify final state contains updates from both replayed events
        assert_eq!(
            orderbook.data.read().bids.get(&dec!(101.0)),
            Some(&dec!(2.0)),
            "Straddling event should be applied"
        );
        assert_eq!(
            orderbook.data.read().bids.get(&dec!(102.0)),
            Some(&dec!(3.0)),
            "After event should be applied"
        );

        // Event entirely before snapshot should not be in final state (was skipped)
        assert_eq!(
            orderbook.data.read().bids.get(&dec!(100.0)),
            None,
            "Pre-snapshot event should be skipped"
        );

        // Snapshot data should be present
        assert_eq!(
            orderbook.data.read().bids.get(&dec!(99.0)),
            Some(&dec!(5.0)),
            "Snapshot data should be present"
        );

        // Final last_update_id should be from the last replayed event
        assert_eq!(orderbook.data.read().last_update_id, 1005);
    }

    #[test]
    fn test_to_orderbook_with_depth() {
        let orderbook = LocalOrderbook::from_snapshot(
            "binance".to_string(),
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
            100,
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
