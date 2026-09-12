use crate::cache::ContractCache;
use crate::risex::ws_types::*;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::Utc;
use futures::{SinkExt, StreamExt};
use perps_core::streaming::*;
use perps_core::types::*;
use rust_decimal::Decimal;
use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;
use std::time::Duration;
use tokio_tungstenite::{connect_async, tungstenite::Message};

const WS_URL: &str = "wss://ws.rise.trade/ws";
const INACTIVITY_TIMEOUT_SECS: u64 = 30;

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

    fn reset_from_snapshot(
        &mut self,
        bid_levels: &[WsLevel],
        ask_levels: &[WsLevel],
    ) -> Result<()> {
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
            Self::apply_level(
                &mut self.bids,
                parse_price(&l.price)?,
                parse_price(&l.quantity)?,
            );
        }
        for l in ask_deltas {
            Self::apply_level(
                &mut self.asks,
                parse_price(&l.price)?,
                parse_price(&l.quantity)?,
            );
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
            .map(|(p, q)| OrderbookLevel {
                price: *p,
                quantity: *q,
            })
            .collect();
        let asks: Vec<OrderbookLevel> = self
            .asks
            .iter()
            .map(|(p, q)| OrderbookLevel {
                price: *p,
                quantity: *q,
            })
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
            // Sanity-check: catches gross scale errors (e.g. a missed/duplicated 10^18 wei
            // conversion), not tight enough to flag legitimately sub-cent tokens (PUMP trades
            // around $0.003-0.006 and is not a bug).
            let mid = (bid.price + ask.price) / Decimal::TWO;
            if mid < Decimal::new(1, 9) || mid > Decimal::new(1_000_000_000, 0) {
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

/// Parse one message using state owned exclusively by the current connection.
fn parse_orderbook(
    text: &str,
    symbols: &HashMap<u64, String>,
    states: &mut HashMap<u64, MarketState>,
) -> Result<Option<Orderbook>> {
    let envelope: WsMsgType = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    match envelope.msg_type.as_str() {
        "snapshot" => {
            let snapshot: WsSnapshot = serde_json::from_str(text)?;
            let id = snapshot.market_id.parse::<u64>()?;
            let Some(symbol) = symbols.get(&id) else {
                return Ok(None);
            };
            let mut state = MarketState::new(symbol.clone());
            state.reset_from_snapshot(&snapshot.data.bids, &snapshot.data.asks)?;
            let book = state.to_orderbook();
            states.insert(id, state);
            Ok(Some(book))
        }
        "update" => {
            let update: WsUpdate = serde_json::from_str(text)?;
            let id = update.market_id.parse::<u64>()?;
            let Some(state) = states.get_mut(&id) else {
                return Ok(None);
            };
            state.apply_update(&update.data.bids, &update.data.asks)?;
            if state.checksum() != update.checksum {
                return Err(anyhow!("RISEx checksum mismatch for {}", state.symbol));
            }
            Ok(Some(state.to_orderbook()))
        }
        _ => Ok(None),
    }
}

#[derive(Clone)]
pub struct RisexWsClient {
    market_id_cache: ContractCache<u64>,
}
impl RisexWsClient {
    pub fn new(market_id_cache: ContractCache<u64>) -> Self {
        Self { market_id_cache }
    }
}
#[async_trait]
impl IPerpsStream for RisexWsClient {
    fn get_name(&self) -> &str {
        "risex"
    }
    async fn stream_tickers(&self, _symbols: Vec<String>) -> Result<DataStream<Ticker>> {
        Err(anyhow!("RISEx ticker streaming is unsupported"))
    }
    async fn stream_trades(&self, _symbols: Vec<String>) -> Result<DataStream<Trade>> {
        Err(anyhow!("RISEx trade streaming is unsupported"))
    }
    async fn stream_orderbooks(&self, symbols: Vec<String>) -> Result<DataStream<Orderbook>> {
        let mut id_to_symbol = HashMap::new();
        let mut ids = Vec::new();
        for symbol in symbols {
            let id = self
                .market_id_cache
                .get(&symbol)
                .await
                .ok_or_else(|| anyhow!("RISEx market ID unavailable for {symbol}"))?;
            ids.push(id);
            id_to_symbol.insert(id, symbol);
        }
        let (mut socket, _) = connect_async(WS_URL).await?;
        socket
            .send(Message::Text(build_subscribe_msg(&ids)?))
            .await?;
        Ok(Box::pin(async_stream::try_stream! {
            let mut states = HashMap::new();
            loop {
                let message = tokio::time::timeout(Duration::from_secs(INACTIVITY_TIMEOUT_SECS), socket.next()).await?;
                match message {
                    Some(Ok(Message::Text(text))) => {
                        if let Some(book) = parse_orderbook(&text, &id_to_symbol, &mut states)? { yield book; }
                    }
                    Some(Ok(Message::Ping(payload))) => socket.send(Message::Pong(payload)).await?,
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(error)) => Err(error)?,
                    _ => {}
                }
            }
        }))
    }
    async fn stream_multi(&self, config: StreamConfig) -> Result<DataStream<StreamEvent>> {
        Ok(Box::pin(
            self.stream_orderbooks(config.symbols)
                .await?
                .map(|item| item.map(StreamEvent::Orderbook)),
        ))
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
            WsLevel {
                price: "95000".to_string(),
                quantity: "1.0".to_string(),
            },
            WsLevel {
                price: "94000".to_string(),
                quantity: "2.0".to_string(),
            },
            WsLevel {
                price: "96000".to_string(),
                quantity: "0.5".to_string(),
            },
        ];
        let asks = vec![
            WsLevel {
                price: "97000".to_string(),
                quantity: "1.0".to_string(),
            },
            WsLevel {
                price: "98000".to_string(),
                quantity: "1.5".to_string(),
            },
        ];
        state.reset_from_snapshot(&bids, &asks).unwrap();

        let ob = state.to_orderbook();

        // Bids descending
        assert!(
            ob.bids[0].price > ob.bids[1].price,
            "bids must be descending"
        );
        assert!(
            ob.bids[1].price > ob.bids[2].price,
            "bids must be descending"
        );

        // Asks ascending
        assert!(
            ob.asks[0].price < ob.asks[1].price,
            "asks must be ascending"
        );

        // No cross
        assert!(
            ob.bids[0].price < ob.asks[0].price,
            "book must not be crossed"
        );
    }

    #[test]
    fn market_state_delete_level_on_zero_qty() {
        let mut state = MarketState::new("BTC".to_string());
        let bids = vec![WsLevel {
            price: "95000".to_string(),
            quantity: "1.0".to_string(),
        }];
        state.reset_from_snapshot(&bids, &[]).unwrap();
        assert_eq!(state.bids.len(), 1);

        let delete = vec![WsLevel {
            price: "95000".to_string(),
            quantity: "0".to_string(),
        }];
        state.apply_update(&delete, &[]).unwrap();
        assert_eq!(state.bids.len(), 0, "zero qty must remove the level");
    }

    #[test]
    fn market_state_update_replaces_qty() {
        let mut state = MarketState::new("BTC".to_string());
        let bids = vec![WsLevel {
            price: "95000".to_string(),
            quantity: "1.0".to_string(),
        }];
        state.reset_from_snapshot(&bids, &[]).unwrap();

        let update = vec![WsLevel {
            price: "95000".to_string(),
            quantity: "3.0".to_string(),
        }];
        state.apply_update(&update, &[]).unwrap();

        let ob = state.to_orderbook();
        assert_eq!(
            ob.bids[0].quantity,
            dec!(3.0),
            "qty must be replaced by update"
        );
    }
}
