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
    /// let buy_slippage = orderbook.get_slippage(dec!(5000), OrderSide::Buy);
    /// let sell_slippage = orderbook.get_slippage(dec!(5000), OrderSide::Sell);
    /// ```
    pub fn get_slippage(&self, amount: Decimal, side: OrderSide) -> Option<Decimal> {
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

        // Calculate slippage in bps
        let slippage = match side {
            OrderSide::Buy => (avg_price - mid_price) / mid_price,
            OrderSide::Sell => (mid_price - avg_price) / mid_price,
        };

        Some(slippage * Decimal::from(10000)) // Convert to bps
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
}
