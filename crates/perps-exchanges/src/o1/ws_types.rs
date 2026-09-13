use serde::Deserialize;

/// Inner payload of a `wss://zo-mainnet.n1.xyz/ws/deltas@{symbol}` frame
/// (`{"delta": {...}}`).
///
/// Confirmed live 2026-09-13: this is a genuine incremental delta, not a full snapshot -
/// the first frame after connecting carries only a handful of changed levels, far fewer
/// than the REST `/market/{id}/orderbook` snapshot size. Each price level's `size` is the
/// new ABSOLUTE quantity at that price (remove the level when `size == 0.0`), not a delta
/// to add - same "full price mode" semantics as `EdgexWsClient`'s `DepthBook`.
#[derive(Debug, Clone, Deserialize)]
pub struct NordWsDelta {
    pub last_update_id: u64,
    pub update_id: u64,
    pub market_symbol: String,
    pub bids: Vec<(f64, f64)>,
    pub asks: Vec<(f64, f64)>,
}

/// Inner payload of a `wss://zo-mainnet.n1.xyz/ws/trades@{symbol}` frame
/// (`{"trades": {...}}`).
///
/// Confirmed live 2026-09-13: unlike the REST `/trades` endpoint's flat `NordTrade`, the WS
/// frame batches trades under a shared `market_symbol`, and its per-trade field names
/// differ from `NordTrade`: `side` (not `taker_side`), `size` (not `base_size`),
/// `physical_time` (not `time`).
#[derive(Debug, Clone, Deserialize)]
pub struct NordWsTradesBatch {
    pub market_symbol: String,
    pub trades: Vec<NordWsTrade>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NordWsTrade {
    pub trade_id: u64,
    pub side: String,
    pub price: f64,
    pub size: f64,
    pub physical_time: String,
}
