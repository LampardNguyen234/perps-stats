//! Public market-data WebSocket client for EdgeX
//! (`wss://edgex-quote-prod-v2.edgex.exchange/api/v1/public/ws`).
//!
//! Closest analogue is `crate::arcus::ws_client::ArcusWsClient`: a single
//! `async_stream::stream!` dispatch loop keyed on envelope `type` then `channel`. EdgeX
//! differs from Arcus in two ways: its heartbeat is an application-level JSON `ping`/`pong`
//! exchange (not a `tokio-tungstenite` transport-level ping), and - per a live capture taken
//! before writing this file (see `docs/plan/edgex_websocket/00_requirements.md`) -
//! `ticker.all.1s` already carries its own best-bid/ask, price-change, and funding-time
//! fields, so `DepthBook` is needed for `Orderbook` events and ticker *quantity* enrichment
//! only, not for ticker price fields or for gating ticker emission.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::{SinkExt, StreamExt};
use perps_core::streaming::{DataStream, IPerpsStream, StreamConfig, StreamDataType, StreamEvent};
use perps_core::{FundingRate, Kline, OrderSide, Orderbook, OrderbookLevel, Ticker, Trade};
use rust_decimal::Decimal;
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

use super::client::{to_edgex_symbol, to_global_symbol, EdgexClient};
use super::conversions::{datetime_from_ms, interval_duration_ms, parse_decimal, parse_ms, KLINE_INTERVALS};
use super::types::ContractMeta;
use super::ws_types::{
    EdgexWsContent, EdgexWsEnvelope, WsBookLevel, WsDepthRecord, WsKlineRecord, WsTickerRecord,
    WsTradeRecord,
};

const WS_BASE_URL: &str = "wss://edgex-quote-prod-v2.edgex.exchange/api/v1/public/ws";
const DEPTH_LEVEL: u32 = 200;

/// Public market-data WebSocket client for EdgeX.
///
/// Holds an owned `EdgexClient` purely to reuse its cached `contractName <-> contractId`
/// metadata resolution (`resolve_contract_id`/`find_contract_by_name`) - no second HTTP
/// client, cache, or rate limiter. `new()` performs no network I/O, matching every other
/// WS client here; the metadata fetch happens lazily on first `stream_*` call.
#[derive(Clone)]
pub struct EdgexWsClient {
    rest: EdgexClient,
    base_url: String,
}

impl EdgexWsClient {
    pub fn new() -> Self {
        Self {
            rest: EdgexClient::new(),
            base_url: WS_BASE_URL.to_string(),
        }
    }

    async fn connect_and_subscribe(
        &self,
        channels: &[String],
    ) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>> {
        tracing::info!("Connecting to EdgeX WebSocket: {}", self.base_url);
        let (mut stream, response) = connect_async(self.base_url.as_str())
            .await
            .context("failed to connect to EdgeX websocket")?;
        tracing::info!(
            "Connected to EdgeX WebSocket (status: {:?})",
            response.status()
        );

        for channel in channels {
            let text = subscribe_msg(channel.clone());
            stream
                .send(Message::Text(text))
                .await
                .with_context(|| format!("failed to subscribe to EdgeX channel {channel}"))?;
        }

        Ok(stream)
    }
}

impl Default for EdgexWsClient {
    fn default() -> Self {
        Self::new()
    }
}

fn subscribe_msg(channel: String) -> String {
    serde_json::json!({"type": "subscribe", "channel": channel}).to_string()
}

fn ticker_all_channel() -> String {
    "ticker.all.1s".to_string()
}

fn depth_channel(contract_id: &str) -> String {
    format!("depth.{contract_id}.{DEPTH_LEVEL}")
}

fn trades_channel(contract_id: &str) -> String {
    format!("trades.{contract_id}")
}

fn kline_channel(contract_id: &str, interval_token: &str) -> String {
    format!("kline.LAST_PRICE.{contract_id}.{interval_token}")
}

/// Dedupe and validate the requested symbol list (case-insensitively), preserving the
/// caller's own casing for the first occurrence of each symbol.
fn dedup_symbols(symbols: Vec<String>) -> Result<Vec<String>> {
    if symbols.is_empty() {
        return Err(anyhow!(
            "at least one symbol is required for EdgeX streaming"
        ));
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for symbol in symbols {
        if seen.insert(symbol.to_uppercase()) {
            out.push(symbol);
        }
    }
    Ok(out)
}

/// Per-symbol local order book maintained from `depth.{contractId}.200` `Snapshot`/`changed`
/// frames. Confirmed live (2026-09-11, `docs/plan/edgex_websocket/00_requirements.md`):
/// `changed` levels are full replacements (insert/replace on non-zero size, remove on
/// `size == "0"`), never incremental deltas - so a missed frame (see `last_end_version`)
/// degrades gracefully rather than corrupting the book.
#[derive(Default)]
struct DepthBook {
    bids: BTreeMap<Decimal, Decimal>,
    asks: BTreeMap<Decimal, Decimal>,
    last_end_version: Option<i64>,
}

fn apply_level(map: &mut BTreeMap<Decimal, Decimal>, level: &WsBookLevel) -> Result<()> {
    let price = parse_decimal(&level.price, "depth level price")?;
    let size = parse_decimal(&level.size, "depth level size")?;
    if size == Decimal::ZERO {
        map.remove(&price);
    } else {
        map.insert(price, size);
    }
    Ok(())
}

impl DepthBook {
    fn apply_snapshot(
        &mut self,
        bids: &[WsBookLevel],
        asks: &[WsBookLevel],
        end_version: i64,
    ) -> Result<()> {
        self.bids.clear();
        self.asks.clear();
        for level in bids {
            apply_level(&mut self.bids, level)?;
        }
        for level in asks {
            apply_level(&mut self.asks, level)?;
        }
        self.last_end_version = Some(end_version);
        Ok(())
    }

    fn apply_changed(
        &mut self,
        bids: &[WsBookLevel],
        asks: &[WsBookLevel],
        start_version: i64,
        end_version: i64,
    ) -> Result<()> {
        if let Some(last) = self.last_end_version {
            if last != start_version {
                tracing::warn!(
                    last_end_version = last,
                    start_version,
                    "EdgeX depth: possible missed frame (version gap)"
                );
            }
        }
        for level in bids {
            apply_level(&mut self.bids, level)?;
        }
        for level in asks {
            apply_level(&mut self.asks, level)?;
        }
        self.last_end_version = Some(end_version);
        Ok(())
    }

    /// Quantity at an exact price on the requested side; `Decimal::ZERO` if the book has no
    /// snapshot yet or no level at that exact price (used only for ticker qty enrichment -
    /// ticker price fields come from the ticker frame itself, not from this book).
    fn qty_at(&self, is_bid: bool, price: Decimal) -> Decimal {
        let map = if is_bid { &self.bids } else { &self.asks };
        map.get(&price).copied().unwrap_or(Decimal::ZERO)
    }

    fn to_orderbook(&self, symbol: &str, timestamp: DateTime<Utc>) -> Orderbook {
        Orderbook {
            symbol: symbol.to_string(),
            bids: self
                .bids
                .iter()
                .rev()
                .map(|(price, quantity)| OrderbookLevel {
                    price: *price,
                    quantity: *quantity,
                })
                .collect(),
            asks: self
                .asks
                .iter()
                .map(|(price, quantity)| OrderbookLevel {
                    price: *price,
                    quantity: *quantity,
                })
                .collect(),
            timestamp,
        }
    }
}

/// Merges a `WsTickerRecord` with an optional maintained `DepthBook` (for best-level
/// quantity only - price comes from the record itself, confirmed live to carry
/// `bestBidPrice`/`bestAskPrice` directly). `symbol` is filled in by the caller.
fn ws_ticker_to_ticker(record: &WsTickerRecord, book: Option<&DepthBook>) -> Result<Ticker> {
    let mark_price = parse_decimal(&record.mark_price, "markPrice")?;
    let open_interest = parse_decimal(&record.open_interest, "openInterest")?;
    // Absent on illiquid/no-book contracts (confirmed live) - zero price/qty, not an error.
    let best_bid_price = record
        .best_bid_price
        .as_deref()
        .map(|p| parse_decimal(p, "bestBidPrice"))
        .transpose()?
        .unwrap_or(Decimal::ZERO);
    let best_ask_price = record
        .best_ask_price
        .as_deref()
        .map(|p| parse_decimal(p, "bestAskPrice"))
        .transpose()?
        .unwrap_or(Decimal::ZERO);

    Ok(Ticker {
        symbol: String::new(),
        last_price: parse_decimal(&record.last_price, "lastPrice")?,
        mark_price,
        index_price: parse_decimal(&record.index_price, "indexPrice")?,
        best_bid_price,
        best_bid_qty: book.map(|b| b.qty_at(true, best_bid_price)).unwrap_or(Decimal::ZERO),
        best_ask_price,
        best_ask_qty: book.map(|b| b.qty_at(false, best_ask_price)).unwrap_or(Decimal::ZERO),
        volume_24h: parse_decimal(&record.size, "size")?,
        turnover_24h: parse_decimal(&record.value, "value")?,
        open_interest,
        open_interest_notional: (open_interest * mark_price).round_dp(2),
        price_change_24h: parse_decimal(&record.price_change, "priceChange")?,
        price_change_pct: parse_decimal(&record.price_change_percent, "priceChangePercent")?,
        high_price_24h: parse_decimal(&record.high, "high")?,
        low_price_24h: parse_decimal(&record.low, "low")?,
        timestamp: datetime_from_ms(parse_ms(&record.end_time, "endTime")?)?,
    })
}

/// Builds `FundingRate` from a `WsTickerRecord` plus the contract's cached metadata.
/// `predicted_rate` falls back to `funding_rate` - the WS payload carries no
/// `predictedFundingRate`/`forecastFundingRate` field at all (confirmed exhaustively via live
/// capture), the same fallback REST performs when its own forecast field is empty
/// (`conversions.rs::funding_record_to_funding_rate`).
fn ws_ticker_to_funding_rate(record: &WsTickerRecord, meta: &ContractMeta) -> Result<FundingRate> {
    let funding_rate = parse_decimal(&record.funding_rate, "fundingRate")?;
    let funding_time = datetime_from_ms(parse_ms(&record.funding_time, "fundingTime")?)?;
    let next_funding_time =
        datetime_from_ms(parse_ms(&record.next_funding_time, "nextFundingTime")?)?;

    let interval_min: i64 = meta.funding_rate_interval_min.parse().with_context(|| {
        format!(
            "failed to parse fundingRateIntervalMin: {:?}",
            meta.funding_rate_interval_min
        )
    })?;
    if interval_min <= 0 || interval_min % 60 != 0 {
        bail!("EdgeX: fundingRateIntervalMin {interval_min} is not a positive multiple of 60");
    }
    let funding_interval = (interval_min / 60) as i32;

    let min_rate = parse_decimal(&meta.funding_min_rate, "fundingMinRate")?.abs();
    let max_rate = parse_decimal(&meta.funding_max_rate, "fundingMaxRate")?.abs();
    let funding_rate_cap_floor = min_rate.max(max_rate);

    Ok(FundingRate {
        symbol: String::new(),
        funding_rate,
        predicted_rate: funding_rate,
        funding_time,
        next_funding_time,
        funding_interval,
        funding_rate_cap_floor,
    })
}

/// Confirmed live (2026-09-11): `trades.{contractId}` carries `isBuyerMaker: bool`, not a
/// `"BUY"`/`"SELL"` string. `is_buyer_maker == true` means the resting order was a buy, so
/// the trade's taker (the side `Trade.side` records) was the seller.
fn ws_trade_to_trade(record: &WsTradeRecord, symbol: &str) -> Result<Trade> {
    Ok(Trade {
        id: record.ticket_id.clone(),
        symbol: symbol.to_string(),
        price: parse_decimal(&record.price, "price")?,
        quantity: parse_decimal(&record.size, "size")?,
        side: if record.is_buyer_maker {
            OrderSide::Sell
        } else {
            OrderSide::Buy
        },
        timestamp: datetime_from_ms(parse_ms(&record.time, "time")?)?,
    })
}

fn ws_kline_to_kline(record: &WsKlineRecord, symbol: &str, interval: &str) -> Result<Kline> {
    let duration_ms = interval_duration_ms(interval)?;
    let open_ms = parse_ms(&record.kline_time, "klineTime")?;
    let open_time = datetime_from_ms(open_ms)?;
    let close_time = datetime_from_ms(open_ms + duration_ms - 1)?;

    Ok(Kline {
        symbol: symbol.to_string(),
        interval: interval.to_string(),
        open_time,
        close_time,
        open: parse_decimal(&record.open, "open")?,
        high: parse_decimal(&record.high, "high")?,
        low: parse_decimal(&record.low, "low")?,
        close: parse_decimal(&record.close, "close")?,
        volume: parse_decimal(&record.size, "size")?,
        turnover: parse_decimal(&record.value, "value")?,
    })
}

#[async_trait]
impl IPerpsStream for EdgexWsClient {
    fn get_name(&self) -> &str {
        "edgex"
    }

    async fn stream_tickers(&self, symbols: Vec<String>) -> Result<DataStream<Ticker>> {
        let stream = self
            .stream_multi(StreamConfig {
                symbols,
                data_types: vec![StreamDataType::Ticker],
                auto_reconnect: false,
                kline_interval: None,
            })
            .await?;

        Ok(Box::pin(stream.filter_map(|item| async move {
            match item {
                Ok(StreamEvent::Ticker(ticker)) => Some(Ok(ticker)),
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            }
        })))
    }

    async fn stream_trades(&self, symbols: Vec<String>) -> Result<DataStream<Trade>> {
        let stream = self
            .stream_multi(StreamConfig {
                symbols,
                data_types: vec![StreamDataType::Trade],
                auto_reconnect: false,
                kline_interval: None,
            })
            .await?;

        Ok(Box::pin(stream.filter_map(|item| async move {
            match item {
                Ok(StreamEvent::Trade(trade)) => Some(Ok(trade)),
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            }
        })))
    }

    async fn stream_orderbooks(&self, symbols: Vec<String>) -> Result<DataStream<Orderbook>> {
        let stream = self
            .stream_multi(StreamConfig {
                symbols,
                data_types: vec![StreamDataType::Orderbook],
                auto_reconnect: false,
                kline_interval: None,
            })
            .await?;

        Ok(Box::pin(stream.filter_map(|item| async move {
            match item {
                Ok(StreamEvent::Orderbook(orderbook)) => Some(Ok(orderbook)),
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            }
        })))
    }

    async fn stream_multi(&self, config: StreamConfig) -> Result<DataStream<StreamEvent>> {
        let wants_ticker = config.data_types.contains(&StreamDataType::Ticker);
        let wants_trade = config.data_types.contains(&StreamDataType::Trade);
        let wants_orderbook = config.data_types.contains(&StreamDataType::Orderbook);
        let wants_funding = config.data_types.contains(&StreamDataType::FundingRate);
        let wants_kline = config.data_types.contains(&StreamDataType::Kline);
        if !wants_ticker && !wants_trade && !wants_orderbook && !wants_funding && !wants_kline {
            return Err(anyhow!("EdgeX stream config contains no supported data types"));
        }

        let kline_type = if wants_kline {
            let interval = config
                .kline_interval
                .as_deref()
                .ok_or_else(|| anyhow!("kline_interval is required when streaming klines"))?;
            let token = KLINE_INTERVALS
                .iter()
                .find(|(k, _, _)| *k == interval)
                .map(|(_, v, _)| *v)
                .ok_or_else(|| anyhow!("EdgeX: unsupported kline interval: {interval}"))?;
            Some((interval.to_string(), token))
        } else {
            None
        };

        let symbols = dedup_symbols(config.symbols)?;

        // Resolve contractId for every requested symbol (reuses EdgexClient's cached
        // metadata - no second HTTP client or cache, per `01_overview.md` Key decision 1).
        let mut symbol_to_contract: HashMap<String, String> = HashMap::new();
        let mut contract_to_symbol: HashMap<String, String> = HashMap::new();
        let mut contract_meta: HashMap<String, ContractMeta> = HashMap::new();
        for symbol in &symbols {
            let contract_id = self
                .rest
                .resolve_contract_id(symbol)
                .await
                .with_context(|| format!("failed to resolve EdgeX contract id for {symbol}"))?;
            let global_symbol = to_global_symbol(symbol);
            contract_to_symbol.insert(contract_id.clone(), global_symbol.clone());
            symbol_to_contract.insert(global_symbol, contract_id);
        }
        if wants_funding {
            for symbol in &symbols {
                let contract_name = to_edgex_symbol(symbol);
                let meta = self
                    .rest
                    .find_contract_by_name(&contract_name)
                    .await
                    .with_context(|| format!("failed to fetch EdgeX contract metadata for {symbol}"))?;
                contract_meta.insert(meta.contract_id.clone(), meta);
            }
        }
        let requested_contract_ids: HashSet<String> = contract_to_symbol.keys().cloned().collect();

        let mut channels = Vec::new();
        if wants_ticker || wants_funding {
            channels.push(ticker_all_channel());
        }
        for contract_id in symbol_to_contract.values() {
            if wants_ticker || wants_orderbook {
                channels.push(depth_channel(contract_id));
            }
            if wants_trade {
                channels.push(trades_channel(contract_id));
            }
            if let Some((_, token)) = &kline_type {
                channels.push(kline_channel(contract_id, token));
            }
        }

        let mut ws_stream = self.connect_and_subscribe(&channels).await?;
        let kline_interval = kline_type.map(|(interval, _)| interval);

        let stream = async_stream::stream! {
            let mut books: HashMap<String, DepthBook> = HashMap::new();

            while let Some(message) = ws_stream.next().await {
                match message {
                    Ok(Message::Text(text)) => {
                        let envelope = match serde_json::from_str::<EdgexWsEnvelope>(&text) {
                            Ok(envelope) => envelope,
                            Err(error) => {
                                tracing::debug!("Skipping malformed EdgeX websocket message: {}", error);
                                continue;
                            }
                        };

                        match envelope.msg_type.as_str() {
                            "ping" => {
                                let time = envelope.time.clone().unwrap_or_default();
                                let pong = serde_json::json!({"type": "pong", "time": time}).to_string();
                                if let Err(error) = ws_stream.send(Message::Text(pong)).await {
                                    yield Err(anyhow!("failed to send EdgeX websocket pong: {}", error));
                                    break;
                                }
                                continue;
                            }
                            "connected" | "subscribed" | "unsubscribed" => continue,
                            "error" => {
                                yield Err(anyhow!("EdgeX websocket error on channel {:?}", envelope.channel));
                                break;
                            }
                            "quote-event" => {}
                            other => {
                                tracing::trace!("Ignoring EdgeX websocket message type: {}", other);
                                continue;
                            }
                        }

                        let content: EdgexWsContent = match envelope.content {
                            Some(content) => content,
                            None => {
                                tracing::debug!("EdgeX quote-event missing content, skipping");
                                continue;
                            }
                        };

                        if envelope.channel == "ticker.all.1s" {
                            let records: Vec<WsTickerRecord> = match serde_json::from_value(content.data) {
                                Ok(records) => records,
                                Err(error) => {
                                    yield Err(anyhow!("failed to parse EdgeX ticker payload: {}", error));
                                    continue;
                                }
                            };
                            for record in &records {
                                if !requested_contract_ids.contains(&record.contract_id) {
                                    continue;
                                }
                                let symbol = &contract_to_symbol[&record.contract_id];

                                if wants_funding {
                                    if let Some(meta) = contract_meta.get(&record.contract_id) {
                                        match ws_ticker_to_funding_rate(record, meta) {
                                            Ok(mut fr) => {
                                                fr.symbol = symbol.clone();
                                                yield Ok(StreamEvent::FundingRate(fr));
                                            }
                                            Err(error) => yield Err(error.context("failed to convert EdgeX funding rate")),
                                        }
                                    }
                                }

                                if wants_ticker {
                                    if !record.market_open {
                                        continue;
                                    }
                                    let book = books.get(symbol);
                                    match ws_ticker_to_ticker(record, book) {
                                        Ok(mut ticker) => {
                                            ticker.symbol = symbol.clone();
                                            yield Ok(StreamEvent::Ticker(ticker));
                                        }
                                        Err(error) => yield Err(error.context("failed to convert EdgeX ticker")),
                                    }
                                }
                            }
                        } else if envelope.channel.starts_with("depth.") {
                            let records: Vec<WsDepthRecord> = match serde_json::from_value(content.data) {
                                Ok(records) => records,
                                Err(error) => {
                                    yield Err(anyhow!("failed to parse EdgeX depth payload: {}", error));
                                    continue;
                                }
                            };
                            for record in &records {
                                let symbol = match contract_to_symbol.get(&record.contract_id) {
                                    Some(symbol) => symbol.clone(),
                                    None => continue,
                                };
                                let (start_version, end_version) = match (
                                    record.start_version.parse::<i64>(),
                                    record.end_version.parse::<i64>(),
                                ) {
                                    (Ok(s), Ok(e)) => (s, e),
                                    _ => {
                                        yield Err(anyhow!(
                                            "EdgeX depth frame has non-numeric version: {:?}/{:?}",
                                            record.start_version, record.end_version
                                        ));
                                        continue;
                                    }
                                };
                                let book = books.entry(symbol.clone()).or_default();
                                let apply_result = match content.data_type.as_str() {
                                    "Snapshot" => book.apply_snapshot(&record.bids, &record.asks, end_version),
                                    "changed" => book.apply_changed(&record.bids, &record.asks, start_version, end_version),
                                    other => {
                                        tracing::trace!("Ignoring EdgeX depth dataType: {}", other);
                                        continue;
                                    }
                                };
                                if let Err(error) = apply_result {
                                    yield Err(error.context("failed to apply EdgeX depth update"));
                                    continue;
                                }
                                if wants_orderbook {
                                    yield Ok(StreamEvent::Orderbook(book.to_orderbook(&symbol, Utc::now())));
                                }
                            }
                        } else if wants_trade && envelope.channel.starts_with("trades.") {
                            let records: Vec<WsTradeRecord> = match serde_json::from_value(content.data) {
                                Ok(records) => records,
                                Err(error) => {
                                    yield Err(anyhow!("failed to parse EdgeX trades payload: {}", error));
                                    continue;
                                }
                            };
                            for record in &records {
                                let symbol = match contract_to_symbol.get(&record.contract_id) {
                                    Some(symbol) => symbol,
                                    None => continue,
                                };
                                match ws_trade_to_trade(record, symbol) {
                                    Ok(trade) => yield Ok(StreamEvent::Trade(trade)),
                                    Err(error) => yield Err(error.context("failed to convert EdgeX trade")),
                                }
                            }
                        } else if let Some(interval) = kline_interval.as_deref().filter(|_| envelope.channel.starts_with("kline.")) {
                            let records: Vec<WsKlineRecord> = match serde_json::from_value(content.data) {
                                Ok(records) => records,
                                Err(error) => {
                                    yield Err(anyhow!("failed to parse EdgeX kline payload: {}", error));
                                    continue;
                                }
                            };
                            for record in &records {
                                let symbol = match contract_to_symbol.get(&record.contract_id) {
                                    Some(symbol) => symbol,
                                    None => continue,
                                };
                                match ws_kline_to_kline(record, symbol, interval) {
                                    Ok(kline) => yield Ok(StreamEvent::Kline(kline)),
                                    Err(error) => yield Err(error.context("failed to convert EdgeX kline")),
                                }
                            }
                        } else {
                            tracing::trace!("Ignoring EdgeX websocket channel: {}", envelope.channel);
                        }
                    }
                    Ok(Message::Ping(payload)) => {
                        if let Err(error) = ws_stream.send(Message::Pong(payload)).await {
                            yield Err(anyhow!("failed to send EdgeX websocket transport pong: {}", error));
                            break;
                        }
                    }
                    Ok(Message::Pong(_)) => tracing::trace!("Received EdgeX websocket transport pong"),
                    Ok(Message::Close(frame)) => {
                        tracing::info!("EdgeX websocket connection closed: {:?}", frame);
                        break;
                    }
                    Ok(_) => tracing::trace!("Ignoring non-text EdgeX websocket message"),
                    Err(error) => {
                        yield Err(anyhow!("EdgeX websocket error: {}", error));
                        break;
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn btc_contract_meta() -> ContractMeta {
        ContractMeta {
            contract_id: "30000001".to_string(),
            contract_name: "BTCUSDC".to_string(),
            base_coin_id: "1001".to_string(),
            quote_coin_id: "1000".to_string(),
            tick_size: "0.1".to_string(),
            step_size: "0.001".to_string(),
            min_order_size: "0.001".to_string(),
            max_order_size: "100".to_string(),
            default_taker_fee_rate: "0.00045".to_string(),
            default_maker_fee_rate: "0.0004".to_string(),
            default_leverage: "10".to_string(),
            funding_rate_interval_min: "240".to_string(),
            funding_min_rate: "-0.002".to_string(),
            funding_max_rate: "0.002".to_string(),
            display_max_leverage: "100".to_string(),
            enable_trade: true,
            enable_display: true,
            is_stock: false,
            is_fx: false,
        }
    }

    fn btc_ticker_record() -> WsTickerRecord {
        WsTickerRecord {
            contract_id: "30000001".to_string(),
            price_change: "2148.2".to_string(),
            price_change_percent: "0.027941".to_string(),
            size: "8138.396".to_string(),
            value: "628869807.4804".to_string(),
            high: "79824.9".to_string(),
            low: "76031.9".to_string(),
            end_time: "1789136100000".to_string(),
            last_price: "79031.2".to_string(),
            index_price: "79076.6724223198".to_string(),
            mark_price: "79025.030809853284955708".to_string(),
            open_interest: "3478.949".to_string(),
            funding_rate: "-0.00005754".to_string(),
            funding_time: "1789128000000".to_string(),
            next_funding_time: "1789142400000".to_string(),
            best_bid_price: Some("79031.1".to_string()),
            best_ask_price: Some("79031.4".to_string()),
            market_open: true,
        }
    }

    #[test]
    fn ws_ticker_to_ticker_uses_own_bid_ask_price_and_book_for_qty() {
        let record = btc_ticker_record();
        let mut book = DepthBook::default();
        book.apply_snapshot(
            &[WsBookLevel { price: "79031.1".to_string(), size: "0.5".to_string() }],
            &[WsBookLevel { price: "79031.4".to_string(), size: "0.3".to_string() }],
            1,
        )
        .unwrap();

        let ticker = ws_ticker_to_ticker(&record, Some(&book)).unwrap();
        assert_eq!(ticker.best_bid_price, Decimal::from_str_exact("79031.1").unwrap());
        assert_eq!(ticker.best_bid_qty, Decimal::from_str_exact("0.5").unwrap());
        assert_eq!(ticker.best_ask_qty, Decimal::from_str_exact("0.3").unwrap());
        assert_eq!(
            ticker.price_change_pct,
            Decimal::from_str_exact("0.027941").unwrap()
        );
        assert!(!ticker.is_empty());
    }

    #[test]
    fn ws_ticker_to_ticker_without_book_zero_fills_qty_not_skipped() {
        let record = btc_ticker_record();
        let ticker = ws_ticker_to_ticker(&record, None).unwrap();
        assert_eq!(ticker.best_bid_qty, Decimal::ZERO);
        assert_eq!(ticker.best_ask_qty, Decimal::ZERO);
        assert!(ticker.best_bid_price > Decimal::ZERO);
    }

    #[test]
    fn ws_ticker_to_ticker_missing_best_bid_ask_zero_fills_not_a_parse_error() {
        // Confirmed live via manual smoke test: an illiquid contract (e.g. EURUSDC) can omit
        // bestBidPrice/bestAskPrice entirely - this must degrade to a zero-price ticker, not
        // fail the whole ticker.all.1s batch for every other symbol.
        let mut record = btc_ticker_record();
        record.best_bid_price = None;
        record.best_ask_price = None;
        let ticker = ws_ticker_to_ticker(&record, None).unwrap();
        assert_eq!(ticker.best_bid_price, Decimal::ZERO);
        assert_eq!(ticker.best_ask_price, Decimal::ZERO);
    }

    #[test]
    fn ws_ticker_to_funding_rate_derives_interval_and_cap_floor_from_meta() {
        let record = btc_ticker_record();
        let meta = btc_contract_meta();
        let fr = ws_ticker_to_funding_rate(&record, &meta).unwrap();
        assert_eq!(fr.funding_rate, Decimal::from_str_exact("-0.00005754").unwrap());
        // No forecast field on the wire - predicted_rate mirrors funding_rate.
        assert_eq!(fr.predicted_rate, fr.funding_rate);
        assert_eq!(fr.funding_interval, 4);
        assert_eq!(fr.funding_rate_cap_floor, Decimal::from_str_exact("0.002").unwrap());
        assert_eq!((fr.next_funding_time - fr.funding_time).num_minutes(), 240);
    }

    #[test]
    fn ws_trade_to_trade_derives_side_from_is_buyer_maker() {
        let buyer_maker = WsTradeRecord {
            ticket_id: "t1".to_string(),
            contract_id: "30000001".to_string(),
            price: "79031.2".to_string(),
            size: "0.007".to_string(),
            time: "1789136310088".to_string(),
            is_buyer_maker: true,
        };
        let trade = ws_trade_to_trade(&buyer_maker, "BTC").unwrap();
        assert_eq!(trade.side, OrderSide::Sell);
        assert_eq!(trade.id, "t1");

        let mut taker_is_buyer = buyer_maker;
        taker_is_buyer.is_buyer_maker = false;
        let trade = ws_trade_to_trade(&taker_is_buyer, "BTC").unwrap();
        assert_eq!(trade.side, OrderSide::Buy);
    }

    #[test]
    fn ws_kline_to_kline_derives_close_time() {
        let record = WsKlineRecord {
            contract_id: "30000001".to_string(),
            kline_time: "1789136280000".to_string(),
            open: "79050.5".to_string(),
            high: "79054.5".to_string(),
            low: "78984.8".to_string(),
            close: "79031.3".to_string(),
            size: "4.077".to_string(),
            value: "322191.4503".to_string(),
        };
        let kline = ws_kline_to_kline(&record, "BTC", "1m").unwrap();
        assert_eq!(
            (kline.close_time - kline.open_time).num_milliseconds(),
            60_000 - 1
        );
    }

    #[test]
    fn depth_book_apply_snapshot_then_changed_matches_live_full_replacement_semantics() {
        let mut book = DepthBook::default();
        book.apply_snapshot(
            &[
                WsBookLevel { price: "79031.2".to_string(), size: "1.0".to_string() },
                WsBookLevel { price: "79030.0".to_string(), size: "2.0".to_string() },
            ],
            &[WsBookLevel { price: "79031.4".to_string(), size: "1.175".to_string() }],
            100,
        )
        .unwrap();
        assert_eq!(
            book.qty_at(true, Decimal::from_str_exact("79031.2").unwrap()),
            Decimal::ONE
        );

        // Live-observed shape: a "changed" frame replaces one level's absolute size and
        // removes another via size == "0", not an additive delta.
        book.apply_changed(
            &[
                WsBookLevel { price: "79031.2".to_string(), size: "0.720".to_string() },
                WsBookLevel { price: "79030.0".to_string(), size: "0".to_string() },
            ],
            &[],
            100,
            120,
        )
        .unwrap();
        assert_eq!(
            book.qty_at(true, Decimal::from_str_exact("79031.2").unwrap()),
            Decimal::from_str_exact("0.720").unwrap()
        );
        assert_eq!(book.qty_at(true, Decimal::from_str_exact("79030.0").unwrap()), Decimal::ZERO);
    }

    #[test]
    fn depth_book_apply_changed_on_unseeded_book_is_a_noop_not_a_panic() {
        let mut book = DepthBook::default();
        book.apply_changed(
            &[WsBookLevel { price: "1".to_string(), size: "1".to_string() }],
            &[],
            0,
            1,
        )
        .unwrap();
        assert_eq!(book.qty_at(true, Decimal::ONE), Decimal::ONE);
    }

    #[test]
    fn to_orderbook_sorts_bids_descending_and_asks_ascending() {
        let mut book = DepthBook::default();
        book.apply_snapshot(
            &[
                WsBookLevel { price: "100".to_string(), size: "1".to_string() },
                WsBookLevel { price: "101".to_string(), size: "1".to_string() },
            ],
            &[
                WsBookLevel { price: "103".to_string(), size: "1".to_string() },
                WsBookLevel { price: "102".to_string(), size: "1".to_string() },
            ],
            1,
        )
        .unwrap();

        let ob = book.to_orderbook("BTC", Utc::now());
        assert_eq!(ob.bids[0].price, Decimal::from(101), "best bid is the highest price, first");
        assert_eq!(ob.asks[0].price, Decimal::from(102), "best ask is the lowest price, first");
        assert!(ob.bids[0].price > ob.bids[1].price, "bids must be descending");
        assert!(ob.asks[0].price < ob.asks[1].price, "asks must be ascending");
    }

    #[test]
    fn envelope_channel_and_dispatch_helpers() {
        assert_eq!(ticker_all_channel(), "ticker.all.1s");
        assert_eq!(depth_channel("30000001"), "depth.30000001.200");
        assert_eq!(trades_channel("30000001"), "trades.30000001");
        assert_eq!(kline_channel("30000001", "MINUTE_1"), "kline.LAST_PRICE.30000001.MINUTE_1");
    }

    #[test]
    fn get_name_is_edgex() {
        assert_eq!(EdgexWsClient::new().get_name(), "edgex");
    }
}
