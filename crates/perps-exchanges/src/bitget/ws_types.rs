use serde::Deserialize;

/// Live-confirmed push envelope (2026-10-27): `{"action":"snapshot"|"update","arg":{...},"data":[...],"ts":...}`.
#[derive(Debug, Deserialize, Clone)]
pub struct WsEnvelope<T> {
    pub action: String,
    pub arg: WsArg,
    pub data: Vec<T>,
    pub ts: i64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct WsArg {
    #[serde(rename = "instType")]
    pub inst_type: String,
    pub topic: String,
    pub symbol: String,
}

/// `ticker` channel push data.
#[derive(Debug, Deserialize, Clone)]
pub struct WsTickerData {
    #[serde(rename = "lastPrice")]
    pub last_price: String,
    #[serde(rename = "openPrice24h")]
    pub open_price_24h: String,
    #[serde(rename = "highPrice24h")]
    pub high_price_24h: String,
    #[serde(rename = "lowPrice24h")]
    pub low_price_24h: String,
    #[serde(rename = "bid1Price")]
    pub bid1_price: String,
    #[serde(rename = "ask1Price")]
    pub ask1_price: String,
    #[serde(rename = "bid1Size")]
    pub bid1_size: String,
    #[serde(rename = "ask1Size")]
    pub ask1_size: String,
    #[serde(rename = "price24hPcnt")]
    pub price_24h_pcnt: String,
    pub volume24h: String,
    pub turnover24h: String,
    #[serde(rename = "indexPrice")]
    pub index_price: String,
    #[serde(rename = "markPrice")]
    pub mark_price: String,
    #[serde(rename = "fundingRate")]
    pub funding_rate: String,
    #[serde(rename = "openInterest")]
    pub open_interest: String,
}

/// `books5` channel push data — always a full 5-level snapshot (no delta/resync logic
/// needed). `seq`/`pseq` arrive as raw JSON numbers live; kept as `serde_json::Value`
/// since they're unused (no incremental reconstruction) and their type isn't load-bearing.
#[derive(Debug, Deserialize, Clone)]
pub struct WsBookData {
    pub a: Vec<[serde_json::Value; 2]>,
    pub b: Vec<[serde_json::Value; 2]>,
    pub ts: String,
}

/// `publicTrade` channel push data.
#[derive(Debug, Deserialize, Clone)]
pub struct WsTradeData {
    pub i: String,
    pub p: String,
    pub v: String,
    #[serde(rename = "S")]
    pub side: String,
    #[serde(rename = "T")]
    pub ts: String,
}
