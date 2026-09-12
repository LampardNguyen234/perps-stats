use super::types::KucoinContract;
use super::ws_types::*;
use crate::cache::ContractCache;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use futures::{SinkExt, StreamExt};
use perps_core::streaming::*;
use perps_core::types::*;
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::str::FromStr;
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};
use url::Url;

const API_BASE_URL: &str = "https://api-futures.kucoin.com";

/// KuCoin WebSocket streaming client
#[derive(Clone)]
pub struct KuCoinWsClient {
    api_base_url: String,
    /// Shared contract cache for quantity conversion
    contract_cache: ContractCache<KucoinContract>,
}

impl KuCoinWsClient {
    pub fn new() -> Self {
        Self {
            api_base_url: API_BASE_URL.to_string(),
            contract_cache: ContractCache::new(),
        }
    }

    pub fn new_with_cache(contract_cache: ContractCache<KucoinContract>) -> Self {
        Self {
            api_base_url: API_BASE_URL.to_string(),
            contract_cache,
        }
    }

    /// Convert lot-based quantity to real quantity
    /// Formula: real_quantity = lot_quantity × lot_size × multiplier
    fn convert_lot_to_real_quantity(&self, lot_qty: i64, contract: &KucoinContract) -> Decimal {
        let lot_qty_decimal = Decimal::from(lot_qty);
        let lot_size_decimal = Decimal::from(contract.lot_size);
        let multiplier_decimal = Decimal::from_f64(contract.multiplier).unwrap_or(Decimal::ONE);

        lot_qty_decimal * lot_size_decimal * multiplier_decimal
    }

    /// Get WebSocket token and endpoint
    fn parse_level2(
        &self,
        response: KuCoinWsResponse,
        contracts: &HashMap<String, KucoinContract>,
    ) -> Result<Option<DepthUpdate>> {
        let Some(symbol) = response
            .topic
            .as_deref()
            .and_then(|topic| topic.strip_prefix("/contractMarket/level2:"))
        else {
            return Ok(None);
        };
        let Some(contract) = contracts.get(symbol) else {
            return Ok(None);
        };
        let update: KuCoinWsLevel2 = serde_json::from_value(
            response
                .data
                .ok_or_else(|| anyhow!("missing KuCoin level2 data"))?,
        )?;
        let parts: Vec<_> = update.change.split(',').collect();
        if parts.len() != 3 {
            return Err(anyhow!("invalid KuCoin level2 change"));
        }
        let level = OrderbookLevel {
            price: Decimal::from_str(parts[0])?,
            quantity: self.convert_lot_to_real_quantity(parts[2].parse()?, contract),
        };
        let (bids, asks) = match parts[1] {
            "buy" => (vec![level], vec![]),
            "sell" => (vec![], vec![level]),
            side => return Err(anyhow!("invalid KuCoin level2 side {side}")),
        };
        let sequence = u64::try_from(update.sequence)?;
        Ok(Some(DepthUpdate {
            symbol: symbol.to_string(),
            first_update_id: sequence,
            final_update_id: sequence,
            previous_id: 0,
            bids,
            asks,
            is_snapshot: false,
        }))
    }

    async fn get_ws_token(&self) -> Result<(String, String)> {
        let url = format!("{}/api/v1/bullet-public", self.api_base_url);
        let response = reqwest::Client::new().post(&url).send().await?;

        let token_response: KuCoinWsTokenResponse = response.json().await?;

        if token_response.code != "200000" {
            return Err(anyhow!(
                "Failed to get WebSocket token: {}",
                token_response.code
            ));
        }

        let server = token_response
            .data
            .instance_servers
            .first()
            .ok_or_else(|| anyhow!("No WebSocket server available"))?;

        Ok((token_response.data.token, server.endpoint.clone()))
    }

    /// Connect to WebSocket and return the stream
    async fn connect(&self) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>> {
        let (token, endpoint) = self.get_ws_token().await?;
        let ws_url = format!("{}?token={}", endpoint, token);

        tracing::info!("Connecting to KuCoin WebSocket: {}", endpoint);
        let url = Url::parse(&ws_url)?;
        let (ws_stream, response) = connect_async(url.as_str()).await?;
        tracing::info!(
            "Connected to KuCoin WebSocket (status: {:?})",
            response.status()
        );
        Ok(ws_stream)
    }

    /// Subscribe to a topic
    async fn subscribe(
        &self,
        ws_stream: &mut WebSocketStream<MaybeTlsStream<TcpStream>>,
        topic: String,
        id: String,
    ) -> Result<()> {
        let request = KuCoinWsSubscribeRequest {
            id,
            msg_type: "subscribe".to_string(),
            topic,
            private_channel: false,
            response: true,
        };
        let sub_message = serde_json::to_string(&request)?;
        tracing::debug!("Subscribing: {}", sub_message);
        ws_stream.send(Message::Text(sub_message)).await?;
        Ok(())
    }

    /// Convert KuCoin ticker to our Ticker type
    fn convert_ticker(&self, ws_ticker: &KuCoinWsTicker) -> Result<Ticker> {
        Ok(Ticker {
            symbol: ws_ticker.symbol.clone(),
            last_price: ws_ticker
                .last_traded_price
                .as_ref()
                .map(|p| Decimal::from_str(p))
                .transpose()?
                .unwrap_or(Decimal::ZERO),
            mark_price: ws_ticker
                .mark_price
                .as_ref()
                .map(|p| Decimal::from_str(p))
                .transpose()?
                .unwrap_or(Decimal::ZERO),
            index_price: ws_ticker
                .index_price
                .as_ref()
                .map(|p| Decimal::from_str(p))
                .transpose()?
                .unwrap_or(Decimal::ZERO),
            best_bid_price: ws_ticker
                .best_bid_price
                .as_ref()
                .map(|p| Decimal::from_str(p))
                .transpose()?
                .unwrap_or(Decimal::ZERO),
            best_bid_qty: ws_ticker
                .best_bid_size
                .map(Decimal::from)
                .unwrap_or(Decimal::ZERO),
            best_ask_price: ws_ticker
                .best_ask_price
                .as_ref()
                .map(|p| Decimal::from_str(p))
                .transpose()?
                .unwrap_or(Decimal::ZERO),
            best_ask_qty: ws_ticker
                .best_ask_size
                .map(Decimal::from)
                .unwrap_or(Decimal::ZERO),
            volume_24h: Decimal::ZERO, // Not available in ticker stream
            turnover_24h: Decimal::ZERO,
            open_interest: Decimal::ZERO,
            open_interest_notional: Decimal::ZERO,
            price_change_24h: Decimal::ZERO,
            price_change_pct: Decimal::ZERO,
            high_price_24h: Decimal::ZERO,
            low_price_24h: Decimal::ZERO,
            timestamp: Utc
                .timestamp_millis_opt(ws_ticker.timestamp)
                .single()
                .unwrap_or_else(Utc::now),
        })
    }

    /// Convert KuCoin execution to our Trade type
    fn convert_trade(&self, ws_execution: &KuCoinWsExecution) -> Result<Trade> {
        Ok(Trade {
            id: ws_execution.trade_id.clone(),
            symbol: ws_execution.symbol.clone(),
            price: Decimal::from_str(&ws_execution.price)?,
            quantity: Decimal::from(ws_execution.size),
            side: match ws_execution.side.as_str() {
                "buy" => OrderSide::Buy,
                "sell" => OrderSide::Sell,
                _ => OrderSide::Buy,
            },
            timestamp: {
                // KuCoin timestamps are in nanoseconds
                let seconds = ws_execution.timestamp / 1_000_000_000;
                let nanos = (ws_execution.timestamp % 1_000_000_000) as u32;
                Utc.timestamp_opt(seconds, nanos)
                    .single()
                    .unwrap_or_else(Utc::now)
            },
        })
    }

    /// Convert interval format from standard to KuCoin format
    /// Standard: 1m, 5m, 15m, 30m, 1h, 4h, 1d, 1w
    /// KuCoin: 1min, 5min, 15min, 30min, 1hour, 4hour, 1day, 1week
    pub fn convert_interval(&self, interval: &str) -> String {
        match interval {
            "1m" => "1min".to_string(),
            "3m" => "3min".to_string(),
            "5m" => "5min".to_string(),
            "15m" => "15min".to_string(),
            "30m" => "30min".to_string(),
            "1h" => "1hour".to_string(),
            "2h" => "2hour".to_string(),
            "4h" => "4hour".to_string(),
            "8h" => "8hour".to_string(),
            "12h" => "12hour".to_string(),
            "1d" => "1day".to_string(),
            "1w" => "1week".to_string(),
            _ => interval.to_string(), // Pass through if unknown
        }
    }

    /// Convert KuCoin kline to our Kline type
    fn convert_kline(&self, ws_kline: &KuCoinWsKline, interval: &str) -> Result<Kline> {
        use perps_core::utils::normalize_interval;

        // Candles format: [start_time, open, close, high, low, volume, amount]
        if ws_kline.candles.len() < 7 {
            return Err(anyhow!(
                "Invalid kline data: expected 7 fields, got {}",
                ws_kline.candles.len()
            ));
        }

        let start_time_secs = i64::from_str(&ws_kline.candles[0])?;
        let open_time = Utc
            .timestamp_opt(start_time_secs, 0)
            .single()
            .ok_or_else(|| anyhow!("Invalid timestamp"))?;

        // Calculate close_time based on interval
        let close_time = match interval {
            "1min" => open_time + chrono::Duration::minutes(1),
            "3min" => open_time + chrono::Duration::minutes(3),
            "5min" => open_time + chrono::Duration::minutes(5),
            "15min" => open_time + chrono::Duration::minutes(15),
            "30min" => open_time + chrono::Duration::minutes(30),
            "1hour" => open_time + chrono::Duration::hours(1),
            "2hour" => open_time + chrono::Duration::hours(2),
            "4hour" => open_time + chrono::Duration::hours(4),
            "8hour" => open_time + chrono::Duration::hours(8),
            "12hour" => open_time + chrono::Duration::hours(12),
            "1day" => open_time + chrono::Duration::days(1),
            "1week" => open_time + chrono::Duration::weeks(1),
            _ => open_time + chrono::Duration::hours(1), // Default to 1 hour
        };

        // Normalize interval to standard format (1hour -> 1h, 1min -> 1m, etc.)
        let normalized_interval = normalize_interval(interval);

        Ok(Kline {
            symbol: ws_kline.symbol.clone(),
            interval: normalized_interval,
            open_time,
            close_time,
            open: Decimal::from_str(&ws_kline.candles[1])?,
            high: Decimal::from_str(&ws_kline.candles[3])?,
            low: Decimal::from_str(&ws_kline.candles[4])?,
            close: Decimal::from_str(&ws_kline.candles[2])?,
            volume: Decimal::from_str(&ws_kline.candles[5])?,
            turnover: Decimal::from_str(&ws_kline.candles[6])?,
        })
    }

    /// Stream klines for a single symbol (one-time fetch for REST API compatibility)
    /// This is a helper method for the REST API to fetch klines via WebSocket
    pub async fn stream_klines_single(
        &self,
        symbol: String,
        interval: String,
        _limit: u32,
    ) -> Result<DataStream<Kline>> {
        let mut ws_stream = self.connect().await?;

        // Subscribe to klines topic
        let topic = format!("/contractMarket/limitCandle:{}_{}", symbol, interval);
        self.subscribe(&mut ws_stream, topic.clone(), "klines-single".to_string())
            .await?;

        let client = self.clone();
        let interval_clone = interval.clone();

        let stream = async_stream::stream! {
            let mut received_count = 0;
            let timeout_duration = std::time::Duration::from_secs(10); // 10 second timeout
            let start_time = std::time::Instant::now();

            while let Some(msg) = ws_stream.next().await {
                // Check timeout
                if start_time.elapsed() > timeout_duration {
                    tracing::warn!("Klines WebSocket fetch timed out after 10 seconds");
                    break;
                }

                match msg {
                    Ok(Message::Text(text)) => {
                        if let Ok(response) = serde_json::from_str::<KuCoinWsResponse>(&text) {
                            match response.msg_type.as_str() {
                                "welcome" => {
                                    tracing::debug!("Received welcome message");
                                }
                                "ack" => {
                                    tracing::debug!("Klines subscription acknowledged");
                                }
                                "message" => {
                                    if let Some(resp_topic) = &response.topic {
                                        if resp_topic.starts_with("/contractMarket/limitCandle:") {
                                            if let Some(data) = response.data {
                                                if let Ok(kline_data) = serde_json::from_value::<KuCoinWsKline>(data) {
                                                    match client.convert_kline(&kline_data, &interval_clone) {
                                                        Ok(kline) => {
                                                            yield Ok(kline);
                                                            received_count += 1;
                                                            // For single fetch, we only need one kline update
                                                            if received_count >= 1 {
                                                                break;
                                                            }
                                                        }
                                                        Err(e) => {
                                                            tracing::error!("Failed to convert kline: {}", e);
                                                            yield Err(anyhow!("Failed to convert kline: {}", e));
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    Ok(Message::Ping(payload)) => {
                        if let Err(e) = ws_stream.send(Message::Pong(payload)).await {
                            tracing::error!("Failed to send pong: {}", e);
                            yield Err(anyhow!("Failed to send pong: {}", e));
                            break;
                        }
                    }
                    Ok(Message::Close(frame)) => {
                        tracing::info!("WebSocket connection closed: {:?}", frame);
                        break;
                    }
                    Err(e) => {
                        yield Err(anyhow!("WebSocket error: {}", e));
                        break;
                    }
                    _ => {}
                }
            }
        };

        Ok(Box::pin(stream))
    }
}

impl Default for KuCoinWsClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IPerpsStream for KuCoinWsClient {
    fn get_name(&self) -> &str {
        "kucoin"
    }

    async fn stream_tickers(&self, symbols: Vec<String>) -> Result<DataStream<Ticker>> {
        let mut ws_stream = self.connect().await?;

        // Subscribe to tickers for each symbol
        for (i, symbol) in symbols.iter().enumerate() {
            let topic = format!("/contractMarket/ticker:{}", symbol);
            self.subscribe(&mut ws_stream, topic, format!("ticker-{}", i))
                .await?;
        }

        let client = self.clone();

        let stream = async_stream::stream! {
            while let Some(msg) = ws_stream.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        tracing::debug!("Received message: {}", &text[..text.len().min(200)]);

                        if let Ok(response) = serde_json::from_str::<KuCoinWsResponse>(&text) {
                            match response.msg_type.as_str() {
                                "welcome" => {
                                    tracing::debug!("Received welcome message");
                                }
                                "ack" => {
                                    tracing::debug!("Subscription acknowledged");
                                }
                                "message" => {
                                    if let Some(topic) = &response.topic {
                                        if topic.starts_with("/contractMarket/ticker:") {
                                            if let Some(data) = response.data {
                                                if let Ok(ticker) = serde_json::from_value::<KuCoinWsTicker>(data) {
                                                    match client.convert_ticker(&ticker) {
                                                        Ok(ticker) => yield Ok(ticker),
                                                        Err(e) => yield Err(anyhow!("Failed to convert ticker: {}", e)),
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    Ok(Message::Ping(payload)) => {
                        if let Err(e) = ws_stream.send(Message::Pong(payload)).await {
                            tracing::error!("Failed to send pong: {}", e);
                            yield Err(anyhow!("Failed to send pong: {}", e));
                            break;
                        }
                    }
                    Ok(Message::Close(frame)) => {
                        tracing::info!("WebSocket connection closed: {:?}", frame);
                        break;
                    }
                    Err(e) => {
                        yield Err(anyhow!("WebSocket error: {}", e));
                        break;
                    }
                    _ => {}
                }
            }
        };

        Ok(Box::pin(stream))
    }

    async fn stream_trades(&self, symbols: Vec<String>) -> Result<DataStream<Trade>> {
        let mut ws_stream = self.connect().await?;

        // Subscribe to executions for each symbol
        for (i, symbol) in symbols.iter().enumerate() {
            let topic = format!("/contractMarket/execution:{}", symbol);
            self.subscribe(&mut ws_stream, topic, format!("execution-{}", i))
                .await?;
        }

        let client = self.clone();

        let stream = async_stream::stream! {
            while let Some(msg) = ws_stream.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        if let Ok(response) = serde_json::from_str::<KuCoinWsResponse>(&text) {
                            if response.msg_type == "message" {
                                if let Some(topic) = &response.topic {
                                    if topic.starts_with("/contractMarket/execution:") {
                                        if let Some(data) = response.data {
                                            if let Ok(execution) = serde_json::from_value::<KuCoinWsExecution>(data) {
                                                match client.convert_trade(&execution) {
                                                    Ok(trade) => yield Ok(trade),
                                                    Err(e) => yield Err(anyhow!("Failed to convert trade: {}", e)),
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Ok(Message::Ping(payload)) => {
                        if let Err(e) = ws_stream.send(Message::Pong(payload)).await {
                            yield Err(anyhow!("Failed to send pong: {}", e));
                            break;
                        }
                    }
                    Ok(Message::Close(frame)) => {
                        tracing::info!("WebSocket connection closed: {:?}", frame);
                        break;
                    }
                    Err(e) => {
                        yield Err(anyhow!("WebSocket error: {}", e));
                        break;
                    }
                    _ => {}
                }
            }
        };

        Ok(Box::pin(stream))
    }

    async fn stream_orderbooks(&self, _symbols: Vec<String>) -> Result<DataStream<Orderbook>> {
        // KuCoin level2 orderbook requires incremental updates which is complex to implement
        // For now, we'll return an error
        Err(anyhow!("Orderbook streaming not yet implemented for KuCoin - requires incremental update handling"))
    }

    async fn stream_multi(&self, config: StreamConfig) -> Result<DataStream<StreamEvent>> {
        let mut ws_stream = self.connect().await?;

        // Get kline interval if streaming klines
        let kline_interval = if config.data_types.contains(&StreamDataType::Kline) {
            let interval = config
                .kline_interval
                .as_ref()
                .ok_or_else(|| anyhow!("kline_interval must be specified when streaming klines"))?;
            Some(self.convert_interval(interval))
        } else {
            None
        };

        // Subscribe to requested topics
        let mut topic_id = 0;
        for symbol in &config.symbols {
            for data_type in &config.data_types {
                let topic = match data_type {
                    StreamDataType::Ticker => format!("/contractMarket/ticker:{}", symbol),
                    StreamDataType::Trade => format!("/contractMarket/execution:{}", symbol),
                    StreamDataType::Orderbook => continue, // Not yet implemented
                    StreamDataType::FundingRate => continue, // Not available via WebSocket
                    StreamDataType::Kline => {
                        if let Some(ref interval) = kline_interval {
                            format!("/contractMarket/limitCandle:{}_{}", symbol, interval)
                        } else {
                            continue;
                        }
                    }
                };
                self.subscribe(&mut ws_stream, topic, format!("multi-{}", topic_id))
                    .await?;
                topic_id += 1;
            }
        }

        let client = self.clone();

        let stream = async_stream::stream! {
            while let Some(msg) = ws_stream.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        if let Ok(response) = serde_json::from_str::<KuCoinWsResponse>(&text) {
                            if response.msg_type == "message" {
                                if let Some(topic) = &response.topic {
                                    if let Some(data) = response.data {
                                        if topic.starts_with("/contractMarket/ticker:") {
                                            if let Ok(ticker) = serde_json::from_value::<KuCoinWsTicker>(data) {
                                                if let Ok(ticker) = client.convert_ticker(&ticker) {
                                                    yield Ok(StreamEvent::Ticker(ticker));
                                                }
                                            }
                                        } else if topic.starts_with("/contractMarket/execution:") {
                                            if let Ok(execution) = serde_json::from_value::<KuCoinWsExecution>(data) {
                                                if let Ok(trade) = client.convert_trade(&execution) {
                                                    yield Ok(StreamEvent::Trade(trade));
                                                }
                                            }
                                        } else if topic.starts_with("/contractMarket/limitCandle:") {
                                            if let Ok(kline_data) = serde_json::from_value::<KuCoinWsKline>(data) {
                                                // Extract interval from topic: /contractMarket/limitCandle:XBTUSDTM_1hour
                                                if let Some(interval_part) = topic.split('_').nth(1) {
                                                    if let Ok(kline) = client.convert_kline(&kline_data, interval_part) {
                                                        yield Ok(StreamEvent::Kline(kline));
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Ok(Message::Ping(payload)) => {
                        if let Err(e) = ws_stream.send(Message::Pong(payload)).await {
                            yield Err(anyhow!("Failed to send pong: {}", e));
                            break;
                        }
                    }
                    Ok(Message::Close(frame)) => {
                        tracing::info!("WebSocket connection closed: {:?}", frame);
                        break;
                    }
                    Err(e) => {
                        yield Err(anyhow!("WebSocket error: {}", e));
                        break;
                    }
                    _ => {}
                }
            }
        };

        Ok(Box::pin(stream))
    }
}

// ============================================================================
// OrderbookStreamer - Optimized streaming interface for orderbook management
// ============================================================================

/// Implementation of OrderbookStreamer trait for KuCoin
///
/// KuCoin Level 2 Depth Streaming:
/// - Uses `/contractMarket/level2Depth50:{symbol}` topic
/// - Multiplexes level2 topics and routes updates to each contract
/// - Updates carry absolute level quantities and per-symbol sequence numbers
/// - REST snapshots initialize the delta reconstruction engine
#[async_trait]
impl OrderbookStreamer for KuCoinWsClient {
    async fn stream_depth_updates(&self, symbols: Vec<String>) -> Result<DepthUpdateStream> {
        let mut contracts = HashMap::new();
        for symbol in &symbols {
            let contract = self
                .contract_cache
                .get(symbol)
                .await
                .ok_or_else(|| anyhow!("Contract info required for symbol: {symbol}"))?;
            contracts.insert(symbol.clone(), contract);
        }
        let mut ws_stream = self.connect().await?;
        for symbol in &symbols {
            self.subscribe(
                &mut ws_stream,
                format!("/contractMarket/level2:{symbol}"),
                format!("depth-{symbol}"),
            )
            .await?;
        }

        let client_clone = self.clone();
        let stream = async_stream::stream! {
            // Set up ping interval (30 seconds as per requirement)
            let mut ping_interval = tokio::time::interval(std::time::Duration::from_secs(30));
            ping_interval.tick().await; // Skip first immediate tick
            let mut ping_counter: u64 = 0;

            loop {
                tokio::select! {
                    // Handle incoming messages
                    msg = ws_stream.next() => {
                        match msg {
                            Some(Ok(Message::Text(text))) => {
                                if let Ok(response) = serde_json::from_str::<KuCoinWsResponse>(&text) {
                                    match response.msg_type.as_str() {
                                        "message" => {
                                            match client_clone.parse_level2(response, &contracts) {
                                                Ok(Some(update)) => yield Ok(update),
                                                Ok(None) => {},
                                                Err(error) => { yield Err(error); break; }
                                            }
                                        }
                                        "welcome" => {
                                            tracing::debug!("[KuCoin] WebSocket connection established");
                                        }
                                        "ack" => {
                                            tracing::debug!("[KuCoin] Subscription acknowledged");
                                        }
                                        "pong" => {
                                            tracing::trace!("[KuCoin] Pong received");
                                        }
                                        _ => {
                                            tracing::trace!("[KuCoin] Unknown message type: {}", response.msg_type);
                                        }
                                    }
                                }
                            }
                            Some(Ok(Message::Ping(payload))) => {
                                tracing::trace!("[KuCoin] Ping received from server, sending pong");
                                if let Err(e) = ws_stream.send(Message::Pong(payload)).await {
                                    tracing::error!("[KuCoin] Failed to send pong: {}", e);
                                    yield Err(anyhow!("Failed to send pong: {}", e));
                                    break;
                                }
                            }
                            Some(Ok(Message::Close(frame))) => {
                                tracing::info!("[KuCoin] WebSocket closed: {:?}", frame);
                                break;
                            }
                            Some(Err(e)) => {
                                tracing::error!("[KuCoin] WebSocket error: {}", e);
                                yield Err(anyhow!("WebSocket error: {}", e));
                                break;
                            }
                            None => {
                                tracing::info!("[KuCoin] WebSocket stream ended");
                                break;
                            }
                            _ => {}
                        }
                    }

                    // Send periodic ping messages (every 30 seconds)
                    _ = ping_interval.tick() => {
                        ping_counter += 1;
                        let ping_id = format!("ping-{ping_counter}");
                        let ping_msg = KuCoinWsPingRequest {
                            id: ping_id.clone(),
                            msg_type: "ping".to_string(),
                        };

                        match serde_json::to_string(&ping_msg) {
                            Ok(ping_json) => {
                                tracing::trace!(%ping_id, "KuCoin sending ping");
                                if let Err(e) = ws_stream.send(Message::Text(ping_json)).await {
                                    tracing::error!("[KuCoin] Failed to send ping: {}", e);
                                    yield Err(anyhow!("Failed to send ping: {}", e));
                                    break;
                                }
                            }
                            Err(e) => {
                                tracing::error!("[KuCoin] Failed to serialize ping message: {}", e);
                                yield Err(anyhow!("Failed to serialize ping: {}", e));
                                break;
                            }
                        }
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }

    fn is_incremental_delta(&self) -> bool {
        false // KuCoin sends full snapshots, not incremental deltas
    }

    fn exchange_name(&self) -> &str {
        "kucoin"
    }

    fn ws_base_url(&self) -> &str {
        // KuCoin uses dynamic WebSocket URLs from token endpoint
        "https://api-futures.kucoin.com"
    }

    fn connection_config(&self) -> ConnectionConfig {
        ConnectionConfig {
            ping_interval: std::time::Duration::from_secs(20),
            pong_timeout: std::time::Duration::from_secs(10),
            reconnect_delay: std::time::Duration::from_secs(2),
            max_reconnect_attempts: 10,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn contract(symbol: &str, multiplier: f64) -> KucoinContract {
        serde_json::from_value(serde_json::json!({
            "symbol": symbol, "baseCurrency": "BTC", "quoteCurrency": "USDT",
            "settleCurrency": "USDT", "tickSize": 0.1, "lotSize": 1,
            "maxOrderQty": 10000, "maxLeverage": 100, "initialMargin": 0.01,
            "multiplier": multiplier, "maintainMargin": 0.005,
            "makerFeeRate": 0.0002, "takerFeeRate": 0.0006, "status": "Open"
        }))
        .unwrap()
    }
    fn response(symbol: &str, sequence: i64, change: &str) -> KuCoinWsResponse {
        serde_json::from_value(serde_json::json!({
            "type": "message", "topic": format!("/contractMarket/level2:{symbol}"),
            "data": {"sequence": sequence, "change": change, "timestamp": 0}
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn interleaved_topics_use_their_own_contract_and_book() {
        let client = KuCoinWsClient::new();
        let contracts: HashMap<String, KucoinContract> = HashMap::from([
            ("XBTUSDTM".into(), contract("XBTUSDTM", 0.001)),
            ("ETHUSDTM".into(), contract("ETHUSDTM", 0.01)),
        ]);
        let manager = perps_core::OrderbookManager::new(
            "kucoin".into(),
            perps_core::OrderbookManagerConfig::for_kucoin(),
        );
        for symbol in contracts.keys() {
            manager.initialize_empty_orderbook(symbol.clone()).await;
            manager
                .apply_snapshot(symbol, vec![], vec![], 1)
                .await
                .unwrap();
        }
        for message in [
            response("XBTUSDTM", 2, "100,buy,2"),
            response("ETHUSDTM", 2, "200,buy,3"),
            response("XBTUSDTM", 3, "101,sell,4"),
            response("ETHUSDTM", 3, "201,sell,5"),
        ] {
            let update = client.parse_level2(message, &contracts).unwrap().unwrap();
            manager
                .apply_update(
                    &update.symbol,
                    update.first_update_id,
                    update.final_update_id,
                    update.previous_id,
                    update.bids,
                    update.asks,
                    false,
                )
                .await
                .unwrap();
        }
        let btc = manager.get_orderbook("XBTUSDTM", 0).await.unwrap();
        let eth = manager.get_orderbook("ETHUSDTM", 0).await.unwrap();
        assert_eq!(btc.bids[0].quantity, dec!(0.002));
        assert_eq!(btc.asks[0].quantity, dec!(0.004));
        assert_eq!(eth.bids[0].quantity, dec!(0.03));
        assert_eq!(eth.asks[0].quantity, dec!(0.05));
        assert_eq!(btc.bids[0].price, dec!(100));
        assert_eq!(eth.bids[0].price, dec!(200));
        assert!(client
            .parse_level2(response("UNKNOWN", 1, "100,buy,1"), &contracts)
            .unwrap()
            .is_none());
        assert!(client
            .parse_level2(response("XBTUSDTM", 4, "bad,buy,1"), &contracts)
            .is_err());
    }
}
