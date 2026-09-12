use std::collections::{HashMap, HashSet};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use futures::{SinkExt, StreamExt};
use perps_core::streaming::{DataStream, IPerpsStream, StreamConfig, StreamDataType, StreamEvent};
use perps_core::{FundingRate, Orderbook, Ticker, Trade};
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

use super::client::{to_arcus_symbol, to_global_symbol};
use super::conversions;
use super::types::OrderbookSnapshot;
use super::ws_types::*;

const WS_BASE_URL: &str = "wss://api.arcus.xyz/v1/ws";
// Matches Arcus REST's own max (`ArcusClient::fetch_orderbook_raw` clamps `nLevels` to 100) -
// keeps the WS-fed `WsOrderbookManager` at
// full parity with what REST would return for the same symbol, since callers requesting
// deep books (`depth: 1000` from `start`/`liquidity`, clamped by Arcus to 100 either way)
// must not silently get fewer levels from the cache than a REST call would give them.
const ORDERBOOK_LEVELS: u32 = 100;

/// Public market-data WebSocket client for Arcus.
#[derive(Clone)]
pub struct ArcusWsClient {
    base_url: String,
}

impl ArcusWsClient {
    pub fn new() -> Self {
        Self {
            base_url: WS_BASE_URL.to_string(),
        }
    }

    async fn connect_and_subscribe(
        &self,
        subscriptions: &[ArcusWsSubscribe],
    ) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>> {
        tracing::info!("Connecting to Arcus WebSocket: {}", self.base_url);
        let (mut stream, response) = connect_async(self.base_url.as_str())
            .await
            .context("failed to connect to Arcus websocket")?;
        tracing::info!(
            "Connected to Arcus WebSocket (status: {:?})",
            response.status()
        );

        for subscription in subscriptions {
            let text = serde_json::to_string(subscription)
                .context("failed to serialize Arcus subscribe message")?;
            stream.send(Message::Text(text)).await.with_context(|| {
                format!(
                    "failed to subscribe to Arcus channel {} for {:?}",
                    subscription.channel, subscription.id
                )
            })?;
        }

        Ok(stream)
    }
}

impl Default for ArcusWsClient {
    fn default() -> Self {
        Self::new()
    }
}

fn normalized_symbols(symbols: Vec<String>) -> Result<(Vec<String>, HashSet<String>)> {
    if symbols.is_empty() {
        return Err(anyhow!(
            "at least one symbol is required for Arcus streaming"
        ));
    }

    let mut seen = HashSet::new();
    let mut arcus_symbols = Vec::new();
    for symbol in symbols {
        let arcus_symbol = to_arcus_symbol(&symbol);
        if seen.insert(arcus_symbol.clone()) {
            arcus_symbols.push(arcus_symbol);
        }
    }
    let requested = arcus_symbols
        .iter()
        .map(|symbol| to_global_symbol(symbol))
        .collect();
    Ok((arcus_symbols, requested))
}

fn orderbook_snapshot(contents: &ArcusWsOrderbookContents) -> OrderbookSnapshot {
    OrderbookSnapshot {
        bids: contents.bids.clone(),
        asks: contents.asks.clone(),
        last_sequence_id: contents.last_sequence_id,
        global_sequence_id: contents.global_sequence_id.unwrap_or_default(),
        timestamp: contents
            .timestamp
            .unwrap_or_else(|| Utc::now().timestamp_micros()),
    }
}

fn bbo_snapshot(contents: &ArcusWsBboContents) -> OrderbookSnapshot {
    let bids = contents
        .best_bid
        .as_ref()
        .map(|level| [[level.price.clone(), level.size.clone()]])
        .into_iter()
        .flatten()
        .collect();
    let asks = contents
        .best_ask
        .as_ref()
        .map(|level| [[level.price.clone(), level.size.clone()]])
        .into_iter()
        .flatten()
        .collect();

    OrderbookSnapshot {
        bids,
        asks,
        last_sequence_id: contents.last_sequence_id,
        global_sequence_id: contents.global_sequence_id.unwrap_or_default(),
        timestamp: contents.timestamp,
    }
}

fn build_orderbook(id: &str, contents: &ArcusWsOrderbookContents) -> Result<Orderbook> {
    let orderbook = conversions::orderbook_snapshot_to_orderbook(
        &orderbook_snapshot(contents),
        to_global_symbol(id),
    )?;

    if let (Some(best_bid), Some(best_ask)) = (orderbook.bids.first(), orderbook.asks.first()) {
        if best_bid.price >= best_ask.price {
            tracing::warn!(
                symbol = %orderbook.symbol,
                bid = %best_bid.price,
                ask = %best_ask.price,
                "Arcus WebSocket returned a crossed orderbook"
            );
        }
    }

    Ok(orderbook)
}

fn build_ticker(
    symbol: &str,
    bbo: &ArcusWsBboContents,
    market: &ArcusWsMarketEntry,
) -> Result<Ticker> {
    if market.status != "ONLINE" {
        return Err(anyhow!(
            "cannot build Arcus ticker for {} market {}",
            market.status,
            market.market_display_name
        ));
    }

    let snapshot = bbo_snapshot(bbo);
    let captured_at = conversions::orderbook_snapshot_to_orderbook(
        &snapshot,
        to_global_symbol(&market.market_display_name),
    )?
    .timestamp;
    let mut ticker = conversions::to_ticker(market, &snapshot, captured_at)?;
    ticker.symbol = symbol.to_string();
    Ok(ticker)
}

fn build_funding_rate(symbol: &str, market: &ArcusWsMarketEntry) -> Result<FundingRate> {
    let mut funding_rate = conversions::to_funding_rate(market)?;
    funding_rate.symbol = symbol.to_string();
    Ok(funding_rate)
}

fn arcus_error(envelope: &ArcusWsEnvelope) -> anyhow::Error {
    let detail = envelope
        .message
        .as_deref()
        .or(envelope.reason.as_deref())
        .unwrap_or("unknown websocket error");
    anyhow!(
        "Arcus websocket error on channel {:?} for {:?}: {}",
        envelope.channel,
        envelope.id,
        detail
    )
}

#[async_trait]
impl IPerpsStream for ArcusWsClient {
    fn get_name(&self) -> &str {
        "arcus"
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

    async fn stream_trades(&self, _symbols: Vec<String>) -> Result<DataStream<Trade>> {
        Err(anyhow!(
            "arcus does not support trade streaming: the `trades` websocket channel has no side field, so a correct Trade cannot be constructed without guessing"
        ))
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
        if config.data_types.contains(&StreamDataType::Trade) {
            return Err(anyhow!(
                "arcus does not support trade streaming: the `trades` websocket channel has no side field, so a correct Trade cannot be constructed without guessing"
            ));
        }

        let wants_ticker = config.data_types.contains(&StreamDataType::Ticker);
        let wants_orderbook = config.data_types.contains(&StreamDataType::Orderbook);
        let wants_funding = config.data_types.contains(&StreamDataType::FundingRate);
        if config.data_types.contains(&StreamDataType::Kline) {
            tracing::warn!("Arcus WebSocket kline streaming is not implemented; skipping klines");
        }
        if !wants_ticker && !wants_orderbook && !wants_funding {
            return Err(anyhow!(
                "Arcus stream config contains no supported data types"
            ));
        }

        let (arcus_symbols, requested_symbols) = normalized_symbols(config.symbols)?;
        let mut subscriptions = Vec::new();
        for symbol in &arcus_symbols {
            if wants_ticker {
                subscriptions.push(ArcusWsSubscribe::market("bbo", symbol.clone()));
            }
            if wants_orderbook {
                subscriptions.push(ArcusWsSubscribe::orderbook(
                    symbol.clone(),
                    ORDERBOOK_LEVELS,
                ));
            }
        }
        if wants_ticker || wants_funding {
            subscriptions.push(ArcusWsSubscribe::markets());
        }

        let mut ws_stream = self.connect_and_subscribe(&subscriptions).await?;
        let stream = async_stream::stream! {
            let mut bbo_state: HashMap<String, ArcusWsBboContents> = HashMap::new();
            let mut market_state: HashMap<String, ArcusWsMarketEntry> = HashMap::new();

            while let Some(message) = ws_stream.next().await {
                match message {
                    Ok(Message::Text(text)) => {
                        let envelope = match serde_json::from_str::<ArcusWsEnvelope>(&text) {
                            Ok(envelope) => envelope,
                            Err(error) => {
                                tracing::debug!("Skipping malformed Arcus websocket message: {}", error);
                                continue;
                            }
                        };

                        match envelope.msg_type.as_str() {
                            "connected" | "unsubscribed" => continue,
                            "degraded" => {
                                tracing::warn!(
                                    channel = %envelope.channel,
                                    id = ?envelope.id,
                                    reason = ?envelope.reason,
                                    retry_after_ms = ?envelope.retry_after_ms,
                                    "Arcus websocket subscription is degraded"
                                );
                                continue;
                            }
                            "error" => {
                                yield Err(arcus_error(&envelope));
                                break;
                            }
                            "subscribed" | "channel_data" => {}
                            other => {
                                tracing::trace!("Ignoring Arcus websocket message type: {}", other);
                                continue;
                            }
                        }

                        match envelope.channel.as_str() {
                            "l2Orderbook" if wants_orderbook => {
                                let id = match envelope.id.as_deref() {
                                    Some(id) => id,
                                    None => {
                                        yield Err(anyhow!("Arcus l2Orderbook message is missing id"));
                                        continue;
                                    }
                                };
                                match serde_json::from_value::<ArcusWsOrderbookContents>(envelope.contents) {
                                    Ok(contents) => match build_orderbook(id, &contents) {
                                        Ok(orderbook) => yield Ok(StreamEvent::Orderbook(orderbook)),
                                        Err(error) => yield Err(error.context("failed to convert Arcus orderbook")),
                                    },
                                    Err(error) => yield Err(anyhow!("failed to parse Arcus orderbook payload: {}", error)),
                                }
                            }
                            "bbo" if wants_ticker => {
                                let id = match envelope.id.as_deref() {
                                    Some(id) => id,
                                    None => {
                                        yield Err(anyhow!("Arcus bbo message is missing id"));
                                        continue;
                                    }
                                };
                                let symbol = to_global_symbol(id);
                                if !requested_symbols.contains(&symbol) {
                                    continue;
                                }
                                match serde_json::from_value::<ArcusWsBboContents>(envelope.contents) {
                                    Ok(contents) => {
                                        bbo_state.insert(symbol.clone(), contents);
                                        if let (Some(bbo), Some(market)) =
                                            (bbo_state.get(&symbol), market_state.get(&symbol))
                                        {
                                            match build_ticker(&symbol, bbo, market) {
                                                Ok(ticker) => yield Ok(StreamEvent::Ticker(ticker)),
                                                Err(error) => yield Err(error.context("failed to merge Arcus ticker")),
                                            }
                                        }
                                    }
                                    Err(error) => yield Err(anyhow!("failed to parse Arcus bbo payload: {}", error)),
                                }
                            }
                            "markets" if wants_ticker || wants_funding => {
                                match serde_json::from_value::<ArcusWsMarketsContents>(envelope.contents) {
                                    Ok(contents) => {
                                        if !contents.is_snapshot {
                                            tracing::warn!("Arcus markets channel sent a non-snapshot payload");
                                        }
                                        for market in contents.markets.into_values() {
                                            let symbol = to_global_symbol(&market.market_display_name);
                                            if !requested_symbols.contains(&symbol) {
                                                continue;
                                            }

                                            if wants_funding && market.status == "ONLINE" {
                                                match build_funding_rate(&symbol, &market) {
                                                    Ok(rate) => yield Ok(StreamEvent::FundingRate(rate)),
                                                    Err(error) => yield Err(error.context("failed to convert Arcus funding rate")),
                                                }
                                            }

                                            market_state.insert(symbol.clone(), market);
                                            if wants_ticker {
                                                if let (Some(bbo), Some(market)) =
                                                    (bbo_state.get(&symbol), market_state.get(&symbol))
                                                {
                                                    match build_ticker(&symbol, bbo, market) {
                                                        Ok(ticker) => yield Ok(StreamEvent::Ticker(ticker)),
                                                        Err(error) => yield Err(error.context("failed to merge Arcus ticker")),
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    Err(error) => yield Err(anyhow!("failed to parse Arcus markets payload: {}", error)),
                                }
                            }
                            _ => tracing::trace!(
                                "Ignoring Arcus websocket channel: {}",
                                envelope.channel
                            ),
                        }
                    }
                    Ok(Message::Ping(payload)) => {
                        if let Err(error) = ws_stream.send(Message::Pong(payload)).await {
                            yield Err(anyhow!("failed to send Arcus websocket pong: {}", error));
                            break;
                        }
                    }
                    Ok(Message::Pong(_)) => tracing::trace!("Received Arcus websocket pong"),
                    Ok(Message::Close(frame)) => {
                        tracing::info!("Arcus websocket connection closed: {:?}", frame);
                        break;
                    }
                    Ok(_) => tracing::trace!("Ignoring non-text Arcus websocket message"),
                    Err(error) => {
                        yield Err(anyhow!("Arcus websocket error: {}", error));
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
    use rust_decimal::Decimal;

    const ORDERBOOK_SNAPSHOT: &str = r#"{
        "type":"subscribed",
        "channel":"l2Orderbook",
        "id":"BTC-USD",
        "contents":{
            "bids":[["77319.0","0.5"],["77320.0","0.1"]],
            "asks":[["77323.0","0.2"],["77322.0","0.4"]],
            "lastSequenceId":169876552,
            "globalSequenceId":2010816233,
            "timestamp":1789110326848873
        }
    }"#;

    const ORDERBOOK_UPDATE: &str = r#"{
        "type":"channel_data",
        "channel":"l2Orderbook",
        "id":"BTC-USD",
        "contents":{
            "bids":[["77320.0","0.1"]],
            "asks":[["77322.0","0.4"]],
            "lastSequenceId":169876557,
            "globalSequenceId":2010816279,
            "timestamp":1789110327067351
        }
    }"#;

    const BBO_UPDATE: &str = r#"{
        "type":"channel_data",
        "channel":"bbo",
        "id":"BTC-USD",
        "contents":{
            "bestBid":{"price":"77319.2","size":"0.01116913"},
            "bestAsk":{"price":"77319.3","size":"0.08576601"},
            "lastSequenceId":169876552,
            "globalSequenceId":2010816192,
            "timestamp":1789110326739016
        }
    }"#;

    const BTC_MARKET: &str = r#"{
        "marketDisplayName":"BTC-USD",
        "fullAssetName":"Bitcoin",
        "marketId":1,
        "status":"ONLINE",
        "baseAsset":"BTC",
        "quoteAsset":"USD",
        "tickSize":"0.1",
        "stepSize":"0.00000001",
        "tickTiers":[{"upToPrice":"500000","tick":"0.1"},{"tick":"5"}],
        "minOrderNotional":"5",
        "minOrderSize":"0.0001",
        "maxOrderSize":"10000",
        "oraclePrice":"77279.8",
        "markPrice":"77307.5",
        "lastTradePrice":"77291.4",
        "fundingRate":"0.0000125",
        "nextFundingRate":"0.000013",
        "nextFundingAt":1789113600,
        "priceChange24h":"-0.0126",
        "volume24h":"439.88",
        "volume24hNotional":"34528317.11",
        "high24h":"78304.2",
        "low24h":"76438.2",
        "trades24h":77372,
        "openInterest":"73.02279113",
        "openInterestCapNotional":null,
        "initialMarginFraction":"0.025",
        "maintenanceMarginFraction":"0.016667",
        "offHoursInitialMarginFraction":"0.025",
        "regularTradingHours":null,
        "isOutsideRth":false,
        "currentSettlementPrice":null,
        "upperTradingBound":null,
        "lowerTradingBound":null,
        "nextUpperTradingBound":null,
        "nextLowerTradingBound":null,
        "isUpperInExpansionZone":null,
        "isLowerInExpansionZone":null,
        "upperZoneEnteredAt":null,
        "upperExpectedExpansionAt":null,
        "lowerZoneEnteredAt":null,
        "lowerExpectedExpansionAt":null,
        "type":"PERPETUAL",
        "category":"CRYPTO",
        "addedTimestamp":1778786860,
        "assetResolution":"10000000000",
        "pythId":"1"
    }"#;

    #[test]
    fn subscription_serialization_matches_arcus_protocol() {
        let value =
            serde_json::to_value(ArcusWsSubscribe::orderbook("BTC-USD".to_string(), 20)).unwrap();
        assert_eq!(value["type"], "subscribe");
        assert_eq!(value["channel"], "l2Orderbook");
        assert_eq!(value["id"], "BTC-USD");
        assert_eq!(value["nLevels"], 20);
    }

    #[test]
    fn deserializes_orderbook_snapshot_and_update_envelopes() {
        for raw in [ORDERBOOK_SNAPSHOT, ORDERBOOK_UPDATE] {
            let envelope: ArcusWsEnvelope = serde_json::from_str(raw).unwrap();
            let contents: ArcusWsOrderbookContents =
                serde_json::from_value(envelope.contents).unwrap();
            assert!(!contents.bids.is_empty());
            assert!(!contents.asks.is_empty());
            assert!(contents.timestamp.is_some());
        }
    }

    #[test]
    fn orderbook_conversion_sorts_both_sides_and_preserves_micros() {
        let envelope: ArcusWsEnvelope = serde_json::from_str(ORDERBOOK_SNAPSHOT).unwrap();
        let contents: ArcusWsOrderbookContents = serde_json::from_value(envelope.contents).unwrap();
        let orderbook = build_orderbook("BTC-USD", &contents).unwrap();

        assert_eq!(orderbook.symbol, "BTC");
        assert_eq!(orderbook.bids[0].price, Decimal::new(77320, 0));
        assert_eq!(orderbook.asks[0].price, Decimal::new(77322, 0));
        assert!(orderbook.bids[0].price < orderbook.asks[0].price);
        assert_eq!(orderbook.timestamp.timestamp_micros(), 1789110326848873);
    }

    #[test]
    fn bbo_accepts_empty_book_sides() {
        let raw = r#"{
            "bestBid":null,
            "bestAsk":null,
            "lastSequenceId":1,
            "globalSequenceId":2,
            "timestamp":1789110326739016
        }"#;
        let bbo: ArcusWsBboContents = serde_json::from_str(raw).unwrap();
        let snapshot = bbo_snapshot(&bbo);
        assert!(snapshot.bids.is_empty());
        assert!(snapshot.asks.is_empty());
    }

    #[test]
    fn markets_snapshot_builds_complete_ticker_and_funding_rate() {
        let market_value: serde_json::Value = serde_json::from_str(BTC_MARKET).unwrap();
        let raw = serde_json::json!({
            "isSnapshot": true,
            "markets": {"1": market_value}
        });
        let markets: ArcusWsMarketsContents = serde_json::from_value(raw).unwrap();
        let market = markets.markets.get("1").unwrap();

        let envelope: ArcusWsEnvelope = serde_json::from_str(BBO_UPDATE).unwrap();
        let bbo: ArcusWsBboContents = serde_json::from_value(envelope.contents).unwrap();
        let ticker = build_ticker("BTC", &bbo, market).unwrap();

        assert_eq!(ticker.symbol, "BTC");
        assert_eq!(ticker.best_bid_price, Decimal::new(773192, 1));
        assert_eq!(ticker.best_ask_price, Decimal::new(773193, 1));
        assert_eq!(ticker.volume_24h, Decimal::new(43988, 2));
        assert_eq!(ticker.turnover_24h, Decimal::new(3452831711, 2));
        assert_eq!(ticker.open_interest, Decimal::new(7302279113, 8));
        assert!(!ticker.is_empty());

        let funding_rate = build_funding_rate("BTC", market).unwrap();
        assert_eq!(funding_rate.symbol, "BTC");
        assert_eq!(funding_rate.funding_rate, Decimal::new(125, 7));
        assert_eq!(funding_rate.predicted_rate, Decimal::new(13, 6));
        assert_eq!(funding_rate.funding_interval, 1);
        assert_eq!(funding_rate.next_funding_time.timestamp(), 1789113600);
        assert_eq!(funding_rate.funding_time.timestamp(), 1789110000);
    }

    #[tokio::test]
    async fn stream_trades_fails_without_connecting() {
        let error = ArcusWsClient::new()
            .stream_trades(vec!["BTC".to_string()])
            .await
            .err()
            .expect("trade streaming should be unsupported");
        assert!(error.to_string().contains("no side field"));
    }

    #[test]
    fn websocket_symbols_use_rest_mapping() {
        assert_eq!(ArcusWsClient::new().get_name(), "arcus");
        assert_eq!(to_arcus_symbol("btc"), "BTC-USD");
        assert_eq!(to_arcus_symbol("BTC-USD"), "BTC-USD");
        assert_eq!(to_global_symbol("BTC-USD"), "BTC");
    }
}
