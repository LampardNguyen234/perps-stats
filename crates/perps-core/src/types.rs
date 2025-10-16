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
            && self.index_price == Decimal::ZERO {
            return true
        }

        if self.volume_24h == Decimal::ZERO && self.turnover_24h == Decimal::ZERO {
            return true
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

    /// Calculate the spread between best bid and best ask in basis points (bps).
    /// Returns None if orderbook is empty.
    pub fn spread(&self) -> Option<Decimal> {
        let best_bid = self.best_bid()?;
        let best_ask = self.best_ask()?;

        if best_bid.is_zero() || best_ask.is_zero() {
            return None;
        }

        let mid_price = (best_bid + best_ask) / Decimal::TWO;
        let spread = best_ask - best_bid;

        Some((spread / mid_price) * Decimal::from(10000))
    }

    /// Calculate total notional value (price × quantity) for bid side within given bps spread.
    /// bps: basis points spread from mid price (e.g., 5 = 5 basis points = 0.05%)
    pub fn bid_notional(&self, bps: Decimal) -> Decimal {
        let Some(best_bid) = self.bids.first() else {
            return Decimal::ZERO;
        };
        let Some(best_ask) = self.asks.first() else {
            return Decimal::ZERO;
        };

        let mid_price = (best_bid.price + best_ask.price) / Decimal::TWO;
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
        let Some(best_bid) = self.bids.first() else {
            return Decimal::ZERO;
        };
        let Some(best_ask) = self.asks.first() else {
            return Decimal::ZERO;
        };

        let mid_price = (best_bid.price + best_ask.price) / Decimal::TWO;
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
        let bid_notional: Decimal = self.bids
            .iter()
            .map(|level| level.price * level.quantity)
            .sum();

        let ask_notional: Decimal = self.asks
            .iter()
            .map(|level| level.price * level.quantity)
            .sum();

        (bid_notional, ask_notional)
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
