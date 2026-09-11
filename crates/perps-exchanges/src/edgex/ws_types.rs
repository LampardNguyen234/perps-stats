//! Message types for EdgeX's public WebSocket API
//! (`wss://edgex-quote-prod-v2.edgex.exchange/api/v1/public/ws`).
//!
//! Field shapes here are confirmed against a live capture (2026-09-11, see
//! `docs/plan/edgex_websocket/00_requirements.md`), not the gitbook docs alone - the docs'
//! implied `ticker.all.1s` shape (differently-cased fields, a `predictedFundingRate` field)
//! did not match the wire; this file models what was actually observed.

use serde::Deserialize;

/// Top-level message envelope. `channel`/`content` are absent on `connected`/`ping`/
/// `subscribed`/`unsubscribed` frames - modeled with `#[serde(default)]` so those frames
/// deserialize without error rather than being treated as malformed (same defensive-envelope
/// lesson as Arcus, `docs/retro.md` entry 3).
#[derive(Debug, Deserialize)]
pub struct EdgexWsEnvelope {
    #[serde(rename = "type")]
    pub msg_type: String, // "connected" | "subscribed" | "unsubscribed" | "quote-event" | "ping" | "error"
    #[serde(default)]
    pub channel: String,
    /// Present on "ping" frames; echoed verbatim in the client's "pong" reply.
    #[serde(default)]
    pub time: Option<String>,
    #[serde(default)]
    pub content: Option<EdgexWsContent>,
}

/// `content` payload of a `"quote-event"` message. `data_type` is `"Snapshot"` (initial) or
/// `"changed"` (updates) - confirmed live: capitalized only on the first word, unlike the
/// nested per-record `depthType` field on depth records, which is `"SNAPSHOT"`/`"CHANGED"`
/// (all-caps). Match on `data_type` here, not `depthType`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgexWsContent {
    pub data_type: String,
    pub data: serde_json::Value,
}

/// `ticker.all.1s` element. Confirmed live to be near-identical to REST's `TickerRecord`
/// (`crate::edgex::types::TickerRecord`) - not the differently-cased/predicted-rate-carrying
/// shape originally guessed from the docs. No `predictedFundingRate`/`forecastFundingRate`
/// field exists on this payload (checked exhaustively across every contract in a live
/// capture); `predicted_rate` is derived at the conversion site as a fallback to
/// `funding_rate`, not parsed from here.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WsTickerRecord {
    pub contract_id: String,
    pub price_change: String,
    pub price_change_percent: String,
    pub size: String,
    pub value: String,
    pub high: String,
    pub low: String,
    pub end_time: String,
    pub last_price: String,
    pub index_price: String,
    pub mark_price: String,
    pub open_interest: String,
    pub funding_rate: String,
    pub funding_time: String,
    pub next_funding_time: String,
    /// Absent on illiquid/no-book contracts (confirmed live: e.g. `EURUSDC` with an
    /// all-zero record) - not just a documentation gap, a small fraction of contracts
    /// genuinely omit these two fields on some cycles. Treated as "no book" (zero
    /// price/qty) at the conversion site, not a parse error - a missing field on one
    /// contract must not fail the whole `ticker.all.1s` batch for every other symbol.
    #[serde(default)]
    pub best_bid_price: Option<String>,
    #[serde(default)]
    pub best_ask_price: Option<String>,
    pub market_open: bool,
}

/// One `[price, size]` book level as sent over `depth.{contractId}.{level}`.
#[derive(Debug, Clone, Deserialize)]
pub struct WsBookLevel {
    pub price: String,
    pub size: String,
}

/// `depth.{contractId}.{level}` element. `start_version`/`end_version` are confirmed
/// present on every frame (contra the original "no sequence field" assumption) and are used
/// only for a best-effort continuity `warn!` in `DepthBook`, never for correctness - the
/// `changed` semantics are confirmed full-level replacement, not incremental deltas, so a
/// missed frame degrades gracefully rather than corrupting the book.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WsDepthRecord {
    pub contract_id: String,
    pub start_version: String,
    pub end_version: String,
    pub bids: Vec<WsBookLevel>,
    pub asks: Vec<WsBookLevel>,
}

/// `trades.{contractId}` element. Confirmed live to carry `isBuyerMaker: bool`, not a
/// `"BUY"`/`"SELL"` string as originally assumed - see `ws_client.rs::ws_trade_to_trade`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WsTradeRecord {
    pub ticket_id: String,
    pub contract_id: String,
    pub price: String,
    pub size: String,
    pub time: String,
    pub is_buyer_maker: bool,
}

/// `kline.{priceType}.{contractId}.{interval}` element. Field names confirmed live to match
/// REST's `KlineRecord` (`crate::edgex::types::KlineRecord`) exactly.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WsKlineRecord {
    pub contract_id: String,
    pub kline_time: String,
    pub open: String,
    pub high: String,
    pub low: String,
    pub close: String,
    pub size: String,
    pub value: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_deserializes_connected_frame_without_channel_or_content() {
        let raw = r#"{"sid":"e86b41b9-f039-d6e7-5d46-517993ed824f","type":"connected"}"#;
        let envelope: EdgexWsEnvelope = serde_json::from_str(raw).unwrap();
        assert_eq!(envelope.msg_type, "connected");
        assert_eq!(envelope.channel, "");
        assert!(envelope.content.is_none());
    }

    #[test]
    fn envelope_deserializes_ping_frame() {
        let raw = r#"{"type":"ping","time":"1789136310001"}"#;
        let envelope: EdgexWsEnvelope = serde_json::from_str(raw).unwrap();
        assert_eq!(envelope.msg_type, "ping");
        assert_eq!(envelope.time.as_deref(), Some("1789136310001"));
    }

    #[test]
    fn envelope_deserializes_subscribed_frame() {
        let raw = r#"{"type":"subscribed","channel":"ticker.all.1s","request":"{}"}"#;
        let envelope: EdgexWsEnvelope = serde_json::from_str(raw).unwrap();
        assert_eq!(envelope.msg_type, "subscribed");
        assert_eq!(envelope.channel, "ticker.all.1s");
    }

    #[test]
    fn ws_ticker_record_deserializes_live_shape() {
        let raw = r#"{
            "contractId":"30000001","contractName":"BTCUSDC","priceChange":"2148.2",
            "priceChangePercent":"0.027941","trades":"127641","size":"8138.396",
            "value":"628869807.4804","high":"79824.9","low":"76031.9","open":"76883.0",
            "close":"79031.2","highTime":"1789135387098","lowTime":"1789129829005",
            "startTime":"1789047900000","endTime":"1789136100000","lastPrice":"79031.2",
            "indexPrice":"79076.6724223198","oraclePrice":"79025.030809853284955708",
            "markPrice":"79025.030809853284955708","openInterest":"3478.949",
            "fundingRate":"-0.00005754","fundingTime":"1789128000000",
            "nextFundingTime":"1789142400000","bestAskPrice":"79031.4",
            "bestBidPrice":"79031.1","marketOpen":true
        }"#;
        let record: WsTickerRecord = serde_json::from_str(raw).unwrap();
        assert_eq!(record.contract_id, "30000001");
        assert_eq!(record.best_bid_price.as_deref(), Some("79031.1"));
        assert_eq!(record.funding_time, "1789128000000");
        assert!(record.market_open);
    }

    #[test]
    fn ws_ticker_record_tolerates_missing_best_bid_ask() {
        // Confirmed live: illiquid/no-book contracts (e.g. EURUSDC) omit these fields
        // entirely on some cycles - must not fail to deserialize.
        let raw = r#"{
            "contractId":"30000162","priceChange":"0","priceChangePercent":"0",
            "size":"0","value":"0","high":"0","low":"0","endTime":"1789136100000",
            "lastPrice":"0","indexPrice":"1.1614238369","markPrice":"1.169107230317320739",
            "openInterest":"0","fundingRate":"0.00005000","fundingTime":"1789128000000",
            "nextFundingTime":"1789142400000","marketOpen":true,"liquidity":"HIGH"
        }"#;
        let record: WsTickerRecord = serde_json::from_str(raw).unwrap();
        assert!(record.best_bid_price.is_none());
        assert!(record.best_ask_price.is_none());
    }

    #[test]
    fn ws_depth_record_deserializes_live_shape() {
        let raw = r#"{
            "startVersion":"1797029327","endVersion":"1797029347","level":200,
            "contractId":"30000001","contractName":"BTCUSDC",
            "asks":[{"price":"79031.4","size":"0"}],
            "bids":[{"price":"79031.2","size":"0.720"}],
            "depthType":"CHANGED"
        }"#;
        let record: WsDepthRecord = serde_json::from_str(raw).unwrap();
        assert_eq!(record.start_version, "1797029327");
        assert_eq!(record.end_version, "1797029347");
        assert_eq!(record.bids[0].size, "0.720");
    }

    #[test]
    fn ws_trade_record_deserializes_live_shape() {
        let raw = r#"{
            "ticketId":"20a536e6-98eb-4bb4-927a-758e4a38f925","time":"1789136310088",
            "price":"79031.2","size":"0.007","value":"553.2184",
            "takerOrderId":"793295181930561942","makerOrderId":"793295181926367638",
            "takerAccountId":"676063439174411159","makerAccountId":"684354441117031794",
            "contractId":"30000001","contractName":"BTCUSDC","isBestMatch":true,
            "isBuyerMaker":false
        }"#;
        let record: WsTradeRecord = serde_json::from_str(raw).unwrap();
        assert_eq!(record.ticket_id, "20a536e6-98eb-4bb4-927a-758e4a38f925");
        assert!(!record.is_buyer_maker);
    }

    #[test]
    fn ws_kline_record_deserializes_live_shape() {
        let raw = r#"{
            "klineId":"2061584383781306426","contractId":"30000001",
            "contractName":"BTCUSDC","klineType":"MINUTE_1","klineTime":"1789136280000",
            "priceType":"LAST_PRICE","trades":"90","size":"4.077","value":"322191.4503",
            "high":"79054.5","low":"78984.8","open":"79050.5","close":"79031.3",
            "makerBuySize":"1.578","makerBuyValue":"124723.9125"
        }"#;
        let record: WsKlineRecord = serde_json::from_str(raw).unwrap();
        assert_eq!(record.kline_time, "1789136280000");
        assert_eq!(record.close, "79031.3");
    }
}
