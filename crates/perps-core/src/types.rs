use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OrderSide {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Market {
    pub symbol: String,
    pub contract: String,
    pub contract_size: Decimal,
    pub price_scale: i32,
    pub quantity_scale: i32,
    pub min_order_qty: Decimal,
    pub max_order_qty: Decimal,
    pub min_order_value: Decimal,
    pub max_leverage: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticker {
    pub symbol: String,
    pub last_price: Decimal,
    pub mark_price: Decimal,
    pub index_price: Decimal,
    pub best_bid_price: Decimal,
    pub best_bid_qty: Decimal,
    pub best_ask_price: Decimal,
    pub best_ask_qty: Decimal,
    pub volume_24h: Decimal,
    pub turnover_24h: Decimal,
    pub open_interest: Decimal,
    pub open_interest_notional: Decimal,
    pub price_change_24h: Decimal,
    pub price_change_pct: Decimal,
    pub high_price_24h: Decimal,
    pub low_price_24h: Decimal,
    pub timestamp: DateTime<Utc>,
}

impl Ticker {
    /// Returns true if the ticker has no meaningful data (all prices and volumes are zero).
    /// This is useful for filtering out incomplete ticker data from WebSocket streams
    /// that don't provide 24h statistics.
    pub fn is_empty(&self) -> bool {
        if self.last_price == Decimal::ZERO
            && self.mark_price == Decimal::ZERO
            && self.index_price == Decimal::ZERO
        {
            return true;
        }

        if self.volume_24h == Decimal::ZERO && self.turnover_24h == Decimal::ZERO {
            return true;
        }

        self.best_bid_price == Decimal::ZERO
            && self.best_ask_price == Decimal::ZERO
            && self.best_bid_qty == Decimal::ZERO
            && self.best_ask_qty == Decimal::ZERO
    }

    /// Returns true if the ticker has incomplete 24h statistics (volume and turnover are zero).
    /// This is common for WebSocket tickers from exchanges like KuCoin that don't provide
    /// 24h statistics in their WebSocket streams.
    pub fn has_incomplete_24h_stats(&self) -> bool {
        self.volume_24h == Decimal::ZERO && self.turnover_24h == Decimal::ZERO
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrderbookLevel {
    pub price: Decimal,
    pub quantity: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Orderbook {
    pub symbol: String,
    pub bids: Vec<OrderbookLevel>,
    pub asks: Vec<OrderbookLevel>,
    pub timestamp: DateTime<Utc>,
}

impl Orderbook {
    /// Get the best bid price (highest bid)
    pub fn best_bid(&self) -> Option<Decimal> {
        self.bids.first().map(|level| level.price)
    }

    /// Get the best ask price (lowest ask)
    pub fn best_ask(&self) -> Option<Decimal> {
        self.asks.first().map(|level| level.price)
    }

    /// Get the best bid quantity
    pub fn best_bid_qty(&self) -> Option<Decimal> {
        self.bids.first().map(|level| level.quantity)
    }

    /// Get the best ask quantity
    pub fn best_ask_qty(&self) -> Option<Decimal> {
        self.asks.first().map(|level| level.quantity)
    }

    /// Calculate the mid price (average of best bid and best ask).
    /// Returns None if orderbook is empty or missing either side.
    pub fn mid_price(&self) -> Option<Decimal> {
        let best_bid = self.best_bid()?;
        let best_ask = self.best_ask()?;
        Some((best_bid + best_ask) / Decimal::TWO)
    }

    /// Calculate the spread between best bid and best ask in basis points (bps).
    /// Returns None if orderbook is empty.
    pub fn spread(&self) -> Option<Decimal> {
        let best_bid = self.best_bid()?;
        let best_ask = self.best_ask()?;

        if best_bid.is_zero() || best_ask.is_zero() {
            return None;
        }

        let mid_price = self.mid_price()?;
        let spread = best_ask - best_bid;
        if spread < Decimal::ZERO {
            tracing::warn!(
                "Negative spread {}, best_bid {}, best_ask {}",
                spread,
                best_bid,
                best_ask
            );
        }

        Some((spread / mid_price) * Decimal::from(10000))
    }

    /// Calculate total notional value (price × quantity) for bid side within given bps spread.
    /// bps: basis points spread from mid price (e.g., 5 = 5 basis points = 0.05%)
    pub fn bid_notional(&self, bps: Decimal) -> Decimal {
        let Some(mid_price) = self.mid_price() else {
            return Decimal::ZERO;
        };

        let threshold = mid_price * (Decimal::ONE - bps / Decimal::from(10000));

        self.bids
            .iter()
            .filter(|level| level.price >= threshold)
            .map(|level| level.price * level.quantity)
            .sum()
    }

    /// Calculate total notional value (price × quantity) for ask side within given bps spread.
    /// bps: basis points spread from mid price (e.g., 5 = 5 basis points = 0.05%)
    pub fn ask_notional(&self, bps: Decimal) -> Decimal {
        let Some(mid_price) = self.mid_price() else {
            return Decimal::ZERO;
        };

        let threshold = mid_price * (Decimal::ONE + bps / Decimal::from(10000));

        self.asks
            .iter()
            .filter(|level| level.price <= threshold)
            .map(|level| level.price * level.quantity)
            .sum()
    }

    /// Returns the number of bid levels and ask levels.
    pub fn size(&self) -> (usize, usize) {
        (self.bids.len(), self.asks.len())
    }

    /// Calculate total notional value for all bids and all asks.
    /// Returns (total_bid_notional, total_ask_notional)
    pub fn total_notional(&self) -> (Decimal, Decimal) {
        let bid_notional: Decimal = self
            .bids
            .iter()
            .map(|level| level.price * level.quantity)
            .sum();

        let ask_notional: Decimal = self
            .asks
            .iter()
            .map(|level| level.price * level.quantity)
            .sum();

        (bid_notional, ask_notional)
    }

    /// Calculate slippage in basis points for a given trade amount and side.
    ///
    /// # Arguments
    /// * `amount` - Trade size in USD notional
    /// * `side` - OrderSide::Buy (executes against asks) or OrderSide::Sell (executes against bids)
    ///
    /// # Returns
    /// * `Some(slippage_bps)` - Slippage in basis points (e.g., 50 = 0.5% = 50 bps)
    /// * `None` - If orderbook is empty, insufficient liquidity, or mid price is zero
    ///
    /// # Examples
    /// ```
    /// use perps_core::{Orderbook, OrderbookLevel, OrderSide};
    /// use rust_decimal_macros::dec;
    /// use chrono::Utc;
    ///
    /// let orderbook = Orderbook {
    ///     symbol: "BTC".to_string(),
    ///     timestamp: Utc::now(),
    ///     bids: vec![
    ///         OrderbookLevel { price: dec!(100000), quantity: dec!(0.1) },
    ///     ],
    ///     asks: vec![
    ///         OrderbookLevel { price: dec!(100100), quantity: dec!(0.1) },
    ///     ],
    /// };
    ///
    /// let buy_slippage = orderbook.get_slippage(dec!(5000), OrderSide::Buy, None);
    /// let sell_slippage = orderbook.get_slippage(dec!(5000), OrderSide::Sell, None);
    ///
    /// // With taker fee (0.04% = 0.0004)
    /// let buy_slippage_with_fee = orderbook.get_slippage(dec!(5000), OrderSide::Buy, Some(dec!(0.0004)));
    /// ```
    pub fn get_slippage(&self, amount: Decimal, side: OrderSide, taker_fee: Option<Decimal>) -> Option<Decimal> {
        // Get mid price
        let mid_price = self.mid_price()?;
        if mid_price.is_zero() {
            return None;
        }

        // Select levels based on side
        let levels = match side {
            OrderSide::Buy => &self.asks,  // Buy executes against asks
            OrderSide::Sell => &self.bids, // Sell executes against bids
        };

        if levels.is_empty() {
            return None;
        }

        // Calculate average execution price
        let mut remaining_notional = amount;
        let mut total_base_qty = Decimal::ZERO;
        let mut total_cost = Decimal::ZERO;

        for level in levels {
            if remaining_notional <= Decimal::ZERO {
                break;
            }

            // Notional available at this level
            let level_notional = level.price * level.quantity;

            if level_notional >= remaining_notional {
                // This level can fill the remaining order
                let base_qty_needed = remaining_notional / level.price;
                total_base_qty += base_qty_needed;
                total_cost += remaining_notional;
                remaining_notional = Decimal::ZERO;
                break;
            } else {
                // Consume entire level
                total_base_qty += level.quantity;
                total_cost += level_notional;
                remaining_notional -= level_notional;
            }
        }

        // Check if trade is feasible
        if remaining_notional > Decimal::ZERO {
            // Insufficient liquidity
            return None;
        }

        // Calculate average execution price
        if total_base_qty.is_zero() {
            return None;
        }
        let avg_price = total_cost / total_base_qty;

        // Apply taker fee to execution price if provided
        let avg_price_with_fee = if let Some(fee) = taker_fee {
            match side {
                OrderSide::Buy => avg_price * (Decimal::ONE + fee),  // Buy: pay fee on top
                OrderSide::Sell => avg_price * (Decimal::ONE - fee), // Sell: receive less after fee
            }
        } else {
            avg_price
        };

        // Calculate slippage in bps (now includes fee impact)
        let slippage = match side {
            OrderSide::Buy => (avg_price_with_fee - mid_price) / mid_price,
            OrderSide::Sell => (mid_price - avg_price_with_fee) / mid_price,
        };

        Some(slippage * Decimal::from(10000)) // Convert to bps
    }
}

/// Multi-resolution orderbook for adaptive aggregation across different trade sizes.
///
/// # Problem
/// Single orderbook aggregation cannot satisfy both:
/// - Tight spreads (1-5 bps) - requires fine aggregation
/// - Large trades ($500K+) - requires coarse aggregation for sufficient depth
///
/// # Solution
/// Stores multiple orderbook resolutions in a vector (fine → medium → coarse order):
/// - **Index 0**: Finest resolution for tight spread calculations (1-5 bps liquidity depth)
/// - **Index 1**: Medium resolution for moderate spreads (10-20 bps) - if available
/// - **Index 2**: Coarsest resolution for large trade slippage ($500K-$10M) - if available
///
/// # Exchange Support
/// - **Hyperliquid**: 3 orderbooks (nSigFigs: 5, 4, 2)
/// - **Other exchanges**: 1 orderbook (single resolution)
///
/// # Design
/// - Vector is ordered from finest to coarsest resolution
/// - `best_for_tight_spreads()` returns first orderbook (finest)
/// - `best_for_large_trades()` returns last orderbook (coarsest)
/// - Empty vector is invalid and should not occur
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiResolutionOrderbook {
    pub symbol: String,
    pub timestamp: DateTime<Utc>,
    /// Orderbooks ordered from finest to coarsest resolution.
    /// Index 0 = finest, last index = coarsest.
    /// Must contain at least one orderbook.
    pub orderbooks: Vec<Orderbook>,
}

impl MultiResolutionOrderbook {
    /// Create from a single orderbook (for exchanges that don't support multi-resolution).
    pub fn from_single(orderbook: Orderbook) -> Self {
        Self {
            symbol: orderbook.symbol.clone(),
            timestamp: orderbook.timestamp,
            orderbooks: vec![orderbook],
        }
    }

    /// Create from multiple orderbooks ordered from finest to coarsest resolution.
    ///
    /// # Arguments
    /// * `symbol` - Trading pair symbol
    /// * `timestamp` - Orderbook timestamp
    /// * `orderbooks` - Vector of orderbooks ordered from finest to coarsest (must not be empty)
    ///
    /// # Panics
    /// Panics if orderbooks vector is empty
    pub fn from_multiple(
        symbol: String,
        timestamp: DateTime<Utc>,
        orderbooks: Vec<Orderbook>,
    ) -> Self {
        assert!(!orderbooks.is_empty(), "MultiResolutionOrderbook must contain at least one orderbook");
        Self {
            symbol,
            timestamp,
            orderbooks,
        }
    }

    /// Get the best orderbook for tight spread calculations (1-5 bps).
    /// Returns the finest resolution (first orderbook in vector).
    pub fn best_for_tight_spreads(&self) -> Option<&Orderbook> {
        self.orderbooks.first()
    }

    /// Get the best orderbook for large trade slippage calculations ($500K-$10M).
    /// Returns the coarsest resolution (last orderbook in vector).
    pub fn best_for_large_trades(&self) -> Option<&Orderbook> {
        self.orderbooks.last()
    }

    /// Get the number of available resolutions.
    pub fn resolution_count(&self) -> usize {
        self.orderbooks.len()
    }

    /// Get orderbook at specific index.
    pub fn get(&self, index: usize) -> Option<&Orderbook> {
        self.orderbooks.get(index)
    }

    /// Calculate slippage for a given trade amount and side, prioritizing orderbooks
    /// with tighter spreads (finer resolutions).
    ///
    /// # Strategy
    /// - Iterates through orderbooks in order (finest to coarsest = index 0 to N-1)
    /// - Orderbooks with smaller index typically have **tighter spreads** (better price discovery)
    /// - **Feasibility check**: Only considers orderbooks with sufficient liquidity to execute the full trade
    /// - Returns the slippage from the **first orderbook** that can execute the trade (has sufficient liquidity)
    /// - Falls back to coarser resolutions if finer ones cannot provide sufficient liquidity
    ///
    /// This prioritization ensures we always get slippage from the most accurate orderbook
    /// representation (finest price granularity) that can actually execute the trade.
    ///
    /// # Arguments
    /// * `amount` - Trade size in USD notional
    /// * `side` - OrderSide::Buy or OrderSide::Sell
    /// * `taker_fee` - Optional taker fee as decimal (e.g., 0.0004 for 0.04%)
    ///
    /// # Returns
    /// * `Some(slippage_bps)` - Slippage in basis points from the first feasible orderbook (includes fee impact if provided)
    /// * `None` - If **no resolution** has sufficient liquidity to execute the full trade
    ///
    /// # Example
    /// ```
    /// // For a multi-resolution orderbook with 2 resolutions:
    /// // - Index 0 (fine, tight spread): Can execute $10K trade -> slippage = 5 bps
    /// // - Index 1 (coarse, wider spread): Can execute $100K trade -> slippage = 3 bps
    /// // For $10K trade: Returns Some(5) (from index 0, uses finer resolution)
    /// // For $50K trade: Returns Some(3) (falls back to index 1, index 0 cannot execute)
    ///
    /// // With taker fee
    /// // For $10K trade with 0.04% fee: Returns Some(9) (5 bps slippage + 4 bps fee = 9 bps total)
    /// ```
    pub fn get_slippage(&self, amount: Decimal, side: OrderSide, taker_fee: Option<Decimal>) -> Option<Decimal> {
        // Iterate in order (finest to coarsest) and return first valid slippage
        self.orderbooks
            .iter()
            .find_map(|book| book.get_slippage(amount, side.clone(), taker_fee))
    }

    /// Calculate bid notional within given bps spread, automatically selecting
    /// the resolution with the highest liquidity.
    ///
    /// Tries all resolutions and returns the maximum bid notional found.
    /// This is useful for liquidity depth calculations where we want the
    /// most accurate representation of available liquidity.
    ///
    /// # Arguments
    /// * `bps` - Basis points spread from mid price (e.g., 5 = 0.05%)
    ///
    /// # Returns
    /// Maximum bid notional across all resolutions
    pub fn bid_notional(&self, bps: Decimal) -> Decimal {
        self.orderbooks
            .iter()
            .map(|book| book.bid_notional(bps))
            .max()
            .unwrap_or(Decimal::ZERO)
    }

    /// Calculate ask notional within given bps spread, automatically selecting
    /// the resolution with the highest liquidity.
    ///
    /// Tries all resolutions and returns the maximum ask notional found.
    /// This is useful for liquidity depth calculations where we want the
    /// most accurate representation of available liquidity.
    ///
    /// # Arguments
    /// * `bps` - Basis points spread from mid price (e.g., 5 = 0.05%)
    ///
    /// # Returns
    /// Maximum ask notional across all resolutions
    pub fn ask_notional(&self, bps: Decimal) -> Decimal {
        self.orderbooks
            .iter()
            .map(|book| book.ask_notional(bps))
            .max()
            .unwrap_or(Decimal::ZERO)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FundingRate {
    pub symbol: String,
    pub funding_rate: Decimal,
    pub predicted_rate: Decimal,
    pub funding_time: DateTime<Utc>,
    pub next_funding_time: DateTime<Utc>,
    pub funding_interval: i32, // in hours
    pub funding_rate_cap_floor: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenInterest {
    pub symbol: String,
    pub open_interest: Decimal,
    pub open_value: Decimal,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Kline {
    pub symbol: String,
    pub interval: String,
    pub open_time: DateTime<Utc>,
    pub close_time: DateTime<Utc>,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    pub volume: Decimal,
    pub turnover: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trade {
    pub id: String,
    pub symbol: String,
    pub price: Decimal,
    pub quantity: Decimal,
    pub side: OrderSide,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketStats {
    pub symbol: String,
    pub volume_24h: Decimal,
    pub turnover_24h: Decimal,
    pub open_interest: Decimal,
    pub funding_rate: Decimal,
    pub last_price: Decimal,
    pub mark_price: Decimal,
    pub index_price: Decimal,
    pub price_change_24h: Decimal,
    pub price_change_pct: Decimal,
    pub high_price_24h: Decimal,
    pub low_price_24h: Decimal,
    pub timestamp: DateTime<Utc>,
}

/// Represents total notional (price × qty) available within fixed spread
/// thresholds (in basis points) for both bid and ask sides.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct LiquidityDepthStats {
    pub timestamp: DateTime<Utc>,
    pub exchange: String,
    pub symbol: String,
    pub mid_price: Decimal,

    // Bid notionals
    pub bid_1bps: Decimal,
    pub bid_2_5bps: Decimal,
    pub bid_5bps: Decimal,
    pub bid_10bps: Decimal,
    pub bid_20bps: Decimal,

    // Ask notionals
    pub ask_1bps: Decimal,
    pub ask_2_5bps: Decimal,
    pub ask_5bps: Decimal,
    pub ask_10bps: Decimal,
    pub ask_20bps: Decimal,
}

/// Slippage calculation for a specific trade amount.
/// Represents the price impact of executing a market order of a given size.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Slippage {
    /// Symbol (normalized to global format, e.g., "BTC")
    pub symbol: String,

    /// Timestamp of calculation
    pub timestamp: DateTime<Utc>,

    /// Mid price (average of best bid and best ask)
    pub mid_price: Decimal,

    /// Trade amount in USD notional
    pub trade_amount: Decimal,

    /// Buy (ask) side metrics - executing a buy order against asks
    pub buy_avg_price: Option<Decimal>,
    pub buy_slippage_bps: Option<Decimal>,
    pub buy_slippage_pct: Option<Decimal>,
    pub buy_total_cost: Option<Decimal>,
    pub buy_feasible: bool,

    /// Sell (bid) side metrics - executing a sell order against bids
    pub sell_avg_price: Option<Decimal>,
    pub sell_slippage_bps: Option<Decimal>,
    pub sell_slippage_pct: Option<Decimal>,
    pub sell_total_cost: Option<Decimal>,
    pub sell_feasible: bool,
}

impl Slippage {
    /// Create a new slippage instance for when orderbook lacks liquidity
    pub fn infeasible(
        symbol: String,
        timestamp: DateTime<Utc>,
        mid_price: Decimal,
        trade_amount: Decimal,
    ) -> Self {
        Self {
            symbol,
            timestamp,
            mid_price,
            trade_amount,
            buy_avg_price: None,
            buy_slippage_bps: None,
            buy_slippage_pct: None,
            buy_total_cost: None,
            buy_feasible: false,
            sell_avg_price: None,
            sell_slippage_bps: None,
            sell_slippage_pct: None,
            sell_total_cost: None,
            sell_feasible: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_get_slippage_buy_single_level() {
        let orderbook = Orderbook {
            symbol: "BTC".to_string(),
            timestamp: Utc::now(),
            bids: vec![
                OrderbookLevel {
                    price: dec!(100000),
                    quantity: dec!(0.1),
                }, // $10,000
            ],
            asks: vec![
                OrderbookLevel {
                    price: dec!(100100),
                    quantity: dec!(0.1),
                }, // $10,010
            ],
        };

        // Mid price = (100000 + 100100) / 2 = 100050
        // Buy $5000 against asks at 100100
        // Slippage = (100100 - 100050) / 100050 * 10000 = ~4.9975 bps
        let slippage = orderbook.get_slippage(dec!(5000), OrderSide::Buy);
        assert!(slippage.is_some());
        let slippage_bps = slippage.unwrap();
        assert!(slippage_bps > dec!(4.99) && slippage_bps < dec!(5.01));
    }

    #[test]
    fn test_get_slippage_sell_single_level() {
        let orderbook = Orderbook {
            symbol: "BTC".to_string(),
            timestamp: Utc::now(),
            bids: vec![
                OrderbookLevel {
                    price: dec!(100000),
                    quantity: dec!(0.1),
                }, // $10,000
            ],
            asks: vec![
                OrderbookLevel {
                    price: dec!(100100),
                    quantity: dec!(0.1),
                }, // $10,010
            ],
        };

        // Mid price = 100050
        // Sell $5000 against bids at 100000
        // Slippage = (100050 - 100000) / 100050 * 10000 = ~4.9975 bps
        let slippage = orderbook.get_slippage(dec!(5000), OrderSide::Sell);
        assert!(slippage.is_some());
        let slippage_bps = slippage.unwrap();
        assert!(slippage_bps > dec!(4.99) && slippage_bps < dec!(5.01));
    }

    #[test]
    fn test_get_slippage_buy_multiple_levels() {
        let orderbook = Orderbook {
            symbol: "BTC".to_string(),
            timestamp: Utc::now(),
            bids: vec![OrderbookLevel {
                price: dec!(100000),
                quantity: dec!(0.1),
            }],
            asks: vec![
                OrderbookLevel {
                    price: dec!(100100),
                    quantity: dec!(0.05),
                }, // $5,005
                OrderbookLevel {
                    price: dec!(100200),
                    quantity: dec!(0.05),
                }, // $5,010
                OrderbookLevel {
                    price: dec!(100300),
                    quantity: dec!(0.05),
                }, // $5,015
            ],
        };

        // Buy $10,000 requires first two levels
        // Level 1: $5,005 at 100100
        // Level 2: $4,995 at 100200 (partial)
        // Avg price should be > 100100 and < 100200
        let slippage = orderbook.get_slippage(dec!(10000), OrderSide::Buy);
        assert!(slippage.is_some());
        let slippage_bps = slippage.unwrap();
        // Should have positive slippage
        assert!(slippage_bps > Decimal::ZERO);
    }

    #[test]
    fn test_get_slippage_sell_multiple_levels() {
        let orderbook = Orderbook {
            symbol: "BTC".to_string(),
            timestamp: Utc::now(),
            bids: vec![
                OrderbookLevel {
                    price: dec!(100000),
                    quantity: dec!(0.05),
                }, // $5,000
                OrderbookLevel {
                    price: dec!(99900),
                    quantity: dec!(0.05),
                }, // $4,995
                OrderbookLevel {
                    price: dec!(99800),
                    quantity: dec!(0.05),
                }, // $4,990
            ],
            asks: vec![OrderbookLevel {
                price: dec!(100100),
                quantity: dec!(0.1),
            }],
        };

        // Sell $10,000 requires first two levels plus partial third
        let slippage = orderbook.get_slippage(dec!(10000), OrderSide::Sell);
        assert!(slippage.is_some());
        let slippage_bps = slippage.unwrap();
        // Should have positive slippage
        assert!(slippage_bps > Decimal::ZERO);
    }

    #[test]
    fn test_get_slippage_insufficient_liquidity() {
        let orderbook = Orderbook {
            symbol: "BTC".to_string(),
            timestamp: Utc::now(),
            bids: vec![
                OrderbookLevel {
                    price: dec!(100000),
                    quantity: dec!(0.05),
                }, // $5,000
            ],
            asks: vec![
                OrderbookLevel {
                    price: dec!(100100),
                    quantity: dec!(0.05),
                }, // $5,005
            ],
        };

        // Try to buy $10,000 but only $5,005 available
        let slippage = orderbook.get_slippage(dec!(10000), OrderSide::Buy);
        assert!(slippage.is_none());

        // Try to sell $10,000 but only $5,000 available
        let slippage = orderbook.get_slippage(dec!(10000), OrderSide::Sell);
        assert!(slippage.is_none());
    }

    #[test]
    fn test_get_slippage_empty_orderbook() {
        let orderbook = Orderbook {
            symbol: "BTC".to_string(),
            timestamp: Utc::now(),
            bids: vec![],
            asks: vec![],
        };

        let slippage = orderbook.get_slippage(dec!(5000), OrderSide::Buy);
        assert!(slippage.is_none());

        let slippage = orderbook.get_slippage(dec!(5000), OrderSide::Sell);
        assert!(slippage.is_none());
    }

    #[test]
    fn test_get_slippage_one_sided_orderbook() {
        // Only bids, no asks
        let orderbook1 = Orderbook {
            symbol: "BTC".to_string(),
            timestamp: Utc::now(),
            bids: vec![OrderbookLevel {
                price: dec!(100000),
                quantity: dec!(0.1),
            }],
            asks: vec![],
        };

        let slippage = orderbook1.get_slippage(dec!(5000), OrderSide::Buy);
        assert!(slippage.is_none()); // Can't buy without asks

        let slippage = orderbook1.get_slippage(dec!(5000), OrderSide::Sell);
        assert!(slippage.is_none()); // Can't calculate mid price

        // Only asks, no bids
        let orderbook2 = Orderbook {
            symbol: "BTC".to_string(),
            timestamp: Utc::now(),
            bids: vec![],
            asks: vec![OrderbookLevel {
                price: dec!(100100),
                quantity: dec!(0.1),
            }],
        };

        let slippage = orderbook2.get_slippage(dec!(5000), OrderSide::Buy);
        assert!(slippage.is_none()); // Can't calculate mid price

        let slippage = orderbook2.get_slippage(dec!(5000), OrderSide::Sell);
        assert!(slippage.is_none()); // Can't sell without bids
    }

    #[test]
    fn test_get_slippage_zero_slippage() {
        // Create orderbook where execution price equals mid price
        // This happens when buy/sell at exactly mid price (theoretical case)
        let orderbook = Orderbook {
            symbol: "BTC".to_string(),
            timestamp: Utc::now(),
            bids: vec![OrderbookLevel {
                price: dec!(100000),
                quantity: dec!(1.0),
            }],
            asks: vec![OrderbookLevel {
                price: dec!(100000),
                quantity: dec!(1.0),
            }],
        };

        // Mid price = 100000
        // Buy/sell at exactly 100000 = 0 slippage
        let buy_slippage = orderbook.get_slippage(dec!(50000), OrderSide::Buy);
        assert!(buy_slippage.is_some());
        assert_eq!(buy_slippage.unwrap(), Decimal::ZERO);

        let sell_slippage = orderbook.get_slippage(dec!(50000), OrderSide::Sell);
        assert!(sell_slippage.is_some());
        assert_eq!(sell_slippage.unwrap(), Decimal::ZERO);
    }

    #[test]
    fn test_get_slippage_large_trade() {
        let orderbook = Orderbook {
            symbol: "BTC".to_string(),
            timestamp: Utc::now(),
            bids: vec![
                OrderbookLevel {
                    price: dec!(100000),
                    quantity: dec!(0.5),
                }, // $50,000
                OrderbookLevel {
                    price: dec!(99000),
                    quantity: dec!(0.5),
                }, // $49,500
                OrderbookLevel {
                    price: dec!(98000),
                    quantity: dec!(0.5),
                }, // $49,000
            ],
            asks: vec![
                OrderbookLevel {
                    price: dec!(101000),
                    quantity: dec!(0.5),
                }, // $50,500
                OrderbookLevel {
                    price: dec!(102000),
                    quantity: dec!(0.5),
                }, // $51,000
                OrderbookLevel {
                    price: dec!(103000),
                    quantity: dec!(0.5),
                }, // $51,500
            ],
        };

        // Large buy that spans all three ask levels
        let buy_slippage = orderbook.get_slippage(dec!(150000), OrderSide::Buy);
        assert!(buy_slippage.is_some());
        let buy_bps = buy_slippage.unwrap();
        // Should have significant slippage (> 1%)
        assert!(buy_bps > dec!(100)); // > 100 bps = > 1%

        // Large sell that spans all three bid levels
        let sell_slippage = orderbook.get_slippage(dec!(145000), OrderSide::Sell);
        assert!(sell_slippage.is_some());
        let sell_bps = sell_slippage.unwrap();
        // Should have significant slippage (> 1%)
        assert!(sell_bps > dec!(100)); // > 100 bps = > 1%
    }

    // MultiResolutionOrderbook tests
    #[test]
    fn test_multi_resolution_from_single() {
        let orderbook = Orderbook {
            symbol: "BTC".to_string(),
            timestamp: Utc::now(),
            bids: vec![OrderbookLevel {
                price: dec!(100000),
                quantity: dec!(0.1),
            }],
            asks: vec![OrderbookLevel {
                price: dec!(100100),
                quantity: dec!(0.1),
            }],
        };

        let multi = MultiResolutionOrderbook::from_single(orderbook.clone());

        assert_eq!(multi.symbol, "BTC");
        assert_eq!(multi.orderbooks.len(), 1);
        assert_eq!(multi.best_for_tight_spreads().unwrap().symbol, "BTC");
        assert_eq!(multi.best_for_large_trades().unwrap().symbol, "BTC");
        assert_eq!(multi.resolution_count(), 1);
    }

    #[test]
    fn test_multi_resolution_from_multiple() {
        let fine = Orderbook {
            symbol: "BTC".to_string(),
            timestamp: Utc::now(),
            bids: vec![
                OrderbookLevel {
                    price: dec!(100000),
                    quantity: dec!(0.1),
                },
                OrderbookLevel {
                    price: dec!(99990),
                    quantity: dec!(0.1),
                },
            ],
            asks: vec![
                OrderbookLevel {
                    price: dec!(100100),
                    quantity: dec!(0.1),
                },
                OrderbookLevel {
                    price: dec!(100110),
                    quantity: dec!(0.1),
                },
            ],
        };

        let coarse = Orderbook {
            symbol: "BTC".to_string(),
            timestamp: Utc::now(),
            bids: vec![
                OrderbookLevel {
                    price: dec!(100000),
                    quantity: dec!(1.0),
                },
                OrderbookLevel {
                    price: dec!(99000),
                    quantity: dec!(1.0),
                },
            ],
            asks: vec![
                OrderbookLevel {
                    price: dec!(101000),
                    quantity: dec!(1.0),
                },
                OrderbookLevel {
                    price: dec!(102000),
                    quantity: dec!(1.0),
                },
            ],
        };

        let multi = MultiResolutionOrderbook::from_multiple(
            "BTC".to_string(),
            Utc::now(),
            vec![fine.clone(), coarse.clone()],
        );

        assert_eq!(multi.symbol, "BTC");
        assert_eq!(multi.orderbooks.len(), 2);
        assert_eq!(multi.resolution_count(), 2);

        // Fine resolution should be first (best for tight spreads)
        let tight = multi.best_for_tight_spreads().unwrap();
        assert_eq!(tight.bids.len(), 2);
        assert_eq!(tight.bids[0].price, dec!(100000));

        // Coarse resolution should be last (best for large trades)
        let large = multi.best_for_large_trades().unwrap();
        assert_eq!(large.bids.len(), 2);
        assert_eq!(large.bids[0].quantity, dec!(1.0));
    }

    #[test]
    fn test_multi_resolution_get_slippage_prioritizes_fine() {
        // Fine orderbook (index 0): tight spread, good for small trades
        let fine = Orderbook {
            symbol: "BTC".to_string(),
            timestamp: Utc::now(),
            bids: vec![OrderbookLevel {
                price: dec!(100000),
                quantity: dec!(0.1),
            }],
            asks: vec![OrderbookLevel {
                price: dec!(100100),
                quantity: dec!(0.1),
            }],
        };

        // Coarse orderbook (index 1): wide spread, but has more depth
        let coarse = Orderbook {
            symbol: "BTC".to_string(),
            timestamp: Utc::now(),
            bids: vec![OrderbookLevel {
                price: dec!(100000),
                quantity: dec!(10.0),
            }],
            asks: vec![OrderbookLevel {
                price: dec!(105000),
                quantity: dec!(10.0),
            }],
        };

        let multi =
            MultiResolutionOrderbook::from_multiple("BTC".to_string(), Utc::now(), vec![fine.clone(), coarse.clone()]);

        // Small trade ($5K) - both orderbooks can handle it
        // Fine (index 0): ~5 bps
        // Coarse (index 1): ~2475 bps (much worse due to wide spread)
        // Should return ~5 bps from fine orderbook (prioritizes index 0)
        let multi_slippage = multi.get_slippage(dec!(5000), OrderSide::Buy);
        assert!(multi_slippage.is_some());

        // Verify it returns slippage from fine (index 0), not coarse
        let fine_slippage = fine.get_slippage(dec!(5000), OrderSide::Buy).unwrap();
        assert_eq!(multi_slippage.unwrap(), fine_slippage, "Should prioritize fine orderbook (index 0)");

        // Verify fine slippage is much better than coarse
        let coarse_slippage = coarse.get_slippage(dec!(5000), OrderSide::Buy).unwrap();
        assert!(fine_slippage < coarse_slippage, "Fine should have better slippage than coarse");
    }

    #[test]
    fn test_multi_resolution_get_slippage_only_coarse_has_depth() {
        // Fine orderbook: insufficient depth for large trades
        let fine = Orderbook {
            symbol: "BTC".to_string(),
            timestamp: Utc::now(),
            bids: vec![OrderbookLevel {
                price: dec!(100000),
                quantity: dec!(0.1),
            }],
            asks: vec![OrderbookLevel {
                price: dec!(100100),
                quantity: dec!(0.1),
            }],
        };

        // Coarse orderbook: enough depth for large trades
        let coarse = Orderbook {
            symbol: "BTC".to_string(),
            timestamp: Utc::now(),
            bids: vec![OrderbookLevel {
                price: dec!(99000),
                quantity: dec!(10.0),
            }],
            asks: vec![OrderbookLevel {
                price: dec!(101000),
                quantity: dec!(10.0),
            }],
        };

        let multi =
            MultiResolutionOrderbook::from_multiple("BTC".to_string(), Utc::now(), vec![fine, coarse]);

        // Large trade ($600K) - only coarse orderbook has enough depth
        // Fine can't handle it (only $10K depth), coarse can (>$1M depth)
        // Should return slippage from coarse orderbook
        let slippage = multi.get_slippage(dec!(600000), OrderSide::Buy);
        assert!(slippage.is_some());
        // Coarse orderbook has enough depth for $600K trade
        assert!(slippage.unwrap() > Decimal::ZERO);
    }

    #[test]
    fn test_multi_resolution_get_slippage_prioritizes_by_index() {
        // Medium resolution (index 0): good for medium-sized trades (20 bps spread)
        let medium = Orderbook {
            symbol: "BTC".to_string(),
            timestamp: Utc::now(),
            bids: vec![OrderbookLevel {
                price: dec!(100000),
                quantity: dec!(1.0),
            }],
            asks: vec![OrderbookLevel {
                price: dec!(100200),
                quantity: dec!(1.0),
            }],
        };

        // Coarse resolution (index 1): has depth but wider spread (100 bps spread)
        let coarse = Orderbook {
            symbol: "BTC".to_string(),
            timestamp: Utc::now(),
            bids: vec![OrderbookLevel {
                price: dec!(99500),
                quantity: dec!(5.0),
            }],
            asks: vec![OrderbookLevel {
                price: dec!(100500),
                quantity: dec!(5.0),
            }],
        };

        let multi = MultiResolutionOrderbook::from_multiple(
            "BTC".to_string(),
            Utc::now(),
            vec![medium.clone(), coarse.clone()],
        );

        // Trade size: $50K - both orderbooks can handle it
        // Medium (index 0): ~10 bps slippage (tight spread)
        // Coarse (index 1): ~50 bps slippage (wide spread)
        // Should pick medium (index 0, prioritized)
        let multi_slippage = multi.get_slippage(dec!(50000), OrderSide::Buy);
        assert!(multi_slippage.is_some());

        // Verify it returns slippage from medium (index 0), not coarse
        let medium_slippage = medium.get_slippage(dec!(50000), OrderSide::Buy).unwrap();
        let coarse_slippage = coarse.get_slippage(dec!(50000), OrderSide::Buy).unwrap();

        assert!(medium_slippage < coarse_slippage, "Medium should have lower slippage than coarse");
        assert_eq!(multi_slippage.unwrap(), medium_slippage, "Should prioritize medium (index 0)");
    }

    #[test]
    fn test_multi_resolution_bid_notional() {
        let fine = Orderbook {
            symbol: "BTC".to_string(),
            timestamp: Utc::now(),
            bids: vec![
                OrderbookLevel {
                    price: dec!(100000),
                    quantity: dec!(0.1),
                }, // $10K
                OrderbookLevel {
                    price: dec!(99900),
                    quantity: dec!(0.1),
                }, // $9.99K
            ],
            asks: vec![OrderbookLevel {
                price: dec!(100100),
                quantity: dec!(0.1),
            }],
        };

        let coarse = Orderbook {
            symbol: "BTC".to_string(),
            timestamp: Utc::now(),
            bids: vec![
                OrderbookLevel {
                    price: dec!(100000),
                    quantity: dec!(1.0),
                }, // $100K
                OrderbookLevel {
                    price: dec!(99000),
                    quantity: dec!(1.0),
                }, // $99K
            ],
            asks: vec![OrderbookLevel {
                price: dec!(101000),
                quantity: dec!(1.0),
            }],
        };

        let multi =
            MultiResolutionOrderbook::from_multiple("BTC".to_string(), Utc::now(), vec![fine, coarse]);

        // bid_notional should return the maximum across all resolutions
        let notional = multi.bid_notional(dec!(100)); // 100 bps = 1% spread
        // Coarse orderbook should provide more liquidity
        assert!(notional > dec!(100000)); // More than $100K
    }

    #[test]
    fn test_multi_resolution_ask_notional() {
        let fine = Orderbook {
            symbol: "BTC".to_string(),
            timestamp: Utc::now(),
            bids: vec![OrderbookLevel {
                price: dec!(100000),
                quantity: dec!(0.1),
            }],
            asks: vec![
                OrderbookLevel {
                    price: dec!(100100),
                    quantity: dec!(0.1),
                }, // $10.01K
                OrderbookLevel {
                    price: dec!(100200),
                    quantity: dec!(0.1),
                }, // $10.02K
            ],
        };

        let coarse = Orderbook {
            symbol: "BTC".to_string(),
            timestamp: Utc::now(),
            bids: vec![OrderbookLevel {
                price: dec!(99000),
                quantity: dec!(1.0),
            }],
            asks: vec![
                OrderbookLevel {
                    price: dec!(101000),
                    quantity: dec!(1.0),
                }, // $101K
                OrderbookLevel {
                    price: dec!(102000),
                    quantity: dec!(1.0),
                }, // $102K
            ],
        };

        let multi =
            MultiResolutionOrderbook::from_multiple("BTC".to_string(), Utc::now(), vec![fine, coarse]);

        // ask_notional should return the maximum across all resolutions
        let notional = multi.ask_notional(dec!(100)); // 100 bps = 1% spread
        // Coarse orderbook should provide more liquidity
        assert!(notional > dec!(100000)); // More than $100K
    }

    #[test]
    #[should_panic(expected = "MultiResolutionOrderbook must contain at least one orderbook")]
    fn test_multi_resolution_from_multiple_empty_panic() {
        MultiResolutionOrderbook::from_multiple("BTC".to_string(), Utc::now(), vec![]);
    }

    #[test]
    fn test_multi_resolution_get_slippage_feasibility_check() {
        // Fine orderbook: tight spread but very shallow depth ($10K total)
        let fine = Orderbook {
            symbol: "BTC".to_string(),
            timestamp: Utc::now(),
            bids: vec![OrderbookLevel {
                price: dec!(100000),
                quantity: dec!(0.1),
            }],
            asks: vec![OrderbookLevel {
                price: dec!(100100),
                quantity: dec!(0.1),
            }], // Total: $10,010
        };

        // Medium orderbook: moderate spread, moderate depth ($100K total)
        let medium = Orderbook {
            symbol: "BTC".to_string(),
            timestamp: Utc::now(),
            bids: vec![OrderbookLevel {
                price: dec!(99900),
                quantity: dec!(1.0),
            }],
            asks: vec![OrderbookLevel {
                price: dec!(100100),
                quantity: dec!(1.0),
            }], // Total: $100,100
        };

        // Coarse orderbook: wide spread but deep ($500K total)
        let coarse = Orderbook {
            symbol: "BTC".to_string(),
            timestamp: Utc::now(),
            bids: vec![OrderbookLevel {
                price: dec!(98000),
                quantity: dec!(5.0),
            }],
            asks: vec![OrderbookLevel {
                price: dec!(102000),
                quantity: dec!(5.0),
            }], // Total: $510,000
        };

        let multi = MultiResolutionOrderbook::from_multiple(
            "BTC".to_string(),
            Utc::now(),
            vec![fine.clone(), medium.clone(), coarse.clone()],
        );

        // Test 1: Small trade ($5K) - all resolutions feasible
        let small_trade = multi.get_slippage(dec!(5000), OrderSide::Buy);
        assert!(small_trade.is_some(), "Small trade should be feasible");
        // Should pick fine (index 0, prioritized)
        let fine_slippage = fine.get_slippage(dec!(5000), OrderSide::Buy).unwrap();
        assert_eq!(small_trade.unwrap(), fine_slippage, "Should pick fine orderbook (index 0) for small trade");

        // Test 2: Medium trade ($50K) - only medium and coarse feasible
        let medium_trade = multi.get_slippage(dec!(50000), OrderSide::Buy);
        assert!(medium_trade.is_some(), "Medium trade should be feasible");
        // Fine can't handle $50K (only $10K depth), so should pick medium
        assert!(fine.get_slippage(dec!(50000), OrderSide::Buy).is_none(), "Fine should be infeasible");
        let medium_slippage = medium.get_slippage(dec!(50000), OrderSide::Buy).unwrap();
        assert_eq!(
            medium_trade.unwrap(),
            medium_slippage,
            "Should pick medium orderbook when fine is infeasible"
        );

        // Test 3: Large trade ($200K) - only coarse feasible
        let large_trade = multi.get_slippage(dec!(200000), OrderSide::Buy);
        assert!(large_trade.is_some(), "Large trade should be feasible");
        // Both fine and medium can't handle $200K
        assert!(fine.get_slippage(dec!(200000), OrderSide::Buy).is_none(), "Fine should be infeasible");
        assert!(medium.get_slippage(dec!(200000), OrderSide::Buy).is_none(), "Medium should be infeasible");
        let coarse_slippage = coarse.get_slippage(dec!(200000), OrderSide::Buy).unwrap();
        assert_eq!(
            large_trade.unwrap(),
            coarse_slippage,
            "Should pick coarse orderbook when others are infeasible"
        );

        // Test 4: Impossible trade ($1M) - no resolution feasible
        let impossible_trade = multi.get_slippage(dec!(1000000), OrderSide::Buy);
        assert!(impossible_trade.is_none(), "Trade exceeding all resolutions should return None");
        // Verify all resolutions are indeed infeasible
        assert!(fine.get_slippage(dec!(1000000), OrderSide::Buy).is_none());
        assert!(medium.get_slippage(dec!(1000000), OrderSide::Buy).is_none());
        assert!(coarse.get_slippage(dec!(1000000), OrderSide::Buy).is_none());
    }
}
